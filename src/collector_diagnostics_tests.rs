use super::*;
use crate::diagnostics::{Category, Operation};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum BudgetFixtureStage {
    CreateDirectory,
    OpenStore,
    ConfigureChannel,
    ConnectTransport,
    ReadRegistration,
    AcknowledgeRegistration,
    AwaitRoomJoin,
    SeedBaseline,
    AwaitBaselineDurability,
    ReadBaselineSession,
    SendExpiredBudgetReceipt,
    AwaitReceiptDurability,
    CheckSessionContinuity,
    ReadOpenVisits,
    AwaitShutdownPart,
    AwaitShutdownClose,
    FlushShutdownClose,
    AwaitCollectorExit,
    ReadClosedSession,
    Complete,
}

struct BudgetFixtureProgress {
    stage: BudgetFixtureStage,
    began: std::time::Instant,
    stage_began: std::time::Instant,
    stats_request_pending: bool,
    expected_counts: Option<(i64, i64)>,
    last_counts: Option<(i64, i64)>,
    last_counts_elapsed_ms: Option<u64>,
}

impl BudgetFixtureProgress {
    fn new() -> Self {
        let began = std::time::Instant::now();
        Self {
            stage: BudgetFixtureStage::CreateDirectory,
            began,
            stage_began: began,
            stats_request_pending: false,
            expected_counts: None,
            last_counts: None,
            last_counts_elapsed_ms: None,
        }
    }

    fn enter(&mut self, stage: BudgetFixtureStage) {
        self.stage = stage;
        self.stage_began = std::time::Instant::now();
    }

    async fn stats(&mut self, store: &Store) -> crate::store::StorageStats {
        self.stats_request_pending = true;
        let result = store.stats().await;
        self.stats_request_pending = false;
        let stats = result.unwrap_or_else(|_| {
            panic!(
                "budget fixture storage statistics failed: {}",
                serde_json::to_string(&store.diagnostics()).unwrap()
            )
        });
        self.last_counts = Some((stats.observation_count, stats.session_count));
        self.last_counts_elapsed_ms =
            Some(self.began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        stats
    }
}

#[derive(Default)]
struct BudgetFixtureResources {
    diagnostics: Option<crate::diagnostics::Diagnostics>,
    health: Option<SharedStatus>,
    budget: Option<Arc<Semaphore>>,
    expired: Option<Arc<std::sync::atomic::AtomicBool>>,
    collector: Option<tokio::task::JoinHandle<()>>,
}

impl BudgetFixtureResources {
    fn failure_context(&self, progress: &BudgetFixtureProgress) -> String {
        let elapsed_ms = progress
            .began
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        // All observations here are synchronous snapshots. Do not enqueue a database
        // request, await a lock or read a journal after the fifteen-second deadline.
        let collector = self.health.as_ref().and_then(|health| health.try_read().ok()).map(|state| {
            let safe_state = match state.state.as_str() {
                "starting" => "starting", "connecting" => "connecting", "joining" => "joining",
                "collecting" => "collecting", "storage_error" => "storage_error", "stopped" => "stopped",
                "backoff" => "backoff", "idle" => "idle", "membership_unavailable" => "membership_unavailable",
                _ => "other",
            };
            serde_json::json!({
                "state": safe_state, "connected_rooms": state.connected_channels,
                "room_count": state.rooms.len(), "has_error": state.last_error.is_some(),
                "gap_rooms": state.rooms.iter().filter(|room| room.state == "gap").count(),
                "rooms_with_error": state.rooms.iter().filter(|room| room.last_error.is_some()).count(),
                "available_receive_permits": state.receive_budget.as_ref().map(|budget| budget.available_permits()),
                "same_receive_budget": self.budget.as_ref().and_then(|original| state.receive_budget.as_ref().map(|budget| Arc::ptr_eq(original, budget))),
            })
        });
        serde_json::json!({
            "schema_version": 1, "stage": progress.stage, "elapsed_ms": elapsed_ms,
            "stage_elapsed_ms": progress.stage_began.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            "stats_request_pending": progress.stats_request_pending,
            "expected_observations": progress.expected_counts.map(|counts| counts.0),
            "expected_sessions": progress.expected_counts.map(|counts| counts.1),
            "last_completed_observations": progress.last_counts.map(|counts| counts.0),
            "last_completed_sessions": progress.last_counts.map(|counts| counts.1),
            "last_completed_stats_age_ms": progress.last_counts_elapsed_ms.map(|at| elapsed_ms.saturating_sub(at)),
            "receive_capacity": irc::RECEIVE_BUFFER_UNITS,
            "original_available_receive_permits": self.budget.as_ref().map(|budget| budget.available_permits()),
            "expired_clock_reached": self.expired.as_ref().map(|clock| clock.load(std::sync::atomic::Ordering::Relaxed)),
            "collector_finished": self.collector.as_ref().map(|task| task.is_finished()),
            "collector": collector, "diagnostics": self.diagnostics.as_ref().map(|diagnostics| diagnostics.snapshot()),
        }).to_string()
    }
}

#[tokio::test]
async fn collector_keeps_same_session_when_durable_append_outlives_old_query_budget() {
    let mut progress = BudgetFixtureProgress::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let directory = tempfile::tempdir().unwrap();
    let mut resources = BudgetFixtureResources::default();
    let result = tokio::time::timeout_at(deadline, async {
        progress.enter(BudgetFixtureStage::OpenStore);
        let database = directory.path().join("collector-durable-budget.sqlite3");
        let diagnostics = crate::diagnostics::Diagnostics::new(&database);
        resources.diagnostics = Some(diagnostics.clone());
        let store = Store::open_with_diagnostics(&database, diagnostics).await.unwrap();
        progress.enter(BudgetFixtureStage::ConfigureChannel);
        store.track(vec!["test".into()], true).await.unwrap();
        progress.enter(BudgetFixtureStage::ConnectTransport);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (_channels, receive_channels) = watch::channel(vec!["test".into()]);
        let (shutdown, receive_shutdown) = watch::channel(false);
        let health = status(&store);
        resources.health = Some(health.clone());
        resources.collector = Some(tokio::spawn(run_endpoint(store.clone(), receive_channels, receive_shutdown, health.clone(), endpoint)));
        let (stream, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        progress.enter(BudgetFixtureStage::ReadRegistration);
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("CAP REQ :twitch.tv/membership\r\n"));
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("PASS SCHMOOPIIE\r\n"));
        let nick = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let guest = nick.strip_prefix("NICK ").unwrap().trim();
        progress.enter(BudgetFixtureStage::AcknowledgeRegistration);
        peer.send(Message::text(format!(":tmi.twitch.tv CAP * ACK :twitch.tv/membership\r\n:tmi.twitch.tv 001 {guest} :Welcome\r\n"))).await.unwrap();
        progress.enter(BudgetFixtureStage::AwaitRoomJoin);
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("JOIN #test\r\n"));
        progress.enter(BudgetFixtureStage::SeedBaseline);
        progress.expected_counts = Some((2, 1));
        peer.send(Message::text(format!(":tmi.twitch.tv 353 {guest} = #test :baseline\r\n:tmi.twitch.tv 366 {guest} #test :End\r\n"))).await.unwrap();
        progress.enter(BudgetFixtureStage::AwaitBaselineDurability);
        let budget = loop {
            let ready = health.read().await.clone();
            if progress.stats(&store).await.observation_count == 2 && ready.state == "collecting" {
                if let Some(budget) = ready.receive_budget.filter(|budget| budget.available_permits() == irc::RECEIVE_BUFFER_UNITS) { break budget; }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        resources.budget = Some(budget.clone());
        progress.enter(BudgetFixtureStage::ReadBaselineSession);
        let baseline_session = store.history("test".into(), None, 10).await.unwrap()[0].id;
        let expired = store.expire_next_append_for_test();
        resources.expired = Some(expired.clone());
        progress.enter(BudgetFixtureStage::SendExpiredBudgetReceipt);
        progress.expected_counts = Some((3, 1));
        peer.send(Message::text(":newaccount!newaccount@newaccount.tmi.twitch.tv JOIN #test\r\n")).await.unwrap();
        progress.enter(BudgetFixtureStage::AwaitReceiptDurability);
        loop {
            if progress.stats(&store).await.observation_count == 3 && budget.available_permits() == irc::RECEIVE_BUFFER_UNITS { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        progress.enter(BudgetFixtureStage::CheckSessionContinuity);
        assert!(expired.load(std::sync::atomic::Ordering::Relaxed));
        let current = health.read().await.clone();
        assert_eq!(current.state, "collecting");
        assert_eq!(current.connected_channels, 1);
        assert!(current.last_error.is_none());
        assert!(Arc::ptr_eq(&budget, current.receive_budget.as_ref().unwrap()));
        let sessions = store.history("test".into(), None, 10).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, baseline_session);
        assert!(sessions[0].ended_at.is_none());
        progress.enter(BudgetFixtureStage::ReadOpenVisits);
        let open_visits: i64 = store.call_read(Operation::Query, |conn| Ok(conn.query_row("SELECT COUNT(*) FROM history_visits WHERE end_basis IS NULL", [], |row| row.get(0))?)).await.unwrap();
        assert_eq!(open_visits, 2, "both committed accounts remain within the same coverage session");
        assert!(store.diagnostics().first_failure.is_none());
        progress.enter(BudgetFixtureStage::AwaitShutdownPart);
        shutdown.send(true).unwrap();
        assert_eq!(peer.next().await.unwrap().unwrap(), Message::text("PART #test\r\n"));
        progress.enter(BudgetFixtureStage::AwaitShutdownClose);
        assert!(matches!(peer.next().await.unwrap().unwrap(), Message::Close(_)));
        progress.enter(BudgetFixtureStage::FlushShutdownClose);
        let _ = peer.flush().await;
        progress.enter(BudgetFixtureStage::AwaitCollectorExit);
        resources.collector.as_mut().unwrap().await.unwrap();
        progress.enter(BudgetFixtureStage::ReadClosedSession);
        let ended = store.history("test".into(), None, 10).await.unwrap();
        assert_eq!(ended[0].id, baseline_session);
        assert_eq!(ended[0].end_reason.as_deref(), Some("shutdown"));
        progress.enter(BudgetFixtureStage::Complete);
    }).await;
    result.unwrap_or_else(|_| {
        panic!(
            "durable budget collector fixture timed out: {}",
            resources.failure_context(&progress)
        )
    });
}

#[test]
fn durable_budget_fixture_context_is_bounded_and_excludes_identifiers_and_errors() {
    let secret = "SECRET_account_channel_path_error";
    let budget = Arc::new(Semaphore::new(irc::RECEIVE_BUFFER_UNITS - 1));
    let health = CollectorStatus {
        state: secret.into(),
        started_at: secret.into(),
        storage_path: secret.into(),
        last_error: Some(secret.into()),
        active_session_id: Some(1),
        enabled_channels: 1,
        connected_channels: 1,
        evidence_scope: secret,
        broadcast_metadata: secret,
        rooms: vec![RoomStatus {
            channel: secret.into(),
            state: "collecting".into(),
            last_positive_at: Some(secret.into()),
            last_error: Some(secret.into()),
            coverage: secret,
        }],
        receive_budget: Some(budget.clone()),
    };
    let resources = BudgetFixtureResources {
        health: Some(Arc::new(RwLock::new(health))),
        budget: Some(budget),
        ..Default::default()
    };
    let mut progress = BudgetFixtureProgress::new();
    progress.enter(BudgetFixtureStage::AwaitReceiptDurability);
    progress.expected_counts = Some((3, 1));
    progress.last_counts = Some((2, 1));
    let context = resources.failure_context(&progress);
    let value: serde_json::Value = serde_json::from_str(&context).unwrap();
    assert_eq!(value["stage"], "await_receipt_durability");
    assert_eq!(value["last_completed_observations"], 2);
    assert_eq!(value["expected_observations"], 3);
    assert_eq!(value["collector"]["state"], "other");
    assert_eq!(value["collector"]["same_receive_budget"], true);
    assert_eq!(
        value["collector"]["available_receive_permits"],
        irc::RECEIVE_BUFFER_UNITS - 1
    );
    assert!(!context.contains(secret));
    assert!(context.len() < 4096);
}

#[tokio::test]
async fn actual_append_progress_callback_interrupt_stops_collector_retains_baseline_and_closes_explicit_failure_gap(
) {
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
        store.interrupt_next_append_for_test();
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
        assert_eq!(record.job_class, Some(crate::diagnostics::JobClass::DurableMutation));
        assert!(!record.progress_interrupt_requested, "injected SQLite interruption did not originate from a query budget");
    }).await.unwrap();
}
