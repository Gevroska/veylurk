use super::*;
use std::sync::atomic::AtomicUsize;

fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
    conn.prepare(
        "SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    )
    .unwrap()
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

fn raw(conn: &Connection) -> Vec<(i64, i64, String, String, String)> {
    conn.prepare("SELECT id,session_id,channel,kind,received_at FROM observations ORDER BY id")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

type RetainedVisit = (i64, i64, i64, i64, Option<i64>, i64, String);

fn visits(conn: &Connection) -> Vec<RetainedVisit> {
    conn.prepare("SELECT id,session_id,first_observation_id,last_observation_id,end_observation_id,positive_observation_count,end_basis FROM history_visits ORDER BY id")
        .unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)))
        .unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn seed(conn: &Connection) {
    conn.execute_batch("INSERT INTO channels VALUES('room',1,1,'2024-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z');
        INSERT INTO sessions(id,started_at,ended_at,end_reason) VALUES(1,'2024-01-01T00:00:00.000Z','2024-01-01T00:00:04.000Z','fixture_end');
        INSERT INTO session_channels(session_id,channel) VALUES(1,'room');
        INSERT INTO observations(id,session_id,channel,account,kind,received_at) VALUES
        (1,1,'room','alice','join','2024-01-01T01:00:00+01:00'),
        (2,1,'room','alice','names','2024-01-01T01:00:01+01:00'),
        (3,1,'room','alice','part','2024-01-01T01:00:02+01:00');").unwrap();
}

fn legacy(conn: &Connection) {
    conn.execute_batch("CREATE TABLE channels(login TEXT PRIMARY KEY,tracked INTEGER NOT NULL,enabled INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
        CREATE TABLE sessions(id INTEGER PRIMARY KEY AUTOINCREMENT,started_at TEXT NOT NULL,ended_at TEXT,end_reason TEXT,end_uncertain INTEGER NOT NULL DEFAULT 0,closed_recorded_at TEXT);
        CREATE TABLE session_channels(session_id INTEGER NOT NULL REFERENCES sessions(id),channel TEXT NOT NULL REFERENCES channels(login),collection_state TEXT NOT NULL DEFAULT 'joining',collection_error TEXT,PRIMARY KEY(session_id,channel));
        CREATE TABLE observations(id INTEGER PRIMARY KEY AUTOINCREMENT,session_id INTEGER NOT NULL,channel TEXT NOT NULL,account TEXT,kind TEXT NOT NULL CHECK(kind IN ('names','join','part','names_end')),received_at TEXT NOT NULL,FOREIGN KEY(session_id,channel) REFERENCES session_channels(session_id,channel));").unwrap();
    seed(conn);
}

#[test]
fn fresh_schema_uses_one_core_commit_and_separate_history_schema_and_recompute_commits() {
    let mut conn = Connection::open_in_memory().unwrap();
    let commits = Arc::new(AtomicUsize::new(0));
    let recorded = commits.clone();
    conn.commit_hook(Some(move || {
        recorded.fetch_add(1, Ordering::SeqCst);
        false
    }));
    initialize_schema(&mut conn).unwrap();
    assert_eq!(
        commits.load(Ordering::SeqCst),
        1,
        "core DDL and user_version must commit together"
    );
    crate::history::initialize(&mut conn).unwrap();
    assert_eq!(
        commits.load(Ordering::SeqCst),
        3,
        "history DDL and recomputation each own one transaction"
    );
    assert!(conn.is_autocommit());
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        crate::history::checked_model_version(&conn).unwrap(),
        Some(crate::history::MODEL_VERSION)
    );
}

#[test]
fn fresh_reopen_preserves_schema_data_and_full_wal_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("startup.sqlite3");
    let mut conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", WAL_CHECKPOINT_PAGES)
        .unwrap();
    conn.pragma_update(None, "journal_size_limit", WAL_RETAINED_LIMIT_BYTES)
        .unwrap();
    initialize_schema(&mut conn).unwrap();
    crate::history::initialize(&mut conn).unwrap();
    let fresh_schema = schema(&conn);
    seed(&conn);
    conn.execute_batch("UPDATE collection_totals SET observation_count=3,session_count=1 WHERE id=1;
        INSERT INTO relationships SELECT channel,account,MIN(received_at),MAX(received_at),COUNT(*) FROM observations GROUP BY channel,account;").unwrap();
    crate::history::recompute(&mut conn).unwrap();
    let retained = raw(&conn);
    let derived = visits(&conn);
    drop(conn);
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", WAL_CHECKPOINT_PAGES)
        .unwrap();
    conn.pragma_update(None, "journal_size_limit", WAL_RETAINED_LIMIT_BYTES)
        .unwrap();
    initialize_schema(&mut conn).unwrap();
    crate::history::initialize(&mut conn).unwrap();
    assert_eq!(schema(&conn), fresh_schema);
    assert_eq!(raw(&conn), retained);
    assert_eq!(visits(&conn), derived);
    assert_eq!(derived, vec![(1, 1, 1, 2, Some(3), 2, "part".into())]);
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.pragma_query_value(None, "wal_autocheckpoint", |r| r.get::<_, i64>(0))
            .unwrap(),
        WAL_CHECKPOINT_PAGES
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_size_limit", |r| r.get::<_, i64>(0))
            .unwrap(),
        WAL_RETAINED_LIMIT_BYTES
    );
}

#[test]
fn legacy_import_backfills_relationships_totals_and_recomputes_without_rewriting_receipts() {
    let mut conn = Connection::open_in_memory().unwrap();
    legacy(&conn);
    let retained = raw(&conn);
    initialize_schema(&mut conn).unwrap();
    crate::history::initialize(&mut conn).unwrap();
    assert_eq!(raw(&conn), retained);
    assert_eq!(visits(&conn), vec![(1, 1, 1, 2, Some(3), 2, "part".into())]);
    let relationship:(String,String,i64)=conn.query_row("SELECT first_observed_at,last_observed_at,observation_count FROM relationships WHERE channel='room' AND account='alice'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        relationship,
        (
            "2024-01-01T01:00:00+01:00".into(),
            "2024-01-01T01:00:02+01:00".into(),
            3
        )
    );
    let totals: (i64, i64) = conn
        .query_row(
            "SELECT observation_count,session_count FROM collection_totals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(totals, (3, 1));
    assert_eq!(
        conn.query_row(
            "SELECT received_at_utc FROM observations WHERE id=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "2024-01-01T00:00:00.000Z"
    );
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn real_core_ddl_failure_rolls_back_preceding_schema_and_version_changes() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE observations(channel TEXT); INSERT INTO observations VALUES('retained');",
    )
    .unwrap();
    let before = schema(&conn);
    assert!(matches!(
        initialize_schema(&mut conn),
        Err(StoreError::Sqlite(_))
    ));
    assert!(conn.is_autocommit());
    assert_eq!(schema(&conn), before);
    assert_eq!(
        conn.query_row("SELECT channel FROM observations", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "retained"
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn real_history_ddl_failure_rolls_back_utc_alter_indexes_and_tables() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize_schema(&mut conn).unwrap();
    seed(&conn);
    conn.execute_batch(
        "CREATE TABLE history_visits(account TEXT); INSERT INTO history_visits VALUES('retained');",
    )
    .unwrap();
    let before = schema(&conn);
    let retained = raw(&conn);
    assert!(matches!(
        crate::history::initialize(&mut conn),
        Err(StoreError::Sqlite(_))
    ));
    assert!(conn.is_autocommit());
    assert_eq!(
        schema(&conn),
        before,
        "UTC ALTER and earlier CREATE statements must all roll back"
    );
    assert_eq!(raw(&conn), retained);
    assert_eq!(
        conn.query_row("SELECT account FROM history_visits", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "retained"
    );
}

#[test]
fn unsupported_core_or_history_versions_reject_before_schema_or_raw_mutations() {
    let mut conn = Connection::open_in_memory().unwrap();
    legacy(&conn);
    conn.pragma_update(None, "user_version", 2).unwrap();
    let before = schema(&conn);
    let retained = raw(&conn);
    assert!(matches!(
        initialize_schema(&mut conn),
        Err(StoreError::Initialize)
    ));
    assert_eq!(schema(&conn), before);
    assert_eq!(raw(&conn), retained);
    conn.pragma_update(None, "user_version", 0).unwrap();
    conn.execute_batch("CREATE TABLE history_model(id INTEGER PRIMARY KEY CHECK(id=1),version INTEGER NOT NULL); INSERT INTO history_model VALUES(1,2);").unwrap();
    let before = schema(&conn);
    assert!(matches!(
        initialize_schema(&mut conn),
        Err(StoreError::Initialize)
    ));
    assert!(matches!(
        crate::history::initialize(&mut conn),
        Err(StoreError::Initialize)
    ));
    assert_eq!(schema(&conn), before);
    assert_eq!(raw(&conn), retained);
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn covering_end_receipt_index_is_installed_for_fresh_and_existing_history() {
    let mut conn = Connection::open_in_memory().unwrap();
    initialize_schema(&mut conn).unwrap();
    crate::history::initialize(&mut conn).unwrap();
    let definition = |conn: &Connection| {
        let columns=conn.prepare("SELECT name FROM pragma_index_info('history_visits_end_receipt_scope') ORDER BY seqno")
            .unwrap().query_map([],|row|row.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        let flags:(bool,bool)=conn.query_row("SELECT \"unique\",partial FROM pragma_index_list('history_visits') WHERE name='history_visits_end_receipt_scope'",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        (columns, flags)
    };
    let expected: (Vec<String>, (bool, bool)) = (
        vec![
            "end_observation_id".into(),
            "session_id".into(),
            "channel".into(),
            "account".into(),
        ],
        (false, true),
    );
    assert_eq!(definition(&conn), expected);
    seed(&conn);
    crate::history::recompute(&mut conn).unwrap();
    let retained = raw(&conn);
    let derived = visits(&conn);
    conn.execute_batch("DROP INDEX history_visits_end_receipt_scope;")
        .unwrap();
    crate::history::initialize(&mut conn).unwrap();
    assert_eq!(definition(&conn), expected);
    assert_eq!(raw(&conn), retained);
    assert_eq!(visits(&conn), derived);
}

#[tokio::test]
async fn covering_end_receipt_lookup_preserves_timeline_parts_and_clock_evidence() {
    use crate::timeline::TimelineQuery;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("timeline-index.sqlite3"))
        .await
        .unwrap();
    store.track(vec!["room".into()], true).await.unwrap();
    let session = store
        .begin_session_at(vec!["room".into()], "2024-01-01T00:00:00.000Z".into())
        .await
        .unwrap();
    let records = [
        ("join", 0),
        ("names", 1),
        ("part", 2),
        ("part", 3),
        ("join", 6),
        ("part", 5),
        ("names", 4),
    ]
    .into_iter()
    .map(|(kind, second)| NewObservation {
        channel: "room".into(),
        account: Some("alice".into()),
        kind: kind.into(),
        received_at: format!("2024-01-01T00:00:{second:02}.000Z"),
    })
    .collect();
    store.append(session, records).await.unwrap();
    store
        .end_session_at(
            session,
            "fixture_end".into(),
            "2024-01-01T00:00:08.000Z".into(),
        )
        .await
        .unwrap();
    let query = TimelineQuery {
        account: Some("alice".into()),
        channel: Some("room".into()),
        from: Some("2024-01-01T00:00:00.000Z".into()),
        to: Some("2024-01-01T00:00:09.000Z".into()),
        limit: Some(100),
        ..TimelineQuery::default()
    };
    store
        .call_write(Operation::Initialize, |conn| {
            conn.execute_batch("DROP INDEX history_visits_end_receipt_scope;")?;
            Ok(())
        })
        .await
        .unwrap();
    let before = store.timeline(query.clone()).await.unwrap();
    store
        .call_write(Operation::Initialize, crate::history::initialize)
        .await
        .unwrap();
    let after = store.timeline(query).await.unwrap();
    assert_eq!(
        after
            .intervals
            .iter()
            .filter(|item| item.kind == "departure")
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert!(after
        .intervals
        .iter()
        .any(|item| item.id == 5 && item.out_of_order));
    let stable = |page| {
        let mut value = serde_json::to_value(page).unwrap();
        value.as_object_mut().unwrap().remove("generated_at");
        value
    };
    assert_eq!(stable(before), stable(after));
    let details=store.call_read(Operation::Query,|conn| {
        let mut statement=conn.prepare("EXPLAIN QUERY PLAN SELECT o.id FROM observations o WHERE o.kind='part' AND NOT EXISTS(SELECT 1 FROM history_visits v WHERE v.session_id=o.session_id AND v.channel=o.channel AND v.account=o.account AND v.end_observation_id=o.id)")?;
        let rows=statement.query_map([],|row|row.get::<_,String>(3))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).await.unwrap();
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("history_visits_end_receipt_scope")
                && detail.to_ascii_lowercase().contains("covering")),
        "expected a covering attached-PART lookup: {details:?}"
    );
}
