//! Bounded queue coalescing, source ordering and durable admission ownership.
use super::*;
use futures_util::{FutureExt, SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

struct BatchFixture {
    sender: mpsc::Sender<irc::QueuedEvent>,
    receiver: mpsc::Receiver<irc::QueuedEvent>,
    deferred: Option<irc::QueuedEvent>,
    flush: tokio::time::Interval,
    pending: Vec<NewObservation>,
    permits: Vec<OwnedSemaphorePermit>,
    rooms: BTreeMap<String, RoomStatus>,
    budget: Arc<Semaphore>,
}

fn receipt(index: usize) -> NewObservation {
    NewObservation {
        channel: if index % 2 == 0 { "alpha" } else { "beta" }.into(),
        account: Some(format!("account{index}")),
        kind: if index % 3 == 0 { "part" } else { "join" }.into(),
        received_at: format!("2026-01-01T00:{:02}:{:02}.000Z", index / 60, index % 60),
    }
}

impl BatchFixture {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel(4096);
        let mut flush = tokio::time::interval(Duration::from_millis(250));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let rooms = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    RoomStatus {
                        channel: name.into(),
                        state: "collecting".into(),
                        last_positive_at: None,
                        last_error: None,
                        coverage: "unknown",
                    },
                )
            })
            .collect();
        Self {
            sender,
            receiver,
            deferred: None,
            flush,
            pending: Vec::new(),
            permits: Vec::new(),
            rooms,
            budget: Arc::new(Semaphore::new(irc::RECEIVE_BUFFER_UNITS)),
        }
    }

    fn envelope(&self, event: StreamEvent, received_at: &str) -> irc::QueuedEvent {
        let weight = match &event {
            StreamEvent::Observations(rows) => {
                assert!(rows.len() <= 256, "match the transport payload bound");
                rows.len().max(1)
            }
            _ => 1,
        };
        let permit = self
            .budget
            .clone()
            .try_acquire_many_owned(weight as u32)
            .unwrap();
        irc::QueuedEvent::for_test(event, received_at.into(), permit)
    }

    fn observations(&self, start: usize, count: usize) -> irc::QueuedEvent {
        let rows = (start..start + count).map(receipt).collect();
        self.envelope(StreamEvent::Observations(rows), &receipt(start).received_at)
    }

    fn admit(&mut self, envelope: irc::QueuedEvent) -> bool {
        let (StreamEvent::Observations(events), permit) = envelope.into_parts() else {
            panic!("fixture admission expects observation payloads");
        };
        queue_observation_event(
            &mut self.flush,
            &mut self.pending,
            &mut self.permits,
            &mut self.rooms,
            events,
            permit,
        )
    }

    fn ready(&mut self) {
        assert!(!self.admit(self.observations(0, 256)));
        assert!(self.admit(self.observations(256, 256)));
    }

    fn coalesce(&mut self) {
        coalesce_queued_observations(
            &mut self.receiver,
            &mut self.deferred,
            &mut self.flush,
            &mut self.pending,
            &mut self.permits,
            &mut self.rooms,
        );
    }
}

#[tokio::test(start_paused = true)]
async fn backlog_coalesces_to_1024_without_waiting_or_consuming_the_next_payload() {
    let mut batch = BatchFixture::new();
    batch.ready();
    for start in [512, 768, 1024] {
        batch
            .sender
            .try_send(batch.observations(start, 256))
            .unwrap();
    }
    let started = tokio::time::Instant::now();
    batch.coalesce();
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(batch.pending.len(), 1024);
    assert_eq!(batch.receiver.len(), 1);
    assert!(batch.deferred.is_none());
    for (index, row) in batch.pending.iter().enumerate() {
        let expected = receipt(index);
        assert_eq!(row.account, expected.account);
        assert_eq!(row.received_at, expected.received_at);
    }
    for name in ["alpha", "beta"] {
        let expected = (0..1024)
            .map(receipt)
            .filter(|row| row.channel == name && row.kind == "join")
            .next_back()
            .unwrap();
        assert_eq!(
            batch.rooms[name].last_positive_at.as_deref(),
            Some(expected.received_at.as_str())
        );
    }
    assert_eq!(
        batch.budget.available_permits(),
        irc::RECEIVE_BUFFER_UNITS - 1280
    );
}

#[tokio::test(start_paused = true)]
async fn empty_queue_keeps_the_512_trigger_immediate() {
    let mut batch = BatchFixture::new();
    batch.ready();
    let started = tokio::time::Instant::now();
    batch.coalesce();
    assert_eq!(started.elapsed(), Duration::ZERO);
    assert_eq!(batch.pending.len(), 512);
    assert!(batch.deferred.is_none());
    assert!(batch.flush.tick().now_or_never().is_none());
}

#[tokio::test]
async fn nonfitting_payload_remains_intact_and_precedes_a_closed_queues_tail() {
    let mut batch = BatchFixture::new();
    batch.ready();
    for (start, count) in [(512, 256), (768, 255), (1023, 2), (1025, 1)] {
        batch
            .sender
            .try_send(batch.observations(start, count))
            .unwrap();
    }
    batch.receiver.close();
    batch.coalesce();
    assert_eq!(batch.pending.len(), 1023);
    assert_eq!(batch.receiver.len(), 1);
    assert_eq!(
        batch.budget.available_permits(),
        irc::RECEIVE_BUFFER_UNITS - 1026
    );
    let deferred = receive_event(&mut batch.receiver, &mut batch.deferred)
        .await
        .unwrap();
    assert_eq!(deferred.observation_len(), Some(2));
    assert_eq!(deferred.received_at(), receipt(1023).received_at);
    let (StreamEvent::Observations(rows), permit) = deferred.into_parts() else {
        panic!("nonfitting observation envelope was preserved");
    };
    assert_eq!(rows[0].account, receipt(1023).account);
    assert_eq!(rows[1].account, receipt(1024).account);
    assert_eq!(permit.num_permits(), 2);
    let tail = receive_event(&mut batch.receiver, &mut batch.deferred)
        .await
        .unwrap();
    assert_eq!(tail.received_at(), receipt(1025).received_at);
    assert!(receive_event(&mut batch.receiver, &mut batch.deferred)
        .await
        .is_none());
}

#[tokio::test]
async fn every_control_defers_with_its_source_clock_and_admission_permit() {
    for event in [
        StreamEvent::Connected,
        StreamEvent::RoomJoining("alpha".into()),
        StreamEvent::RoomReady("alpha".into()),
        StreamEvent::RoomUnavailable("alpha".into()),
        StreamEvent::RoomLeft("alpha".into()),
    ] {
        let mut batch = BatchFixture::new();
        batch.ready();
        let expected = format!("{event:?}");
        let cutoff = "2026-01-01T00:09:00.123Z";
        batch
            .sender
            .try_send(batch.envelope(event, cutoff))
            .unwrap();
        batch.sender.try_send(batch.observations(512, 1)).unwrap();
        batch.receiver.close();
        batch.coalesce();
        assert_eq!(batch.pending.len(), 512);
        assert_eq!(batch.receiver.len(), 1);
        let envelope = receive_event(&mut batch.receiver, &mut batch.deferred)
            .await
            .unwrap();
        assert_eq!(envelope.received_at(), cutoff);
        assert_eq!(envelope.observation_len(), None);
        let (actual, permit) = envelope.into_parts();
        assert_eq!(format!("{actual:?}"), expected);
        assert_eq!(permit.num_permits(), 1);
        assert_eq!(
            batch.budget.available_permits(),
            irc::RECEIVE_BUFFER_UNITS - 514
        );
        drop(permit);
        assert_eq!(
            batch.budget.available_permits(),
            irc::RECEIVE_BUFFER_UNITS - 513
        );
        let next = receive_event(&mut batch.receiver, &mut batch.deferred)
            .await
            .unwrap();
        assert_eq!(next.received_at(), receipt(512).received_at);
        assert!(receive_event(&mut batch.receiver, &mut batch.deferred)
            .await
            .is_none());
    }
}

#[tokio::test]
async fn another_ready_select_branch_cannot_take_the_deferred_envelope() {
    let mut batch = BatchFixture::new();
    batch.deferred = Some(batch.observations(512, 1));
    tokio::select! {
        biased;
        _ = std::future::ready(()) => {},
        _ = receive_event(&mut batch.receiver, &mut batch.deferred) => panic!("the first branch wins"),
    }
    assert!(batch.deferred.is_some());
    assert_eq!(
        batch.budget.available_permits(),
        irc::RECEIVE_BUFFER_UNITS - 1
    );
    let envelope = receive_event(&mut batch.receiver, &mut batch.deferred)
        .await
        .unwrap();
    assert_eq!(envelope.received_at(), receipt(512).received_at);
}

#[tokio::test]
async fn empty_payloads_cannot_extend_the_coalescing_visit_cap() {
    let mut batch = BatchFixture::new();
    batch.ready();
    for _ in 0..1500 {
        batch.sender.try_send(batch.observations(512, 0)).unwrap();
    }
    batch.coalesce();
    assert_eq!(batch.pending.len(), 512);
    assert_eq!(batch.receiver.len(), 1500 - 1024);
    assert_eq!(batch.permits.len(), 2 + 1024);
    assert_eq!(
        batch.budget.available_permits(),
        irc::RECEIVE_BUFFER_UNITS - 512 - 1500
    );
}

async fn store_fixture() -> (tempfile::TempDir, Store, BTreeMap<String, i64>) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("coalescing.sqlite3"))
        .await
        .unwrap();
    store
        .track(vec!["alpha".into(), "beta".into()], true)
        .await
        .unwrap();
    let mut sessions = BTreeMap::new();
    for name in ["alpha", "beta"] {
        let id = store.begin_session(vec![name.into()]).await.unwrap();
        sessions.insert(name.into(), id);
    }
    store.reset_work_for_test();
    (directory, store, sessions)
}

#[tokio::test]
async fn coalesced_receipts_stay_charged_through_a_blocked_append_and_commit_once() {
    let (_directory, store, sessions) = store_fixture().await;
    let mut batch = BatchFixture::new();
    batch.ready();
    for start in [512, 768] {
        batch
            .sender
            .try_send(batch.observations(start, 256))
            .unwrap();
    }
    batch.coalesce();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let held_store = store.clone();
    let hold = tokio::spawn(async move {
        held_store
            .call_write(crate::diagnostics::Operation::Initialize, move |_| {
                let _ = entered_tx.send(());
                release_rx
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|_| crate::store::StoreError::Unavailable)?;
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    let mut saving = Box::pin(flush_pending(
        &store,
        &sessions,
        &mut batch.pending,
        &mut batch.permits,
    ));
    assert!(saving.as_mut().now_or_never().is_none());
    assert_eq!(
        batch.budget.available_permits(),
        irc::RECEIVE_BUFFER_UNITS - 1024
    );
    release_tx.send(()).unwrap();
    saving.await.unwrap();
    hold.await.unwrap().unwrap();
    assert!(batch.pending.is_empty());
    assert!(batch.permits.is_empty());
    assert_eq!(batch.budget.available_permits(), irc::RECEIVE_BUFFER_UNITS);
    let work = serde_json::to_value(store.work_snapshot_for_test()).unwrap();
    assert_eq!(work["append"]["successful_commits"], 1);
    assert_eq!(work["append"]["committed_rows"], 1024);
    assert_eq!(store.stats().await.unwrap().observation_count, 1024);
    for name in ["alpha", "beta"] {
        let channel = name.to_owned();
        let actual = store.call(move |conn| {
            let mut statement = conn.prepare("SELECT account,kind,received_at FROM observations WHERE channel=?1 ORDER BY id")?;
            let rows = statement.query_map([channel], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await.unwrap();
        let expected = (0..1024)
            .map(receipt)
            .filter(|row| row.channel == name)
            .map(|row| (row.account.unwrap(), row.kind, row.received_at))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn boundary_flushes_keep_each_transaction_at_most_1024_rows() {
    let (_directory, store, sessions) = store_fixture().await;
    let mut batch = BatchFixture::new();
    for start in (0..2049).step_by(256) {
        batch.admit(batch.observations(start, (2049 - start).min(256)));
    }
    flush_pending(&store, &sessions, &mut batch.pending, &mut batch.permits)
        .await
        .unwrap();
    let work = serde_json::to_value(store.work_snapshot_for_test()).unwrap();
    assert_eq!(work["append"]["successful_commits"], 3);
    assert_eq!(work["append"]["larger_than_512_batches"], 2);
    assert_eq!(work["append"]["larger_than_512_rows"], 2048);
    assert_eq!(work["append"]["undersized_rows"], 1);
    assert_eq!(store.stats().await.unwrap().observation_count, 2049);
    assert_eq!(batch.budget.available_permits(), irc::RECEIVE_BUFFER_UNITS);
}

#[tokio::test]
async fn failed_coalesced_append_rolls_back_without_retaining_rows_for_replay() {
    let (_directory, store, sessions) = store_fixture().await;
    let mut batch = BatchFixture::new();
    batch.ready();
    for start in [512, 768] {
        batch
            .sender
            .try_send(batch.observations(start, 256))
            .unwrap();
    }
    batch.coalesce();
    store.interrupt_next_append_for_test();
    assert!(
        flush_pending(&store, &sessions, &mut batch.pending, &mut batch.permits)
            .await
            .is_err()
    );
    assert!(
        batch.pending.is_empty(),
        "failed rows cannot be replayed by cleanup"
    );
    assert_eq!(
        batch.budget.available_permits(),
        irc::RECEIVE_BUFFER_UNITS - 1024
    );
    assert_eq!(store.stats().await.unwrap().observation_count, 0);
    let work = serde_json::to_value(store.work_snapshot_for_test()).unwrap();
    assert_eq!(work["append"]["successful_commits"], 0);
    let failure = store.diagnostics().first_failure.unwrap();
    assert_eq!(failure.operation, crate::diagnostics::Operation::Append);
    assert_eq!(
        failure.category,
        crate::diagnostics::Category::SqliteInterrupt
    );
    drop(batch);
    assert_eq!(store.stats().await.unwrap().observation_count, 0);
}

#[tokio::test]
async fn queued_coalesced_append_failure_stops_the_collector_without_replay_or_reconnect() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("coalesced-failure.sqlite3")).await.unwrap();
        store.track(vec!["alpha".into()], true).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (_channels, channels_rx) = watch::channel(vec!["alpha".into()]);
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let health = status(&store);
        let collector = tokio::spawn(run_endpoint(store.clone(), channels_rx, shutdown_rx, health.clone(), endpoint));
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("CAP REQ :twitch.tv/membership\r\n"));
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("PASS SCHMOOPIIE\r\n"));
        let nick = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
        peer.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("JOIN #alpha\r\n"));
        let budget = loop {
            let current = health.read().await;
            if current.rooms.len() == 1 && current.rooms[0].state == "joining" {
                break current.receive_budget.clone().unwrap();
            }
            drop(current);
            tokio::task::yield_now().await;
        };
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let held_store = store.clone();
        let hold = tokio::spawn(async move {
            held_store.call_write(crate::diagnostics::Operation::Initialize, move |_| {
                let _ = entered_tx.send(());
                release_rx.recv_timeout(Duration::from_secs(10))
                    .map_err(|_| crate::store::StoreError::Unavailable)?;
                Ok(())
            }).await
        });
        entered_rx.await.unwrap();
        store.reset_work_for_test();
        store.interrupt_next_append_for_test();
        // RoomReady queues a durable state change behind the held actor. All
        // observation envelopes then arrive before that state change can finish.
        let mut frame = format!(":tmi.twitch.tv 366 {guest} #alpha :End\r\n");
        for index in 0..1280 {
            frame.push_str(&format!(":account{index}!account{index}@account{index}.tmi.twitch.tv JOIN #alpha\r\n"));
        }
        assert!(frame.len() < 128 * 1024);
        peer.send(Message::text(frame)).await.unwrap();
        peer.send(Message::text("PING :queued-before-failure\r\n")).await.unwrap();
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("PONG :queued-before-failure\r\n"));
        assert_eq!(budget.available_permits(), irc::RECEIVE_BUFFER_UNITS - 1282);
        release_tx.send(()).unwrap();
        let mut parted = false;
        while let Some(message) = peer.next().await {
            match message.unwrap() {
                Message::Text(text) => {
                    assert_eq!(text, "PART #alpha\r\n");
                    parted = true;
                }
                Message::Close(_) => { let _ = peer.flush().await; break; }
                _ => {}
            }
        }
        assert!(parted, "storage failure stops the transport explicitly");
        collector.await.unwrap();
        hold.await.unwrap().unwrap();
        let current = health.read().await;
        assert_eq!(current.state, "storage_error");
        assert_eq!(current.connected_channels, 0);
        assert!(current.rooms.iter().all(|room| room.state == "gap"));
        drop(current);
        assert_eq!(budget.available_permits(), irc::RECEIVE_BUFFER_UNITS);
        assert_eq!(store.stats().await.unwrap().observation_count, 0);
        let sessions = store.history("alpha".into(), None, 10).await.unwrap();
        assert_eq!(sessions.len(), 1, "the collector does not reconnect after append failure");
        assert_eq!(sessions[0].end_reason.as_deref(), Some("storage_failure_collection_stopped"));
        assert!(sessions[0].ended_at.is_some());
        let work = serde_json::to_value(store.work_snapshot_for_test()).unwrap();
        assert_eq!(work["append"]["successful_commits"], 0, "failed or queued rows are never replayed");
        let failure = store.diagnostics().first_failure.unwrap();
        assert_eq!(failure.operation, crate::diagnostics::Operation::Append);
        assert_eq!(failure.category, crate::diagnostics::Category::SqliteInterrupt);
    }).await.expect("coalesced failure fixture stays finite");
}
