//! Continuous collection with explicit connection sessions and visible failure state.
use crate::{
    irc::{self, StreamEvent},
    store::{now, NewObservation, Store},
};
use serde::Serialize;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch, RwLock};

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
        }
        if requested.is_empty() {
            status.write().await.state = "idle".into();
            tokio::select! { _=channels.changed()=>{}, _=shutdown.changed()=>break }
            continue;
        }
        let session = match store.begin_session(requested.clone()).await {
            Ok(id) => id,
            Err(error) => {
                let mut state = status.write().await;
                state.state = "storage_error".into();
                state.last_error = Some(error.to_string());
                return;
            }
        };
        {
            let mut state = status.write().await;
            state.state = "connecting".into();
            state.active_session_id = Some(session);
        }
        let (events_tx, mut events_rx) = mpsc::channel(64);
        let (stop_tx, stop_rx) = watch::channel(false);
        let connection_endpoint = endpoint.clone();
        let worker = tokio::spawn(async move {
            irc::stream_endpoint(requested, events_tx, stop_rx, &connection_endpoint).await
        });
        let mut reason = None::<String>;
        let mut ready = HashSet::new();
        let mut storage_failure = false;
        let mut pending = Vec::<NewObservation>::new();
        let mut flush = tokio::time::interval(Duration::from_millis(250));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let began = std::time::Instant::now();
        loop {
            tokio::select! {
                _=flush.tick(),if !pending.is_empty()=> {
                    if store.append(session,std::mem::take(&mut pending)).await.is_err() {
                        storage_failure=true; reason=Some("storage_failure_collection_stopped".into());
                        let mut state=status.write().await; state.state="storage_error".into(); state.last_error=Some("Storage failed; collection stopped; uncommitted observations may be missing.".into());
                        let _=stop_tx.send(true); break;
                    }
                }
                _=channels.changed(),if reason.is_none()=> { reason=Some("configuration_changed".into()); let _=stop_tx.send(true); }
                _=shutdown.changed(),if reason.is_none()=> { reason=Some("shutdown".into()); let _=stop_tx.send(true); }
                event=events_rx.recv()=> {
                    match event {
                        Some(StreamEvent::Connected)=> { status.write().await.state="joining".into(); }
                        Some(StreamEvent::RoomReady(channel))=> {
                            if let Err(error)=store.room_state(session,channel.clone(),true).await {
                                storage_failure=true;reason=Some("storage_failure_collection_stopped".into());let _=stop_tx.send(true);
                                let mut state=status.write().await;state.state="storage_error".into();state.last_error=Some(error.to_string());break;
                            }
                            ready.insert(channel); let mut state=status.write().await; state.connected_channels=ready.len(); state.state="collecting".into();
                        }
                        Some(StreamEvent::RoomUnavailable(channel))=> {
                            ready.remove(&channel);
                            if let Err(error)=store.room_state(session,channel,false).await {
                                storage_failure=true;reason=Some("storage_failure_collection_stopped".into());let _=stop_tx.send(true);
                                let mut state=status.write().await;state.state="storage_error".into();state.last_error=Some(error.to_string());break;
                            }
                            let mut state=status.write().await;state.connected_channels=ready.len();state.last_error=Some("One or more channels have unavailable membership; healthy rooms continue collecting.".into());
                        }
                        Some(StreamEvent::Observations(observations))=> {
                            pending.extend(observations);
                            while pending.len()>=512 {
                                let batch=pending.drain(..512).collect();
                                if store.append(session,batch).await.is_err() {
                                    storage_failure=true; reason=Some("storage_failure_collection_stopped".into());
                                    let mut state=status.write().await; state.state="storage_error".into(); state.last_error=Some("Storage failed; collection stopped; uncommitted observations may be missing.".into());
                                    let _=stop_tx.send(true); break;
                                }
                            }
                            if storage_failure {break;}
                        }
                        None=>break,
                    }
                }
            }
        }
        if !storage_failure
            && !pending.is_empty()
            && store
                .append(session, std::mem::take(&mut pending))
                .await
                .is_err()
        {
            storage_failure = true;
            reason = Some("storage_failure_collection_stopped".into());
            let mut state = status.write().await;
            state.state = "storage_error".into();
            state.last_error = Some(
                "Storage failed; collection stopped; uncommitted observations may be missing."
                    .into(),
            );
            let _ = stop_tx.send(true);
        }
        drop(events_rx);
        let outcome = worker.await;
        let failed = matches!(&outcome, Ok(Err(_)) | Err(_));
        let end_reason = reason.unwrap_or_else(|| match outcome {
            Ok(Ok(())) => "connection_ended".into(),
            Ok(Err(error)) => error.to_string(),
            Err(_) => "collector_worker_failed".into(),
        });
        if let Err(error) = store.end_session(session, end_reason.clone()).await {
            storage_failure = true;
            let mut state = status.write().await;
            state.state = "storage_error".into();
            state.last_error = Some(error.to_string());
        }
        {
            let mut state = status.write().await;
            state.active_session_id = None;
            state.connected_channels = 0;
            if failed {
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
        // Configuration changes are coalesced while the minimum reconnect delay remains enforced.
        tokio::select! { _=tokio::time::sleep(delay)=>{}, _=shutdown.changed()=>break }
    }
    let mut state = status.write().await;
    state.state = "stopped".into();
    state.active_session_id = None;
    state.connected_channels = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

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
