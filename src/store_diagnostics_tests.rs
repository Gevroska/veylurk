use super::*;
use crate::diagnostics::{Category, PersistenceState};

// A channel gate holds the actual worker without sleeping or blocking the Tokio thread.
// Dropping the guard releases it even if an assertion fails.
struct WorkerGate {
    release: Option<std::sync::mpsc::Sender<()>>,
    _task: tokio::task::JoinHandle<Result<(), StoreError>>,
}

impl Drop for WorkerGate {
    fn drop(&mut self) {
        self.release.take();
    }
}

async fn hold_worker(store: &Store) -> WorkerGate {
    let (entered, received) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let store = store.clone();
    let task = tokio::spawn(async move {
        store
            .call_write(Operation::Checkpoint, move |_| {
                let _ = entered.send(());
                let _ = wait.recv();
                Ok(())
            })
            .await
    });
    received.await.unwrap();
    WorkerGate {
        release: Some(release),
        _task: task,
    }
}

async fn wait_for_activity(
    store: &Store,
    check: impl Fn(&crate::diagnostics::StorageActivitySnapshot) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if check(&store.storage_activity()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("activity transition");
}

#[tokio::test]
async fn activity_separates_admission_queue_and_execution_and_keeps_disconnected_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("activity.sqlite3"))
        .await
        .unwrap();
    let gate = hold_worker(&store).await;
    let first = store.storage_activity();
    assert_eq!(
        first.active.as_ref().unwrap().job_class,
        JobClass::DurableMutation
    );
    assert_eq!(
        first.active.as_ref().unwrap().operation,
        Operation::Checkpoint
    );
    assert_eq!(first.totals.started, 1);

    let executed = Arc::new(AtomicBool::new(false));
    let read_store = store.clone();
    let read_executed = executed.clone();
    let read = tokio::spawn(async move {
        read_store
            .call_read(Operation::Query, move |_| {
                read_executed.store(true, Ordering::Relaxed);
                Ok(())
            })
            .await
    });
    let write_store = store.clone();
    let write = tokio::spawn(async move {
        write_store.call_write(Operation::ChannelConfiguration, |conn| {
            conn.execute_batch("CREATE TABLE activity_write(value INTEGER); INSERT INTO activity_write VALUES (42);")?;
            Ok(())
        }).await
    });
    wait_for_activity(&store, |snapshot| snapshot.waiting_to_start == 2).await;
    read.abort();
    write.abort();
    assert!(read.await.unwrap_err().is_cancelled());
    assert!(write.await.unwrap_err().is_cancelled());

    let mut queued = Vec::new();
    for _ in 0..STORAGE_QUEUE_CAPACITY - 2 {
        let store = store.clone();
        queued.push(tokio::spawn(async move {
            store.call_read(Operation::Query, |_| Ok(())).await
        }));
    }
    wait_for_activity(&store, |snapshot| {
        snapshot.waiting_to_start == STORAGE_QUEUE_CAPACITY
    })
    .await;
    let pending_store = store.clone();
    let pending =
        tokio::spawn(async move { pending_store.call_read(Operation::Query, |_| Ok(())).await });
    wait_for_activity(&store, |snapshot| snapshot.admission_waiting == 1).await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    for task in queued {
        task.abort();
        let _ = task.await;
    }
    let held = store.storage_activity();
    assert_eq!(held.admission_waiting, 0);
    assert_eq!(held.waiting_to_start, STORAGE_QUEUE_CAPACITY);
    assert_eq!(held.totals.abandoned_admission, 1);
    assert_eq!(
        held.totals.canceled_before_start, 0,
        "queued tombstones remain bounded until dequeue"
    );
    assert_eq!(held.admission_wait_ms.count, 65);
    assert_eq!(held.queue_wait_ms.count, 1);
    assert_eq!(held.execution_ms.count, 0);
    assert!(held.oldest_queued_age_ms.is_some());
    // Only a tiny scheduler tick checks age; the worker remains controlled by the gate.
    tokio::time::sleep(Duration::from_millis(2)).await;
    let later = store.storage_activity();
    assert!(
        later.active.as_ref().unwrap().active_age_ms > first.active.as_ref().unwrap().active_age_ms
    );
    assert!(later.oldest_queued_age_ms.unwrap() >= held.oldest_queued_age_ms.unwrap());
    drop(gate);
    let value: i64 = store
        .call_read(Operation::Query, |conn| {
            Ok(conn.query_row("SELECT value FROM activity_write", [], |row| row.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(
        value, 42,
        "caller abandonment never discards an admitted mutation"
    );
    assert!(
        !executed.load(Ordering::Relaxed),
        "abandoned queued read never ran"
    );
    let completed = store.storage_activity();
    assert_eq!(completed.admission_waiting, 0);
    assert_eq!(completed.waiting_to_start, 0);
    assert!(completed.active.is_none());
    assert_eq!(completed.totals.admitted, 66);
    assert_eq!(completed.totals.started, 3);
    assert_eq!(completed.totals.succeeded, 3);
    assert_eq!(completed.totals.canceled_before_start, 63);
    assert_eq!(completed.queue_wait_ms.count, 66);
    assert_eq!(completed.execution_ms.count, 3);
    assert!(store.diagnostics().first_failure.is_none());
    assert!(serde_json::to_string(&completed).unwrap().len() < 4096);
    assert_eq!(
        completed.last_completed.unwrap().outcome,
        ActivityOutcome::Succeeded
    );
}

#[tokio::test]
async fn read_response_deadline_closes_queued_read_without_latching_storage_failure() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("response-deadline.sqlite3"))
        .await
        .unwrap();
    let gate = hold_worker(&store).await;
    tokio::time::pause();
    let executed = Arc::new(AtomicBool::new(false));
    let ran = executed.clone();
    let reader = store.clone();
    let read = tokio::spawn(async move {
        reader
            .call_read(Operation::Query, move |_| {
                ran.store(true, Ordering::Relaxed);
                Ok(())
            })
            .await
    });
    wait_for_activity(&store, |snapshot| snapshot.waiting_to_start == 1).await;
    tokio::time::advance(READ_RESPONSE_BUDGET).await;
    assert!(matches!(
        read.await.unwrap(),
        Err(StoreError::ReadWaitTimeout)
    ));
    tokio::time::resume();
    let held = store.storage_activity();
    assert_eq!(held.totals.read_response_timeouts, 1);
    assert_eq!(held.waiting_to_start, 1);
    assert_eq!(held.totals.failed, 0);
    assert!(store.diagnostics().first_failure.is_none());
    drop(gate);
    store
        .call_write(Operation::Checkpoint, |_| Ok(()))
        .await
        .unwrap();
    assert!(!executed.load(Ordering::Relaxed));
    let finished = store.storage_activity();
    assert_eq!(finished.totals.canceled_before_start, 1);
    assert_eq!(finished.totals.failed, 0);
    assert_eq!(finished.waiting_to_start, 0);
    assert!(finished.active.is_none());
    assert!(store.diagnostics().first_failure.is_none());
}

#[tokio::test]
async fn timeout_during_active_read_does_not_interrupt_write_or_leak_query_callback() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("active-deadline.sqlite3"))
        .await
        .unwrap();
    let (entered, received) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let reader = store.clone();
    let read = tokio::spawn(async move {
        reader
            .call_read(Operation::Query, move |conn| {
                let _ = entered.send(());
                let _ = wait.recv();
                Ok(conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?)
            })
            .await
    });
    received.await.unwrap();
    tokio::time::pause();
    tokio::time::advance(READ_RESPONSE_BUDGET).await;
    assert!(matches!(
        read.await.unwrap(),
        Err(StoreError::ReadWaitTimeout)
    ));
    tokio::time::resume();
    assert!(
        store.storage_activity().active.is_some(),
        "timeout does not claim blocked OS work stopped"
    );
    assert!(store.diagnostics().first_failure.is_none());
    drop(release);
    store
        .call_write(Operation::ChannelConfiguration, |conn| {
            conn.execute_batch(
                "CREATE TABLE active_write(value INTEGER); INSERT INTO active_write VALUES (7);",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let sum: i64 = store.call_read(Operation::Query, |conn| Ok(conn.query_row("WITH RECURSIVE work(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM work WHERE x<100000) SELECT SUM(x)+(SELECT value FROM active_write) FROM work", [], |row| row.get(0))?)).await.unwrap();
    assert_eq!(sum, 5_000_050_007);
    assert_eq!(store.storage_activity().totals.succeeded, 3);
    assert!(store.diagnostics().first_failure.is_none());
}

#[tokio::test]
async fn worker_drop_and_unavailable_admission_release_all_activity_slots() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("worker-drop.sqlite3"))
        .await
        .unwrap();
    let (sender, receiver) = mpsc::channel(STORAGE_QUEUE_CAPACITY);
    // Isolated memory channel: dropping its receiver models worker loss without a panic.
    store.sender = sender;
    let queued = store.clone();
    let task =
        tokio::spawn(async move { queued.call_write(Operation::Checkpoint, |_| Ok(())).await });
    wait_for_activity(&store, |snapshot| snapshot.waiting_to_start == 1).await;
    drop(receiver);
    assert!(matches!(task.await.unwrap(), Err(StoreError::Unavailable)));
    assert!(matches!(
        store.call_read(Operation::Query, |_| Ok(())).await,
        Err(StoreError::Unavailable)
    ));
    let snapshot = store.storage_activity();
    assert_eq!(snapshot.waiting_to_start, 0);
    assert_eq!(snapshot.admission_waiting, 0);
    assert!(snapshot.active.is_none());
    assert_eq!(snapshot.totals.worker_dropped_before_start, 1);
    assert_eq!(snapshot.totals.admission_unavailable, 1);
    assert_eq!(snapshot.totals.started, 0);
}

#[tokio::test]
async fn read_deadline_during_admission_does_not_enqueue_a_job_or_leak_a_permit() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("admission-deadline.sqlite3"))
        .await
        .unwrap();
    let (sender, mut receiver) = mpsc::channel(STORAGE_QUEUE_CAPACITY);
    store.sender = sender;
    for _ in 0..STORAGE_QUEUE_CAPACITY {
        assert!(store.sender.try_send(Box::new(|_| {})).is_ok());
    }
    tokio::time::pause();
    let reader = store.clone();
    let read = tokio::spawn(async move { reader.call_read(Operation::Query, |_| Ok(())).await });
    wait_for_activity(&store, |snapshot| snapshot.admission_waiting == 1).await;
    tokio::time::advance(READ_RESPONSE_BUDGET).await;
    assert!(matches!(
        read.await.unwrap(),
        Err(StoreError::ReadWaitTimeout)
    ));
    tokio::time::resume();
    let snapshot = store.storage_activity();
    assert_eq!(snapshot.admission_waiting, 0);
    assert_eq!(snapshot.totals.requested, 1);
    assert_eq!(snapshot.totals.admitted, 0);
    assert_eq!(snapshot.totals.abandoned_admission, 1);
    assert_eq!(snapshot.totals.read_response_timeouts, 1);
    assert!(store.diagnostics().first_failure.is_none());
    assert_eq!(store.sender.capacity(), 0);
    let _ = receiver.recv().await.unwrap();
    assert_eq!(
        store.sender.capacity(),
        1,
        "canceled reserve did not consume capacity"
    );
}

fn receipt(channel: &str, account: &str) -> NewObservation {
    NewObservation {
        channel: channel.into(),
        account: Some(account.into()),
        kind: "join".into(),
        received_at: "2026-01-01T00:00:30Z".into(),
    }
}

async fn counts(store: &Store) -> (i64, i64, i64, i64) {
    store.call(|conn| {
        Ok(conn.query_row("SELECT (SELECT COUNT(*) FROM observations),(SELECT COUNT(*) FROM history_visits),(SELECT SUM(observation_count) FROM relationships),(SELECT observation_count FROM collection_totals WHERE id=1)", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?)
    }).await.unwrap()
}

#[tokio::test]
async fn actual_append_progress_callback_interrupt_rolls_back_raw_derived_and_totals_and_next_job_is_fresh(
) {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("append.sqlite3");
    let store = Store::open(&database).await.unwrap();
    store
        .track(vec!["alpha".into(), "beta".into()], true)
        .await
        .unwrap();
    let alpha = store
        .begin_session_at(vec!["alpha".into()], "2026-01-01T00:00:00Z".into())
        .await
        .unwrap();
    let beta = store
        .begin_session_at(vec!["beta".into()], "2026-01-01T00:00:00Z".into())
        .await
        .unwrap();
    store
        .append(alpha, vec![receipt("alpha", "baseline")])
        .await
        .unwrap();
    let baseline = counts(&store).await;
    assert_eq!(baseline, (1, 1, 1, 1));

    store.interrupt_next_append_for_test();
    let error = store
        .append_sessions(vec![
            (alpha, vec![receipt("alpha", "newalpha")]),
            (beta, vec![receipt("beta", "newbeta")]),
        ])
        .await
        .unwrap_err();
    match &error {
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => {
            assert_eq!(code.extended_code, rusqlite::ffi::SQLITE_INTERRUPT)
        }
        _ => panic!("expected actual SQLite interruption"),
    }
    let first = store.diagnostics().first_failure.unwrap();
    assert_eq!(first.operation, Operation::Append);
    assert_eq!(first.category, Category::SqliteInterrupt);
    assert_eq!(first.sqlite_primary_code, Some(9));
    assert_eq!(first.job_class, Some(JobClass::DurableMutation));
    assert!(
        !first.progress_interrupt_requested,
        "independent fault injection is not a query-budget cancellation"
    );
    assert_eq!(first.autocommit_after, Some(true));
    assert_eq!(
        counts(&store).await,
        baseline,
        "all rooms rolled back raw, derived, relationship and aggregate writes"
    );

    // This statement reaches many callbacks. A retained expired handler would fail it.
    let sum: i64 = store.call(|conn| Ok(conn.query_row("WITH RECURSIVE work(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM work WHERE x<100000) SELECT SUM(x) FROM work", [], |row| row.get(0))?)).await.unwrap();
    assert_eq!(sum, 5_000_050_000);
    store
        .append(beta, vec![receipt("beta", "fresh")])
        .await
        .unwrap();
    assert_eq!(counts(&store).await, (2, 2, 2, 2));
    store.record_failure(Operation::Checkpoint, &StoreError::CheckpointBusy);
    assert_eq!(store.diagnostics().first_failure.unwrap(), first);
    assert_eq!(
        store.diagnostics().latest.unwrap().operation,
        Operation::Checkpoint
    );
    assert!(store.diagnostics.flush(Duration::from_secs(2)));
    assert_eq!(
        store.diagnostics().persistence.state,
        PersistenceState::Ready
    );
    let mut journal = database.as_os_str().to_owned();
    journal.push(".diagnostics.jsonl");
    let retained = std::fs::read_to_string(PathBuf::from(journal)).unwrap();
    assert!(retained.contains("\"progress_interrupt_requested\":false"));
}

#[tokio::test]
async fn bounded_durable_append_survives_expired_old_query_budget_without_sleeping() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("durable-budget.sqlite3"))
        .await
        .unwrap();
    store
        .track(vec!["alpha".into(), "beta".into()], true)
        .await
        .unwrap();
    let alpha = store
        .begin_session_at(vec!["alpha".into()], "2026-01-01T00:00:00Z".into())
        .await
        .unwrap();
    let beta = store
        .begin_session_at(vec!["beta".into()], "2026-01-01T00:00:00Z".into())
        .await
        .unwrap();
    let expired = store.expire_next_append_for_test();
    let groups = [(alpha, "alpha"), (beta, "beta")]
        .into_iter()
        .enumerate()
        .map(|(room, (session, channel))| {
            let rows = (0..256)
                .map(|index| receipt(channel, &format!("room{room}account{index}")))
                .collect();
            (session, rows)
        })
        .collect();
    store.append_sessions(groups).await.unwrap();
    assert!(
        expired.load(Ordering::Relaxed),
        "clock advanced after real transaction writes and before callback-sized SQL"
    );
    assert_eq!(counts(&store).await, (512, 512, 512, 512));
    assert!(store.diagnostics().first_failure.is_none());
    store.checkpoint().await.unwrap();
}

#[tokio::test]
async fn query_deadline_still_interrupts_real_sql_and_next_query_and_append_have_fresh_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("query-budget.sqlite3"))
        .await
        .unwrap();
    store.track(vec!["alpha".into()], true).await.unwrap();
    let session = store
        .begin_session_at(vec!["alpha".into()], "2026-01-01T00:00:00Z".into())
        .await
        .unwrap();
    store.expire_next_query_for_test();
    let error = store.call_read(Operation::Query, |conn| Ok(conn.query_row("WITH RECURSIVE work(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM work WHERE x<100000) SELECT SUM(x) FROM work", [], |row| row.get::<_, i64>(0))?)).await.unwrap_err();
    assert!(
        matches!(error, StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) if code.extended_code == rusqlite::ffi::SQLITE_INTERRUPT)
    );
    let first = store.diagnostics().first_failure.unwrap();
    assert_eq!(first.job_class, Some(JobClass::Query));
    assert_eq!(first.operation, Operation::Query);
    assert!(first.progress_interrupt_requested);
    let sum: i64 = store.call_read(Operation::Query, |conn| Ok(conn.query_row("WITH RECURSIVE work(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM work WHERE x<100000) SELECT SUM(x) FROM work", [], |row| row.get(0))?)).await.unwrap();
    assert_eq!(sum, 5_000_050_000);
    store
        .append(session, vec![receipt("alpha", "fresh")])
        .await
        .unwrap();
    assert_eq!(counts(&store).await, (1, 1, 1, 1));
    assert_eq!(store.diagnostics().first_failure.unwrap(), first);
}

#[tokio::test]
async fn diagnostic_write_failure_preserves_original_store_error_and_worker_health() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("write-failure.sqlite3");
    let store = Store::open(&database).await.unwrap();
    let mut journal = database.as_os_str().to_owned();
    journal.push(".diagnostics.jsonl");
    std::fs::create_dir(PathBuf::from(journal)).unwrap();
    let secret = "SECRET_original_backend_message";
    let error = store
        .call(move |_| {
            Err::<(), _>(StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR_READ),
                Some(secret.into()),
            )))
        })
        .await
        .unwrap_err();
    match error {
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, message)) => {
            assert_eq!(code.extended_code, rusqlite::ffi::SQLITE_IOERR_READ);
            assert_eq!(message.as_deref(), Some(secret));
        }
        _ => panic!("diagnostic persistence changed the original error"),
    }
    assert!(store.diagnostics.flush(Duration::from_secs(2)));
    let snapshot = store.diagnostics();
    assert_eq!(snapshot.persistence.state, PersistenceState::Failed);
    assert_eq!(
        snapshot.first_failure.unwrap().sqlite_primary_code,
        Some(10)
    );
    assert!(!serde_json::to_string(&store.diagnostics())
        .unwrap()
        .contains(secret));
    let activity = store.storage_activity();
    assert_eq!(activity.totals.failed, 1);
    assert_eq!(activity.execution_ms.count, 1);
    assert_eq!(
        activity.last_completed.as_ref().unwrap().outcome,
        ActivityOutcome::Failed
    );
    assert!(!serde_json::to_string(&activity).unwrap().contains(secret));
    assert_eq!(store.stats().await.unwrap().observation_count, 0);
}

#[tokio::test]
async fn initialization_failure_is_recorded_before_open_returns() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("directory-instead-of-database");
    std::fs::create_dir(&database).unwrap();
    let diagnostics = Diagnostics::new(&database);
    assert!(matches!(
        Store::open_with_diagnostics(&database, diagnostics.clone()).await,
        Err(StoreError::Sqlite(_))
    ));
    let record = diagnostics.snapshot().first_failure.unwrap();
    assert_eq!(record.operation, Operation::Initialize);
    assert_eq!(
        record.sqlite_primary_code,
        Some(rusqlite::ffi::SQLITE_CANTOPEN)
    );
    assert_eq!(record.autocommit_after, None);
    assert!(diagnostics.flush(Duration::from_secs(2)));
    assert_eq!(
        diagnostics.snapshot().persistence.state,
        PersistenceState::Ready
    );
}
