//! Continuous collection with explicit connection sessions and visible failure state.
use crate::{
    irc::{self, StreamEvent},
    store::{now, NewObservation, Store},
};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::{watch, OwnedSemaphorePermit, RwLock, Semaphore};

#[derive(Debug, Clone, Serialize)]
pub struct CollectorStatus {
    pub state: String,
    pub started_at: String,
    pub storage_path: String,
    pub last_error: Option<String>,
    pub active_session_id: Option<i64>,
    pub enabled_channels: usize,
    pub connected_channels: usize,
    pub evidence_scope: &'static str,
    pub broadcast_metadata: &'static str,
    pub rooms: Vec<RoomStatus>,
    #[serde(skip)]
    pub receive_budget: Option<Arc<Semaphore>>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RoomStatus {
    pub channel: String,
    pub state: String,
    pub last_positive_at: Option<String>,
    pub last_error: Option<String>,
    pub coverage: &'static str,
}
pub type SharedStatus = Arc<RwLock<CollectorStatus>>;

pub fn status(store: &Store) -> SharedStatus {
    Arc::new(RwLock::new(CollectorStatus {
        state: "starting".into(),
        started_at: now(),
        storage_path: store.path().display().to_string(),
        last_error: None,
        active_session_id: None,
        enabled_channels: 0,
        connected_channels: 0,
        evidence_scope: irc::EVIDENCE_SCOPE,
        broadcast_metadata: "see /api/metadata/status",
        rooms: Vec::new(),
        receive_budget: None,
    }))
}

pub async fn run(
    store: Store,
    channels: watch::Receiver<Vec<String>>,
    shutdown: watch::Receiver<bool>,
    status: SharedStatus,
) {
    run_endpoint(
        store,
        channels,
        shutdown,
        status,
        "wss://irc-ws.chat.twitch.tv:443".into(),
    )
    .await;
}

async fn flush_pending(
    store: &Store,
    sessions: &BTreeMap<String, i64>,
    pending: &mut Vec<NewObservation>,
    permits: &mut Vec<OwnedSemaphorePermit>,
) -> Result<(), crate::store::StoreError> {
    let mut grouped = BTreeMap::<i64, Vec<NewObservation>>::new();
    let mut count = 0;
    for event in std::mem::take(pending) {
        let id = *sessions
            .get(&event.channel)
            .ok_or(crate::store::StoreError::Invalid)
            .inspect_err(|error| {
                store.record_failure(crate::diagnostics::Operation::Append, error)
            })?;
        grouped.entry(id).or_default().push(event);
        count += 1;
        if count == 512 {
            store
                .append_sessions(std::mem::take(&mut grouped).into_iter().collect())
                .await?;
            count = 0;
        }
    }
    if !grouped.is_empty() {
        store.append_sessions(grouped.into_iter().collect()).await?;
    }
    // The admission budget includes pending/in-flight receipts until every part
    // of this flush has committed, including a room-boundary flush.
    permits.clear();
    Ok(())
}

async fn publish_rooms(
    status: &SharedStatus,
    rooms: &BTreeMap<String, RoomStatus>,
    sessions: &BTreeMap<String, i64>,
    requested: usize,
) {
    let mut state = status.write().await;
    state.rooms = rooms.values().cloned().collect();
    state.enabled_channels = requested;
    state.connected_channels = rooms
        .values()
        .filter(|room| room.state == "collecting")
        .count();
    state.active_session_id = sessions.values().next().copied();
    state.state = if requested == 0 {
        "idle"
    } else if state.connected_channels > 0 {
        "collecting"
    } else if rooms.values().any(|room| room.state == "unavailable") {
        "membership_unavailable"
    } else {
        "joining"
    }
    .into();
    if rooms.values().all(|room| room.last_error.is_none()) && state.connected_channels > 0 {
        state.last_error = None;
    }
}

async fn run_endpoint(
    store: Store,
    mut channels: watch::Receiver<Vec<String>>,
    mut shutdown: watch::Receiver<bool>,
    status: SharedStatus,
    endpoint: String,
) {
    let mut failures = 0u32;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let requested = channels.borrow_and_update().clone();
        {
            let mut state = status.write().await;
            state.enabled_channels = requested.len();
            state.connected_channels = 0;
            state.active_session_id = None;
            state.rooms.clear();
        }
        if requested.is_empty() {
            status.write().await.state = "idle".into();
            tokio::select! {result=channels.changed()=>{if result.is_err(){break;}},_=shutdown.changed()=>break}
            continue;
        }
        status.write().await.state = "connecting".into();
        let mut rooms = BTreeMap::<String, RoomStatus>::new();
        let mut sessions = BTreeMap::<String, i64>::new();
        let (events_tx, mut events_rx) = irc::receipt_queue();
        status.write().await.receive_budget = Some(events_tx.budget_handle());
        let transport_end = events_tx.end_time_handle();
        let (stop_tx, stop_rx) = watch::channel(false);
        let connection_endpoint = endpoint.clone();
        let connection_channels = channels.clone();
        let worker = tokio::spawn(async move {
            irc::stream_dynamic_endpoint(
                connection_channels,
                events_tx,
                stop_rx,
                &connection_endpoint,
            )
            .await
        });
        let mut reason = None::<String>;
        let mut storage_failure = false;
        let mut pending = Vec::<NewObservation>::new();
        let mut pending_permits = Vec::<OwnedSemaphorePermit>::new();
        let mut flush = tokio::time::interval(Duration::from_millis(250));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let began = std::time::Instant::now();
        loop {
            let outcome: Result<(), crate::store::StoreError> = tokio::select! {
                _=flush.tick(),if !pending.is_empty()=> {
                    let saved=flush_pending(&store,&sessions,&mut pending,&mut pending_permits).await;
                    if saved.is_ok() {status.write().await.rooms=rooms.values().cloned().collect();}
                    saved
                },
                result=channels.changed(),if reason.is_none()=> {
                    if result.is_err() {reason=Some("configuration_unavailable".into());let _=stop_tx.send(true);}
                    let desired=channels.borrow_and_update().clone();
                    publish_rooms(&status,&rooms,&sessions,desired.len()).await;
                    Ok(())
                },
                _=shutdown.changed(),if reason.is_none()=> {reason=Some("shutdown".into());let _=stop_tx.send(true);Ok(())},
                event=events_rx.recv()=> {
                    let Some(envelope)=event else {break;};
                    let source_at=envelope.received_at().to_owned();
                    let (event,permit)=envelope.into_parts();
                    match event {
                        StreamEvent::Connected=> {status.write().await.state="joining".into();Ok(())},
                        StreamEvent::RoomJoining(channel)=> {
                            let saved=store.begin_session_at(vec![channel.clone()],source_at).await;
                            match saved {
                                Ok(id)=> {
                                    sessions.insert(channel.clone(),id);
                                    rooms.insert(channel.clone(),RoomStatus{channel,state:"joining".into(),last_positive_at:None,last_error:None,coverage:"unknown"});
                                    let desired=channels.borrow().len();
                                    publish_rooms(&status,&rooms,&sessions,desired).await;
                                    Ok(())
                                },
                                Err(error)=>Err(error),
                            }
                        },
                        StreamEvent::RoomReady(channel)=> {
                            match sessions.get(&channel).copied() {
                                Some(id)=> {
                                    let saved=store.room_state(id,channel.clone(),true).await;
                                    if saved.is_ok() {
                                        if let Some(room)=rooms.get_mut(&channel) {room.state="collecting".into();room.last_error=None;}
                                        let desired=channels.borrow().len();publish_rooms(&status,&rooms,&sessions,desired).await;
                                    }
                                    saved
                                },
                                None=>{
                                    store.record_failure(crate::diagnostics::Operation::RoomState, &crate::store::StoreError::Invalid);
                                    Err(crate::store::StoreError::Invalid)
                                },
                            }
                        },
                        StreamEvent::RoomUnavailable(channel)|StreamEvent::RoomLeft(channel)=> {
                            // The transport marks an unavailable room before its retry; removing
                            // the room has the same evidence boundary and cannot extend a visit.
                            let desired=channels.borrow().clone();
                            let unavailable=desired.contains(&channel);
                            let mut saved=flush_pending(&store,&sessions,&mut pending,&mut pending_permits).await;
                            if saved.is_ok() {
                                if let Some(id)=sessions.remove(&channel) {
                                    saved=store.end_session_at(id,if unavailable {"membership_unavailable"} else {"configuration_changed"}.into(),source_at).await;
                                }
                            }
                            if saved.is_ok() {
                                if unavailable {
                                    if let Some(room)=rooms.get_mut(&channel) {room.state="unavailable".into();room.last_error=Some("Membership unavailable; paced room retry pending. Other rooms continue collecting.".into());}
                                } else {rooms.remove(&channel);}
                                publish_rooms(&status,&rooms,&sessions,desired.len()).await;
                            }
                            saved
                        },
                        StreamEvent::Observations(events)=> {
                            pending_permits.push(permit);
                            for event in &events {
                                if matches!(event.kind.as_str(),"names"|"join") {
                                    if let Some(room)=rooms.get_mut(&event.channel) {room.last_positive_at=Some(event.received_at.clone());}
                                }
                            }
                            pending.extend(events);
                            if pending.len()>=512 {flush_pending(&store,&sessions,&mut pending,&mut pending_permits).await} else {Ok(())}
                        },
                    }
                },
            };
            if let Err(error) = outcome {
                storage_failure = true;
                reason = Some("storage_failure_collection_stopped".into());
                let mut state = status.write().await;
                state.state = "storage_error".into();
                state.last_error=Some(format!("Storage failed; collection stopped; uncommitted observations may be missing. {error}"));
                let _ = stop_tx.send(true);
                break;
            }
        }
        if !storage_failure
            && flush_pending(&store, &sessions, &mut pending, &mut pending_permits)
                .await
                .is_err()
        {
            storage_failure = true;
            reason = Some("storage_failure_collection_stopped".into());
            status.write().await.last_error = Some(
                "Storage failed; collection stopped; uncommitted observations may be missing."
                    .into(),
            );
        }
        let _ = stop_tx.send(true);
        drop(events_rx);
        let worker_error = match worker.await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("collector_worker_failed".into()),
        };
        let failed = worker_error.is_some();
        let end_reason = session_end_reason(reason, worker_error);
        let ended_at = transport_end.lock().ok().and_then(|at| at.clone());
        for (_, id) in sessions {
            let saved = match &ended_at {
                Some(at) => {
                    store
                        .end_session_at(id, end_reason.clone(), at.clone())
                        .await
                }
                None => store.end_session_uncertain(id, end_reason.clone()).await,
            };
            if saved.is_err() {
                storage_failure = true;
            }
        }
        {
            let mut state = status.write().await;
            state.active_session_id = None;
            state.connected_channels = 0;
            for room in &mut state.rooms {
                room.state = "gap".into();
            }
            if storage_failure {
                state.state = "storage_error".into();
                state.last_error = Some(format!(
                    "{} Session end: {end_reason}",
                    state
                        .last_error
                        .as_deref()
                        .unwrap_or("Storage failed; session end may be missing.")
                ));
            } else if failed {
                state.last_error = Some(end_reason);
            }
        }
        if storage_failure {
            return;
        }
        if *shutdown.borrow() {
            break;
        }
        failures = if began.elapsed() > Duration::from_secs(120) {
            0
        } else if failed {
            (failures + 1).min(4)
        } else {
            0
        };
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            % 1000;
        let delay = Duration::from_secs((6u64.saturating_mul(1u64 << failures)).min(60))
            + Duration::from_millis(u64::from(jitter));
        status.write().await.state = "backoff".into();
        tokio::select! {_=tokio::time::sleep(delay)=>{},_=shutdown.changed()=>break}
    }
    let mut state = status.write().await;
    state.state = "stopped".into();
    state.active_session_id = None;
    state.connected_channels = 0;
    for room in &mut state.rooms {
        room.state = "stopped".into();
    }
}

fn session_end_reason(reason: Option<String>, worker_error: Option<String>) -> String {
    match (reason, worker_error) {
        (Some(reason), Some(error)) => format!("{reason}; {error}"),
        (Some(reason), None) => reason,
        (None, Some(error)) => error,
        (None, None) => "connection_ended".into(),
    }
}

#[cfg(test)]
#[path = "collector_diagnostics_tests.rs"]
mod diagnostics_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn stop_reasons_preserve_concurrent_worker_failure_evidence() {
        let error = irc::IrcError::Backpressure.to_string();
        for reason in [
            "shutdown",
            "configuration_changed",
            "storage_failure_collection_stopped",
        ] {
            assert_eq!(
                session_end_reason(Some(reason.into()), Some(error.clone())),
                format!("{reason}; {error}")
            );
        }
        assert_eq!(
            session_end_reason(Some("shutdown".into()), None),
            "shutdown"
        );
        assert_eq!(session_end_reason(None, Some(error.clone())), error);
        assert_eq!(session_end_reason(None, None), "connection_ended");
    }

    #[tokio::test]
    async fn removing_one_channel_preserves_the_other_rooms_evidence_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("test.sqlite3")).await.unwrap();
        store
            .track(vec!["alpha".into(), "beta".into()], true)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (channels_tx, channels_rx) = watch::channel(vec!["alpha".into(), "beta".into()]);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await;
            let _ = socket.next().await;
            let nick = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let guest = nick.strip_prefix("NICK ").unwrap().trim();
            socket.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #alpha,#beta\r\n")
            );
            socket.send(Message::text(format!(":tmi.twitch.tv 366 {guest} #alpha :End\r\n:tmi.twitch.tv 366 {guest} #beta :End\r\n:alice!alice@alice.tmi.twitch.tv JOIN #alpha\r\n:alice!alice@alice.tmi.twitch.tv JOIN #beta\r\nPING :before-change\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PONG :before-change\r\n")
            );
            channels_tx.send(vec!["alpha".into()]).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #beta\r\n")
            );
            socket
                .send(Message::text(
                    ":bob!bob@bob.tmi.twitch.tv JOIN #alpha\r\nPING :after-change\r\n",
                ))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PONG :after-change\r\n")
            );
            shutdown_tx.send(true).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #alpha\r\n")
            );
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let health = status(&store);
        tokio::time::timeout(
            Duration::from_secs(10),
            run_endpoint(store.clone(), channels_rx, shutdown_rx, health, endpoint),
        )
        .await
        .unwrap();
        server.await.unwrap();
        let alpha = store.history("alpha".into(), None, 10).await.unwrap();
        let beta = store.history("beta".into(), None, 10).await.unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(beta.len(), 1);
        assert_eq!(alpha[0].end_reason.as_deref(), Some("shutdown"));
        assert_eq!(beta[0].end_reason.as_deref(), Some("configuration_changed"));
        let alice = store
            .search(Some("alice".into()), Some("alpha".into()), None, 10)
            .await
            .unwrap();
        let bob = store
            .search(Some("bob".into()), Some("alpha".into()), None, 10)
            .await
            .unwrap();
        assert_eq!(alice[0].session_id, bob[0].session_id);
    }

    #[tokio::test]
    async fn received_flood_is_batched_and_fully_drained_before_session_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("test.sqlite")).await.unwrap();
        store.track(vec!["test".into()], true).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("CAP REQ :twitch.tv/membership\r\n")
            );
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PASS SCHMOOPIIE\r\n")
            );
            let nick = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let guest = nick.strip_prefix("NICK ").unwrap().trim().to_owned();
            socket.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("JOIN #test\r\n")
            );
            let mut received = format!(":tmi.twitch.tv 366 {guest} #test :End\r\n");
            for index in 0..512 {
                received.push_str(&format!(
                    ":account{index}!account{index}@account{index}.tmi.twitch.tv JOIN #test\r\n"
                ));
            }
            socket.send(Message::text(received)).await.unwrap();
            socket
                .send(Message::text("PING :fixture\r\n"))
                .await
                .unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PONG :fixture\r\n")
            );
            shutdown_tx.send(true).unwrap();
            assert_eq!(
                socket.next().await.unwrap().unwrap(),
                Message::text("PART #test\r\n")
            );
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let (_channels, channels_rx) = watch::channel(vec!["test".into()]);
        let health = status(&store);
        tokio::time::timeout(
            Duration::from_secs(10),
            run_endpoint(
                store.clone(),
                channels_rx,
                shutdown_rx,
                health.clone(),
                endpoint,
            ),
        )
        .await
        .unwrap();
        server.await.unwrap();
        assert_eq!(store.stats().await.unwrap().observation_count, 513);
        let sessions = store.history("test".into(), None, 10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].end_reason.as_deref(), Some("shutdown"));
        assert_eq!(health.read().await.state, "stopped");
        assert_eq!(
            store
                .search(Some("account511".into()), None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(test)]
#[path = "collector_buffer_tests.rs"]
mod buffer_tests;
