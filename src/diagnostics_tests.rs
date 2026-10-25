use super::*;

fn sqlite_error(code: i32, message: &str) -> StoreError {
    StoreError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(code),
        Some(message.into()),
    ))
}

#[test]
fn diagnostics_preserve_first_storage_failure_and_never_serialize_error_messages() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("private-database-name.sqlite3");
    let diagnostics = Diagnostics::new(&database);
    diagnostics.signal_unavailable(Some(6));
    assert!(diagnostics.snapshot().first_failure.is_none());
    let secret = "SECRET_sql_account_path_authorization_bearer";
    diagnostics.storage_failure(
        Operation::Append,
        &sqlite_error(9, secret),
        Duration::from_millis(7),
        true,
        Some(true),
    );
    diagnostics.storage_failure(
        Operation::Checkpoint,
        &StoreError::CheckpointBusy,
        Duration::ZERO,
        false,
        Some(true),
    );
    assert!(diagnostics.flush(Duration::from_secs(2)));
    let snapshot = diagnostics.snapshot();
    let first = snapshot.first_failure.as_ref().unwrap();
    assert_eq!(first.operation, Operation::Append);
    assert_eq!(first.sqlite_primary_code, Some(9));
    assert!(first.progress_interrupt_requested);
    assert_eq!(
        snapshot.latest.as_ref().unwrap().operation,
        Operation::Checkpoint
    );
    assert_eq!(snapshot.persistence.state, PersistenceState::Ready);
    assert_eq!(snapshot.persistence.pending_records, 0);
    let api = serde_json::to_string(&snapshot).unwrap();
    let journal = fs::read_to_string(sidecar(&database)).unwrap();
    for text in [&api, &journal] {
        assert!(!text.contains(secret));
        assert!(!text.contains("private-database-name"));
    }
    let records: Vec<serde_json::Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["event"], "signal_unavailable");
    assert_eq!(records[1]["sqlite_primary_code"], 9);
    drop(diagnostics);
    let reopened = Diagnostics::new(&database);
    reopened.storage_failure(
        Operation::Generic,
        &StoreError::Invalid,
        Duration::ZERO,
        false,
        None,
    );
    assert!(reopened.flush(Duration::from_secs(2)));
    assert!(fs::read_to_string(sidecar(&database))
        .unwrap()
        .contains("\"sqlite_primary_code\":9"));
}

#[test]
fn diagnostics_journal_rotation_and_per_run_limit_are_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("bounded.sqlite3");
    let diagnostics = Diagnostics::new(&database);
    diagnostics.storage_failure(
        Operation::Append,
        &StoreError::Invalid,
        Duration::ZERO,
        false,
        None,
    );
    assert!(diagnostics.flush(Duration::from_secs(2)));
    let record = diagnostics.snapshot().latest.unwrap();
    let path = sidecar(&database);
    for _ in 0..400 {
        append_record(&path, &record).unwrap();
    }
    let mut previous = path.as_os_str().to_owned();
    previous.push(".previous");
    let previous = PathBuf::from(previous);
    assert!(fs::metadata(&path).unwrap().len() <= JOURNAL_BYTES);
    assert!(fs::metadata(&previous).unwrap().len() <= JOURNAL_BYTES);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    for _ in 1..RUN_RECORDS {
        diagnostics.storage_failure(
            Operation::Generic,
            &StoreError::Invalid,
            Duration::ZERO,
            false,
            None,
        );
        assert!(diagnostics.flush(Duration::from_secs(2)));
    }
    diagnostics.storage_failure(
        Operation::Checkpoint,
        &StoreError::CheckpointBusy,
        Duration::ZERO,
        false,
        None,
    );
    let snapshot = diagnostics.snapshot();
    assert_eq!(snapshot.persistence.dropped_count, 1);
    assert_eq!(snapshot.first_failure.unwrap().operation, Operation::Append);
    assert_eq!(snapshot.latest.unwrap().operation, Operation::Checkpoint);
}

#[test]
fn diagnostics_failed_writes_and_full_queue_are_honest_and_nonblocking() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("failed.sqlite3");
    fs::create_dir(sidecar(&database)).unwrap();
    let diagnostics = Diagnostics::new(&database);
    diagnostics.storage_failure(
        Operation::Append,
        &sqlite_error(19, "secret"),
        Duration::ZERO,
        false,
        Some(true),
    );
    assert!(
        diagnostics.flush(Duration::from_secs(2)),
        "writer acknowledged a failed attempt"
    );
    let snapshot = diagnostics.snapshot();
    assert_eq!(snapshot.persistence.state, PersistenceState::Failed);
    assert_eq!(snapshot.persistence.failure_count, 1);
    assert_eq!(snapshot.persistence.pending_records, 0);
    assert_eq!(
        snapshot.first_failure.unwrap().sqlite_primary_code,
        Some(19)
    );

    let (writer, _held_receiver) = mpsc::sync_channel(1);
    let stalled = Diagnostics {
        state: diagnostics.state.clone(),
        writer,
    };
    stalled.storage_failure(
        Operation::Generic,
        &StoreError::Invalid,
        Duration::ZERO,
        false,
        None,
    );
    stalled.storage_failure(
        Operation::Generic,
        &StoreError::Invalid,
        Duration::ZERO,
        false,
        None,
    );
    assert!(!stalled.flush(Duration::ZERO));
    let snapshot = stalled.snapshot();
    assert_eq!(snapshot.persistence.state, PersistenceState::Pending);
    assert_eq!(snapshot.persistence.pending_records, 1);
    assert_eq!(snapshot.persistence.dropped_count, 1);
}
