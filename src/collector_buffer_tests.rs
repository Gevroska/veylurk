//! Acceptance coverage through the real collector, WebSocket transport and SQLite actor.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::{BTreeSet, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::{net::TcpStream, sync::oneshot, time::Instant};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};

const ROOMS: usize = 50;
const RECEIVE_UNITS: usize = 32_768;
const INITIAL_BACKLOG: usize = 4_096;
const BURST_RECEIPTS: usize = 26_000;
const RECEIPTS_PER_ROUND: usize = INITIAL_BACKLOG + BURST_RECEIPTS;
type Peer = WebSocketStream<TcpStream>;

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn delayed_room_boundaries_keep_transport_time_and_do_not_bridge_a_known_gap() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("source-boundaries.sqlite3")).await.unwrap();
        store.track(vec!["alpha".into(), "beta".into()], true).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (channels_tx, channels_rx) = watch::channel(vec!["alpha".into()]);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let health = status(&store);
        let collector = tokio::spawn(run_endpoint(store.clone(), channels_rx, shutdown_rx, health.clone(), endpoint));
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(peer_text(&mut peer).await, "CAP REQ :twitch.tv/membership\r\n");
        assert_eq!(peer_text(&mut peer).await, "PASS SCHMOOPIIE\r\n");
        let nick = peer_text(&mut peer).await;
        let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
        peer.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
        assert_eq!(peer_text(&mut peer).await, "JOIN #alpha\r\n");
        peer.send(Message::text(format!(":tmi.twitch.tv 366 {guest} #alpha :End\r\n:alice!alice@alice.tmi.twitch.tv JOIN #alpha\r\n"))).await.unwrap();
        loop {
            if store.stats().await.unwrap().observation_count == 2 {
                let current = health.read().await;
                if current.connected_channels == 1 && current.receive_budget.as_ref().is_some_and(|budget| budget.available_permits() == RECEIVE_UNITS) { break; }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let held_store = store.clone();
        let hold = tokio::spawn(async move {
            held_store.call(move |_| {
                let _ = entered_tx.send(());
                release_rx.recv_timeout(Duration::from_secs(20)).map_err(|_| crate::store::StoreError::Unavailable)?;
                Ok(())
            }).await
        });
        entered_rx.await.unwrap();
        channels_tx.send(vec!["alpha".into(), "beta".into()]).unwrap();
        // Production pacing puts this attempt six seconds after alpha. The SQLite
        // actor is held throughout joining, the first receipt, and alpha's gap.
        assert_eq!(peer_text(&mut peer).await, "JOIN #beta\r\n");
        peer.send(Message::text(format!(":tmi.twitch.tv 366 {guest} #beta :End\r\n:bob!bob@bob.tmi.twitch.tv JOIN #beta\r\n:tmi.twitch.tv NOTICE #alpha :Unavailable\r\n"))).await.unwrap();
        assert_eq!(peer_text(&mut peer).await, "PART #alpha\r\n");
        ping(&mut peer, "source-gap-known").await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        release_tx.send(()).unwrap();
        hold.await.unwrap().unwrap();
        loop {
            if store.stats().await.unwrap().observation_count == 4 {
                let current = health.read().await;
                if current.connected_channels == 1
                    && current.rooms.iter().any(|room| room.channel == "alpha" && room.state == "unavailable")
                    && current.rooms.iter().any(|room| room.channel == "beta" && room.state == "collecting")
                    && current.receive_budget.as_ref().is_some_and(|budget| budget.available_permits() == RECEIVE_UNITS) { break; }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let (ended, recorded, beta_started, beta_first) = store.call(|connection| {
            let (ended, recorded): (String, String) = connection.query_row(
                "SELECT s.ended_at,s.closed_recorded_at FROM sessions s JOIN session_channels sc ON sc.session_id=s.id WHERE sc.channel='alpha'",
                [], |row| Ok((row.get(0)?,row.get(1)?)))?;
            let (started, first): (String, String) = connection.query_row(
                "SELECT s.started_at,MIN(o.received_at_utc) FROM sessions s JOIN session_channels sc ON sc.session_id=s.id JOIN observations o ON o.session_id=s.id WHERE sc.channel='beta' GROUP BY s.id",
                [], |row| Ok((row.get(0)?,row.get(1)?)))?;
            Ok((ended,recorded,started,first))
        }).await.unwrap();
        let ended = chrono::DateTime::parse_from_rfc3339(&ended).unwrap();
        let recorded = chrono::DateTime::parse_from_rfc3339(&recorded).unwrap();
        assert!((recorded - ended).num_milliseconds() >= 1000, "transport and persistence boundaries stay separate");
        assert!(beta_started <= beta_first, "a queued joining attempt cannot start after its received evidence");
        let inside_gap = (ended + chrono::Duration::milliseconds(500)).to_rfc3339();
        assert!(store.presence("alpha".into(), Some("alice".into()), inside_gap, 0, 10).await.unwrap().is_empty(), "a later database write cannot extend presence past the known transport gap");
        shutdown_tx.send(true).unwrap();
        assert_eq!(peer_text(&mut peer).await, "PART #beta\r\n");
        assert!(matches!(peer.next().await.unwrap().unwrap(), Message::Close(_)));
        let _ = peer.flush().await;
        collector.await.unwrap();
        assert_eq!(store.stats().await.unwrap().observation_count, 4);
        assert_eq!(store.stats().await.unwrap().session_count, 2);
    }).await.expect("delayed-boundary fixture remains finite");
}

async fn peer_text(peer: &mut Peer) -> String {
    tokio::time::timeout(Duration::from_secs(8), async {
        peer.next()
            .await
            .expect("collector connection remained open")
            .expect("collector WebSocket message")
            .into_text()
            .expect("collector IRC text")
            .to_string()
    })
    .await
    .expect("collector IRC command arrived before its deadline")
}

async fn ping(peer: &mut Peer, token: &str) {
    let began = Instant::now();
    tokio::time::timeout(Duration::from_secs(1), async {
        peer.send(Message::text(format!("PING :{token}\r\n")))
            .await
            .unwrap();
        assert_eq!(peer_text(peer).await, format!("PONG :{token}\r\n"));
    })
    .await
    .expect("IRC PONG remained responsive while SQLite was blocked");
    assert!(began.elapsed() <= Duration::from_secs(1));
}

async fn send_receipts<S>(peer: &mut S, first: usize, count: usize)
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    // Each JOIN line produces a singleton receipt. Grouping lines in small WebSocket
    // frames avoids turning the fixture into a test of socket syscall throughput.
    for start in (first..first + count).step_by(200) {
        let end = (start + 200).min(first + count);
        let mut frame = String::new();
        for index in start..end {
            let account = format!("u{:06}", index + 1);
            frame.push_str(&format!(
                ":{account}!{account}@{account}.tmi.twitch.tv JOIN #channel{:02}\r\n",
                index % ROOMS
            ));
        }
        assert!(frame.len() < 128 * 1024, "fixture frame stays bounded");
        peer.send(Message::text(frame)).await.unwrap();
    }
}

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum RecoveryStage {
    InitialRooms,
    AfterFirstStall,
    AfterSecondStall,
}

#[derive(Clone, Copy)]
struct RecoveryTarget<'a> {
    observations: usize,
    stage: RecoveryStage,
    original_budget: Option<&'a Arc<Semaphore>>,
    work_at_release: Option<crate::store::StorageWorkForTest>,
}

#[derive(serde::Serialize)]
struct RecoveryStats {
    observations: i64,
    sessions: i64,
    completed_elapsed_ms: u64,
}

#[derive(Default)]
struct RecoveryProgress {
    stats_requests: usize,
    stats_request_pending: bool,
    last_stats: Option<RecoveryStats>,
    last_observation_progress_elapsed_ms: Option<u64>,
}

fn recovery_health(
    current: &CollectorStatus,
    original_budget: Option<&Arc<Semaphore>>,
) -> serde_json::Value {
    // Copy only aggregate values and an allowlisted state. Full status contains paths,
    // channel identities and free-text errors, none of which belong in failure output.
    let state = match current.state.as_str() {
        "starting" => "starting",
        "idle" => "idle",
        "connecting" => "connecting",
        "joining" => "joining",
        "collecting" => "collecting",
        "membership_unavailable" => "membership_unavailable",
        "storage_error" => "storage_error",
        "backoff" => "backoff",
        "stopped" => "stopped",
        _ => "other",
    };
    serde_json::json!({
        "state": state,
        "enabled_rooms": current.enabled_channels,
        "connected_rooms": current.connected_channels,
        "has_error": current.last_error.is_some(),
        "rooms": current.rooms.len(),
        "collecting_rooms": current.rooms.iter().filter(|room| room.state == "collecting").count(),
        "gap_rooms": current.rooms.iter().filter(|room| room.state == "gap").count(),
        "unavailable_rooms": current.rooms.iter().filter(|room| room.state == "unavailable").count(),
        "rooms_with_error": current.rooms.iter().filter(|room| room.last_error.is_some()).count(),
        "available_receive_permits": current.receive_budget.as_ref().map(|budget| budget.available_permits()),
        "same_receive_budget": original_budget.and_then(|original| current.receive_budget.as_ref().map(|budget| Arc::ptr_eq(original, budget))),
    })
}

fn recovery_context(
    store: &Store,
    health: &SharedStatus,
    target: RecoveryTarget<'_>,
    began: Instant,
    progress: &RecoveryProgress,
) -> String {
    let elapsed_ms = began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    // Never queue another database job or await a status lock after the acceptance deadline.
    // If SQLite is still blocked, last_stats is explicitly a completed earlier sample.
    let current = health
        .try_read()
        .ok()
        .map(|state| recovery_health(&state, target.original_budget));
    serde_json::json!({
        "schema_version": 2,
        "stage": target.stage,
        "recovery_elapsed_ms": elapsed_ms,
        "expected_observations": target.observations,
        "expected_sessions": ROOMS,
        "receive_capacity": RECEIVE_UNITS,
        "stats_requests": progress.stats_requests,
        "stats_request_pending": progress.stats_request_pending,
        "last_completed_stats": progress.last_stats,
        "last_completed_stats_age_ms": progress.last_stats.as_ref().map(|stats| elapsed_ms.saturating_sub(stats.completed_elapsed_ms)),
        "last_observation_progress_age_ms": progress.last_observation_progress_elapsed_ms.map(|at| elapsed_ms.saturating_sub(at)),
        "original_available_receive_permits": target.original_budget.map(|budget| budget.available_permits()),
        "collector": current,
        "diagnostics": store.diagnostics(),
        "store_work": {
            "completion_window": if target.work_at_release.is_some() { "since_round_start" } else { "since_store_open" },
            "at_release": target.work_at_release,
            "now": store.work_snapshot_for_test(),
            "timing_scope": "Full actor execution durations of operations completing in the window; actor queue wait excluded. Append SQL includes any SQLite busy wait; commit includes FULL sync and automatic checkpoint. Stats includes filesystem metadata. The fixture's actor hold is not append SQL time.",
        },
    }).to_string()
}

async fn recovered(
    store: &Store,
    health: &SharedStatus,
    observations: usize,
    stage: RecoveryStage,
    original_budget: Option<&Arc<Semaphore>>,
    work_at_release: Option<crate::store::StorageWorkForTest>,
) -> Arc<tokio::sync::Semaphore> {
    let target = RecoveryTarget {
        observations,
        stage,
        original_budget,
        work_at_release,
    };
    let began = Instant::now();
    let mut progress = RecoveryProgress::default();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            progress.stats_requests += 1;
            progress.stats_request_pending = true;
            let stats_result = store.stats().await;
            progress.stats_request_pending = false;
            let stats = stats_result.unwrap_or_else(|_| {
                panic!(
                    "recovery storage statistics failed: {}",
                    recovery_context(store, health, target, began, &progress)
                );
            });
            let elapsed_ms = began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            if progress
                .last_stats
                .as_ref()
                .is_none_or(|last| last.observations != stats.observation_count)
            {
                progress.last_observation_progress_elapsed_ms = Some(elapsed_ms);
            }
            progress.last_stats = Some(RecoveryStats {
                observations: stats.observation_count,
                sessions: stats.session_count,
                completed_elapsed_ms: elapsed_ms,
            });
            let state = health.read().await;
            let budget = state.receive_budget.clone();
            if stats.observation_count == observations as i64
                && state.connected_channels == ROOMS
                && budget
                    .as_ref()
                    .is_some_and(|budget| budget.available_permits() == RECEIVE_UNITS)
            {
                assert_eq!(stats.session_count, ROOMS as i64);
                assert_eq!(state.state, "collecting");
                assert!(
                    state.last_error.is_none(),
                    "no collection error after recovery"
                );
                assert_eq!(state.rooms.len(), ROOMS);
                assert!(state
                    .rooms
                    .iter()
                    .all(|room| room.state == "collecting" && room.last_error.is_none()));
                return budget.unwrap();
            }
            drop(state);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let budget = result.unwrap_or_else(|_| {
        panic!(
            "all receipts became durable and all receive permits recovered: {}",
            recovery_context(store, health, target, began, &progress)
        );
    });
    if work_at_release.is_some() {
        eprintln!(
            "buffered-stall recovery complete: {}",
            recovery_context(store, health, target, began, &progress)
        );
    }
    budget
}

#[test]
fn recovery_timeout_health_context_contains_only_finite_state_and_aggregates() {
    let secret = "SECRET_channel_account_path_error";
    let budget = Arc::new(Semaphore::new(RECEIVE_UNITS - 512));
    let current = CollectorStatus {
        state: secret.into(),
        started_at: secret.into(),
        storage_path: secret.into(),
        last_error: Some(secret.into()),
        active_session_id: Some(1),
        enabled_channels: ROOMS,
        connected_channels: ROOMS - 1,
        evidence_scope: secret,
        broadcast_metadata: secret,
        rooms: vec![RoomStatus {
            channel: secret.into(),
            state: "gap".into(),
            last_positive_at: Some(secret.into()),
            last_error: Some(secret.into()),
            coverage: secret,
        }],
        receive_budget: Some(budget.clone()),
    };
    let context = recovery_health(&current, Some(&budget));
    assert_eq!(context["state"], "other");
    assert_eq!(context["gap_rooms"], 1);
    assert_eq!(context["available_receive_permits"], RECEIVE_UNITS - 512);
    assert_eq!(context["same_receive_budget"], true);
    assert!(!context.to_string().contains(secret));
    assert!(context.to_string().len() < 1024);
}

#[derive(Default)]
struct RoomEvidence {
    session: Option<i64>,
    previous_account: usize,
    joins: usize,
    names_end: usize,
}

async fn assert_durable_integrity(store: &Store, expected_joins: usize, ended: bool) {
    // Inspect raw rows independently of the denormalized statistics API. Assertions
    // report aggregate counts only, without printing synthetic account identities.
    let (rows, distinct, violations, session_count, session_violations) = store
        .call(move |connection| {
            let mut rooms: BTreeMap<String, RoomEvidence> = (0..ROOMS)
                .map(|index| (format!("channel{index:02}"), RoomEvidence::default()))
                .collect();
            let mut accounts = HashSet::new();
            let mut violations = 0usize;
            let mut row_count = 0usize;
            let mut statement = connection.prepare(
                "SELECT channel,account,kind,session_id FROM observations ORDER BY id",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                row_count += 1;
                let channel: String = row.get(0)?;
                let account: Option<String> = row.get(1)?;
                let kind: String = row.get(2)?;
                let session: i64 = row.get(3)?;
                let Some(room) = rooms.get_mut(&channel) else {
                    violations += 1;
                    continue;
                };
                if let Some(previous) = room.session.replace(session) {
                    violations += usize::from(previous != session);
                }
                match kind.as_str() {
                    "names_end" => {
                        room.names_end += 1;
                        violations += usize::from(account.is_some());
                    }
                    "join" => {
                        room.joins += 1;
                        let index = account.as_deref().and_then(|value| value.strip_prefix('u'))
                            .and_then(|value| value.parse::<usize>().ok());
                        if let Some(index) = index.filter(|index| *index > 0 && *index <= expected_joins) {
                            violations += usize::from(index <= room.previous_account);
                            violations += usize::from(channel != format!("channel{:02}", (index - 1) % ROOMS));
                            violations += usize::from(!accounts.insert(index));
                            room.previous_account = index;
                        } else {
                            violations += 1;
                        }
                    }
                    _ => violations += 1,
                }
            }
            for (index, room) in rooms.values().enumerate() {
                let expected = expected_joins / ROOMS + usize::from(index < expected_joins % ROOMS);
                violations += usize::from(room.names_end != 1 || room.joins != expected || room.session.is_none());
            }
            let room_sessions: HashSet<_> = rooms.values().filter_map(|room| room.session).collect();
            violations += usize::from(room_sessions.len() != ROOMS);
            let (session_count, session_violations): (i64, i64) = connection.query_row(
                "SELECT COUNT(*),COALESCE(SUM(CASE WHEN (?1 AND ended_at IS NOT NULL AND end_reason='shutdown' AND end_uncertain=0) OR (NOT ?1 AND ended_at IS NULL AND end_reason IS NULL AND end_uncertain=0) THEN 0 ELSE 1 END),0) FROM sessions",
                [ended],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok((row_count, accounts.len(), violations, session_count, session_violations))
        })
        .await
        .unwrap();
    assert_eq!(
        rows,
        ROOMS + expected_joins,
        "exact durable raw receipt count"
    );
    assert_eq!(
        distinct, expected_joins,
        "every positive receipt has a distinct account"
    );
    assert_eq!(
        violations, 0,
        "room assignment, receipt order and evidence sessions are intact"
    );
    assert_eq!(session_count, ROOMS as i64, "one evidence session per room");
    assert_eq!(
        session_violations, 0,
        "session lifetime and shutdown reason are intact"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_room_collector_recovers_twice_from_thirteen_second_sqlite_stalls() {
    tokio::time::timeout(Duration::from_secs(180), async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("buffered-stall.sqlite3")).await.unwrap();
        let channels: Vec<String> = (0..ROOMS).map(|index| format!("channel{index:02}")).collect();
        store.track(channels.clone(), true).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (_channels_tx, channels_rx) = watch::channel(channels.clone());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let health = status(&store);
        let collector = tokio::spawn(run_endpoint(store.clone(), channels_rx, shutdown_rx, health.clone(), endpoint));
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(peer_text(&mut peer).await, "CAP REQ :twitch.tv/membership\r\n");
        assert_eq!(peer_text(&mut peer).await, "PASS SCHMOOPIIE\r\n");
        let nick_command = peer_text(&mut peer).await;
        let guest = nick_command.strip_prefix("NICK ").unwrap().trim().to_owned();
        peer.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();

        // Exercise the production ten-room/six-second pacing: all fifty rooms must
        // be ready before the storage stall can establish a valid baseline.
        let mut joined = BTreeSet::new();
        while joined.len() < ROOMS {
            let command = peer_text(&mut peer).await;
            let batch: Vec<_> = command.strip_prefix("JOIN ").expect("paced JOIN command")
                .trim().split(',').map(|channel| channel.trim_start_matches('#').to_owned()).collect();
            assert!(!batch.is_empty() && batch.len() <= 10);
            let mut response = String::new();
            for channel in batch {
                assert!(channels.contains(&channel) && joined.insert(channel.clone()));
                response.push_str(&format!(":tmi.twitch.tv 353 {guest} = #{channel} :{guest}\r\n:tmi.twitch.tv 366 {guest} #{channel} :End\r\n"));
            }
            peer.send(Message::text(response)).await.unwrap();
        }
        let budget = recovered(&store, &health, ROOMS, RecoveryStage::InitialRooms, None, None).await;
        assert_durable_integrity(&store, 0, false).await;

        for round in 0..2 {
            // Start a fresh completion window only after the previous recovery and
            // integrity query completed. No actor job or extra polling is introduced.
            store.reset_work_for_test();
            let held = Arc::new(AtomicBool::new(false));
            let actor_held = held.clone();
            let (entered_tx, entered_rx) = oneshot::channel();
            let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
            let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
            let held_store = store.clone();
            // Spawn so Store::call is actually polled and reaches the SQLite actor.
            let hold = tokio::spawn(async move {
                held_store.call(move |_| {
                    actor_held.store(true, Ordering::SeqCst);
                    let _ = entered_tx.send(());
                    // A finite watchdog also releases the actor if a peer assertion fails.
                    if start_rx.recv_timeout(Duration::from_secs(15)).is_err() {
                        actor_held.store(false, Ordering::SeqCst);
                        return Err(crate::store::StoreError::Unavailable);
                    }
                    let began = std::time::Instant::now();
                    std::thread::sleep(Duration::from_secs(13));
                    // Keep the actor blocked for the terminal PONG's full one-second
                    // allowance. A failed test disconnects this channel; a missing
                    // release has a five-second watchdog, bounding this hold to 18s.
                    let release = release_rx.recv_timeout(Duration::from_secs(5));
                    actor_held.store(false, Ordering::SeqCst);
                    match release {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(began.elapsed()),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(crate::store::StoreError::Unavailable),
                    }
                }).await
            });
            entered_rx.await.unwrap();
            let first = round * RECEIPTS_PER_ROUND;
            send_receipts(&mut peer, first, INITIAL_BACKLOG).await;
            ping(&mut peer, "initial-backlog").await;
            assert!(held.load(Ordering::SeqCst));
            assert_eq!(budget.available_permits(), RECEIVE_UNITS - INITIAL_BACKLOG,
                "all initial receipts remain charged while SQLite is held");

            start_tx.send(()).unwrap();
            let began = Instant::now();
            let (mut writer, mut reader) = peer.split();
            let (ping_tx, mut ping_rx) = tokio::sync::mpsc::unbounded_channel::<(&'static str, Instant, bool)>();
            // Reading PONGs runs concurrently with the paced writer, so a legal
            // one-second PONG delay cannot pause or lower the receipt injection rate.
            let emit = async {
                let mut pace = tokio::time::interval_at(began, Duration::from_millis(100));
                pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                for batch in 0..BURST_RECEIPTS / 200 {
                    pace.tick().await;
                    send_receipts(&mut writer, first + INITIAL_BACKLOG + batch * 200, 200).await;
                    if batch % 20 == 0 {
                        assert!(held.load(Ordering::SeqCst), "SQLite still held before the paced PING");
                        let sent = Instant::now();
                        if batch == 120 {
                            assert!(sent.duration_since(began) >= Duration::from_secs(12));
                        }
                        ping_tx.send(("during-stall", sent, batch == 120)).unwrap();
                        writer.send(Message::text("PING :during-stall\r\n")).await.unwrap();
                    }
                }
                let sent_elapsed = began.elapsed();
                ping_tx.send(("burst-complete", Instant::now(), false)).unwrap();
                writer.send(Message::text("PING :burst-complete\r\n")).await.unwrap();
                drop(ping_tx);
                sent_elapsed
            };
            let receive_pongs = async {
                let mut near_end_ping = false;
                let mut replies = 0;
                while let Some((token, sent, near_end)) = ping_rx.recv().await {
                    let response = tokio::time::timeout_at(sent + Duration::from_secs(1), reader.next())
                        .await.expect("IRC PONG arrived within one second while SQLite was held")
                        .expect("collector remained connected during the burst")
                        .expect("valid PONG WebSocket message")
                        .into_text().expect("IRC PONG text");
                    assert_eq!(response.as_str(), format!("PONG :{token}\r\n"));
                    assert!(held.load(Ordering::SeqCst), "SQLite still held after each PONG");
                    assert!(sent.elapsed() <= Duration::from_secs(1));
                    near_end_ping |= near_end;
                    replies += 1;
                }
                assert_eq!(replies, 8, "all seven periodic PONGs and the terminal PONG arrived");
                assert!(near_end_ping, "a PONG arrived after the near-end PING at twelve seconds");
            };
            let (sent_elapsed, ()) = tokio::join!(emit, receive_pongs);
            eprintln!("buffered-stall round {}: {} paced singleton receipts in {:?}", round + 1, BURST_RECEIPTS, sent_elapsed);
            assert!(sent_elapsed >= Duration::from_millis(12_800) && sent_elapsed < Duration::from_secs(13),
                "the finite 2,000-receipt/s burst fits inside the thirteen-second stall");
            assert!(held.load(Ordering::SeqCst), "all burst receipts and PONGs precede SQLite release");
            assert_eq!(budget.available_permits(), RECEIVE_UNITS - RECEIPTS_PER_ROUND,
                "every stalled receipt remains charged until commit");
            let work_at_release = store.work_snapshot_for_test();
            release_tx.send(()).unwrap();
            let actual_hold = hold.await.unwrap().unwrap();
            assert!(actual_hold >= Duration::from_secs(13));
            eprintln!("buffered-stall round {}: measured SQLite hold {:?}", round + 1, actual_hold);
            peer = writer.reunite(reader).expect("reunite the same local WebSocket");

            let expected_joins = (round + 1) * RECEIPTS_PER_ROUND;
            let stage = if round == 0 { RecoveryStage::AfterFirstStall } else { RecoveryStage::AfterSecondStall };
            let recovered_budget = recovered(&store, &health, ROOMS + expected_joins, stage, Some(&budget), Some(work_at_release)).await;
            assert!(Arc::ptr_eq(&budget, &recovered_budget), "the transport did not reconnect");
            assert_durable_integrity(&store, expected_joins, false).await;
        }

        shutdown_tx.send(true).unwrap();
        let part = peer_text(&mut peer).await;
        let parted: BTreeSet<_> = part.strip_prefix("PART ").expect("explicit shutdown PART")
            .trim().split(',').map(|channel| channel.trim_start_matches('#').to_owned()).collect();
        assert_eq!(parted, joined, "shutdown parts all fifty rooms");
        assert!(matches!(peer.next().await.unwrap().unwrap(), Message::Close(_)));
        let _ = peer.flush().await;
        collector.await.unwrap();
        assert_eq!(budget.available_permits(), RECEIVE_UNITS);
        let state = health.read().await;
        assert_eq!(state.state, "stopped");
        assert!(state.last_error.is_none(), "shutdown has no backpressure or transport error");
        drop(state);
        assert_eq!(store.stats().await.unwrap().observation_count, (ROOMS + 2 * RECEIPTS_PER_ROUND) as i64);
        assert_durable_integrity(&store, 2 * RECEIPTS_PER_ROUND, true).await;
    }).await.expect("the complete local production-path buffered-stall fixture is finite");
}
