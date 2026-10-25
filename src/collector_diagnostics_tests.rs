use super::*;
use crate::diagnostics::{Category, Operation};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn actual_append_interrupt_stops_collector_retains_baseline_and_closes_explicit_failure_gap()
{
    tokio::time::timeout(Duration::from_secs(15), async {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("collector-interruption.sqlite3")).await.unwrap();
        store.track(vec!["test".into()], true).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (_channels, receive_channels) = watch::channel(vec!["test".into()]);
        let (_shutdown, receive_shutdown) = watch::channel(false);
        let health = status(&store);
        let collector = tokio::spawn(run_endpoint(store.clone(), receive_channels, receive_shutdown, health.clone(), endpoint));
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("CAP REQ :twitch.tv/membership\r\n"));
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("PASS SCHMOOPIIE\r\n"));
        let nick = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let guest = nick.strip_prefix("NICK ").unwrap().trim();
        peer.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("JOIN #test\r\n"));
        peer.send(Message::text(format!(":tmi.twitch.tv 353 {guest} = #test :baseline\r\n:tmi.twitch.tv 366 {guest} #test :End\r\n"))).await.unwrap();
        loop {
            if store.stats().await.unwrap().observation_count == 2 && health.read().await.state == "collecting" { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        store.expire_next_append_for_test();
        peer.send(Message::text(":newaccount!newaccount@newaccount.tmi.twitch.tv JOIN #test\r\n")).await.unwrap();
        let closing = tokio::spawn(async move {
            let mut parted = false;
            while let Some(message) = peer.next().await {
                match message.unwrap() {
                    Message::Text(text) => {
                        assert_eq!(text, "PART #test\r\n");
                        parted = true;
                    }
                    Message::Close(_) => { let _ = peer.flush().await; break; }
                    _ => {}
                }
            }
            assert!(parted, "storage failure stopped the transport explicitly");
        });
        collector.await.unwrap();
        closing.await.unwrap();
        let current = health.read().await.clone();
        assert_eq!(current.state, "storage_error");
        assert_eq!(current.connected_channels, 0);
        assert!(current.rooms.iter().all(|room| room.state == "gap"));
        assert!(current.last_error.unwrap().contains("storage_failure_collection_stopped"));
        assert_eq!(store.stats().await.unwrap().observation_count, 2);
        let sessions = store.history("test".into(), None, 10).await.unwrap();
        assert_eq!(sessions.len(), 1, "collector returned without a blind reconnect");
        assert_eq!(sessions[0].end_reason.as_deref(), Some("storage_failure_collection_stopped"));
        assert!(sessions[0].ended_at.is_some());
        let boundary: (i64, String, String) = store.call(|conn| {
            Ok(conn.query_row("SELECT (SELECT COUNT(*) FROM history_visits),end_basis,end_reason FROM history_visits WHERE account='baseline'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?)
        }).await.unwrap();
        assert_eq!(boundary, (1, "session_end".into(), "storage_failure_collection_stopped".into()));
        let record = store.diagnostics().first_failure.unwrap();
        assert_eq!(record.operation, Operation::Append);
        assert_eq!(record.category, Category::SqliteInterrupt);
        assert_eq!(record.sqlite_extended_code, Some(rusqlite::ffi::SQLITE_INTERRUPT));
        assert!(record.progress_interrupt_requested);
    }).await.unwrap();
}
