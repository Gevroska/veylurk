use super::*;
use crate::diagnostics::{Category, PersistenceState};

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
