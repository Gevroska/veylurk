use super::*;

fn stamp_at(time: &str) -> String {
    format!("2026-10-23T{time}.000Z")
}

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(super::super::SCHEMA).unwrap();
    conn.execute("INSERT INTO metadata_broadcasts(id,stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at,offline_observed_at) VALUES(1,'stream-one','caster','Latest title must never leak','game','en',?1,?2,?3,?4)", params![stamp_at("07:00:00"),stamp_at("07:01:00"),stamp_at("08:00:00"),stamp_at("08:01:00")]).unwrap();
    title(&conn, 1, "07:01:00", "First title");
    title(&conn, 1, "07:02:00", "Changed title");
    check(&conn, 1, "07:01:00", "live", Some(1));
    check(&conn, 2, "07:02:00", "live", Some(1));
    conn
}

fn check(conn: &Connection, id: i64, time: &str, state: &str, broadcast: Option<i64>) {
    conn.execute("INSERT INTO metadata_checks(id,channel,checked_at,state,broadcast_id) VALUES(?1,'caster',?2,?3,?4)", params![id,stamp_at(time),state,broadcast]).unwrap();
}

fn title(conn: &Connection, broadcast: i64, time: &str, title: &str) {
    conn.execute("INSERT INTO metadata_titles(broadcast_id,seen_at,title,game_name,language) VALUES(?1,?2,?3,'game','en')", params![broadcast,stamp_at(time),title]).unwrap();
}

fn point(conn: &Connection, time: &str) -> Context {
    lookup(conn, "caster".into(), stamp_at(time), None).unwrap()
}

#[test]
fn exact_and_between_checks_use_previous_check_title_not_latest_broadcast_title() {
    let conn = fixture();
    let exact = point(&conn, "07:01:00");
    assert_eq!(exact.basis, "exact_check");
    assert_eq!(exact.reason, None);
    assert_eq!(exact.title.unwrap().title, "First title");
    let between = point(&conn, "07:01:30");
    assert_eq!(between.basis, "last_reported");
    assert_eq!(between.check.unwrap().id, 1);
    assert_eq!(between.next_check.unwrap().id, 2);
    assert_eq!(between.title.unwrap().title, "First title");
    assert_eq!(
        point(&conn, "07:02:00").title.unwrap().title,
        "Changed title"
    );
}

#[test]
fn title_history_is_not_backfilled_and_empty_titles_are_retained() {
    let conn = fixture();
    conn.execute(
        "DELETE FROM metadata_titles WHERE seen_at=?1",
        [stamp_at("07:01:00")],
    )
    .unwrap();
    let missing = point(&conn, "07:01:30");
    assert_eq!(missing.reason, Some("missing_title_history"));
    assert!(missing.title.is_none());
    title(&conn, 1, "07:01:00", "");
    let empty = point(&conn, "07:01:30");
    assert_eq!(empty.basis, "last_reported");
    assert_eq!(empty.title.unwrap().title, "");
}

#[test]
fn no_history_and_offline_unknown_checks_do_not_carry_live_title() {
    let conn = fixture();
    let before = point(&conn, "07:00:30");
    assert_eq!(before.reason, Some("no_history"));
    assert!(before.check.is_none());
    assert!(before.next_check.is_some());
    for (state, reason) in [
        ("unknown", "metadata_unknown"),
        ("offline", "offline_report"),
    ] {
        conn.execute(
            "UPDATE metadata_checks SET state=?1,broadcast_id=NULL WHERE id=1",
            [state],
        )
        .unwrap();
        let result = point(&conn, "07:01:30");
        assert_eq!(result.reason, Some(reason));
        assert!(result.title.is_none());
        assert!(result.broadcast.is_none());
        let scoped = lookup(&conn, "caster".into(), stamp_at("07:01:30"), Some(1)).unwrap();
        assert_eq!(scoped.reason, Some(reason));
        assert!(scoped.title.is_none());
    }
}

#[test]
fn long_polling_brackets_and_outside_window_do_not_report_titles() {
    let conn = fixture();
    check(&conn, 3, "07:10:00", "live", Some(1));
    assert_eq!(point(&conn, "07:03:00").reason, Some("polling_gap"));
    check(&conn, 4, "08:00:00", "live", Some(1));
    assert_eq!(point(&conn, "08:00:00").basis, "exact_check");
    // Even the retained not-live timestamp cannot extend the confirmed window.
    assert_eq!(
        point(&conn, "08:00:01").reason,
        Some("outside_broadcast_window")
    );
    assert_eq!(
        point(&conn, "08:01:00").reason,
        Some("outside_broadcast_window")
    );
}

#[test]
fn freshness_threshold_is_inclusive_and_exact_checks_ignore_future_corruption() {
    let conn = fixture();
    conn.execute(
        "UPDATE metadata_checks SET checked_at=?1 WHERE id=2",
        [stamp_at("07:03:30")],
    )
    .unwrap();
    assert_eq!(point(&conn, "07:01:30").basis, "last_reported");
    conn.execute(
        "UPDATE metadata_checks SET checked_at='2026-10-23T07:03:30.001Z' WHERE id=2",
        [],
    )
    .unwrap();
    assert_eq!(point(&conn, "07:01:30").reason, Some("polling_gap"));
    conn.execute(
        "UPDATE metadata_checks SET checked_at='invalid',state='corrupt' WHERE id=2",
        [],
    )
    .unwrap();
    assert_eq!(point(&conn, "07:01:00").basis, "exact_check");
    assert_eq!(
        point(&conn, "07:01:30").reason,
        Some("inconsistent_history")
    );
}

#[test]
fn broadcast_transition_is_ambiguous_between_checks_but_exact_check_is_authoritative() {
    let conn = fixture();
    conn.execute("INSERT INTO metadata_broadcasts(id,stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at) VALUES(2,'stream-two','caster','New','game','en',?1,?2,?2)",params![stamp_at("07:01:15"),stamp_at("07:02:00")]).unwrap();
    conn.execute("UPDATE metadata_checks SET broadcast_id=2 WHERE id=2", [])
        .unwrap();
    title(&conn, 2, "07:02:00", "New");
    assert_eq!(
        point(&conn, "07:01:30").reason,
        Some("transition_ambiguous")
    );
    assert_eq!(point(&conn, "07:01:00").basis, "exact_check");
    assert_eq!(point(&conn, "07:02:00").broadcast.unwrap().id, 2);
    let wrong_scope = lookup(&conn, "caster".into(), stamp_at("07:01:00"), Some(2)).unwrap();
    assert_eq!(wrong_scope.reason, Some("broadcast_mismatch"));
    assert!(wrong_scope.title.is_none());
    assert!(matches!(
        lookup(&conn, "caster".into(), stamp_at("07:01:00"), Some(99)),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        lookup(&conn, "other".into(), stamp_at("07:01:00"), Some(1)),
        Err(StoreError::Invalid)
    ));
}

#[test]
fn inconsistent_history_is_unknown_and_duplicate_timestamps_choose_highest_id() {
    let conn = fixture();
    check(&conn, 3, "07:01:00", "unknown", None);
    assert_eq!(point(&conn, "07:01:00").check.unwrap().id, 3);
    conn.execute("DELETE FROM metadata_checks WHERE id=3", [])
        .unwrap();
    // Deliberately corrupt only this transaction to exercise legacy dangling history.
    // Keep foreign keys enabled and roll back before the separate clock-corruption case.
    conn.execute_batch("PRAGMA foreign_keys=ON; BEGIN; PRAGMA defer_foreign_keys=ON;")
        .unwrap();
    conn.execute("UPDATE metadata_checks SET broadcast_id=99 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        point(&conn, "07:01:30").reason,
        Some("inconsistent_history")
    );
    conn.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.pragma_query_value(None, "defer_foreign_keys", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT broadcast_id FROM metadata_checks WHERE id=1",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let mut foreign_key_check = conn.prepare("PRAGMA foreign_key_check").unwrap();
    assert!(foreign_key_check
        .query([])
        .unwrap()
        .next()
        .unwrap()
        .is_none());
    conn.execute(
        "UPDATE metadata_broadcasts SET first_seen_at=?1 WHERE id=1",
        [stamp_at("08:10:00")],
    )
    .unwrap();
    assert_eq!(
        point(&conn, "07:01:30").reason,
        Some("inconsistent_history")
    );
}

#[test]
fn point_lookups_use_existing_time_indexes_and_accept_equivalent_rfc3339_offsets() {
    let conn = fixture();
    for (sql, expected) in [
        (PRECEDING, "metadata_checks_channel_time"),
        (FOLLOWING, "metadata_checks_channel_time"),
        (TITLE, "metadata_titles_time"),
    ] {
        let explain = format!("EXPLAIN QUERY PLAN {sql}");
        let mut statement = conn.prepare(&explain).unwrap();
        let rows = if sql == TITLE {
            statement
                .query_map(params![1, stamp_at("07:01:30")], |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        } else {
            statement
                .query_map(params!["caster", stamp_at("07:01:30")], |row| {
                    row.get::<_, String>(3)
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert!(rows.iter().any(|row| row.contains(expected)), "{rows:?}");
        assert!(
            !rows.iter().any(|row| row.contains("TEMP B-TREE")),
            "{rows:?}"
        );
    }
    assert_eq!(
        stamp("2026-10-23T09:01:30+02:00").unwrap(),
        stamp_at("07:01:30")
    );
    assert!(stamp("+10000-01-01T00:00:00Z").is_err());
    assert!(stamp("invalid").is_err());
}

#[tokio::test]
async fn route_validates_fields_and_uses_query_job_without_current_metadata_health() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(directory.path().join("point.sqlite3"))
        .await
        .unwrap();
    let metadata = super::super::initialize(store.clone()).await.unwrap();
    metadata.health.write().await.configured = false;
    metadata.health.write().await.state = "error";
    let router = metadata.router();
    for query in [
        "",
        "at=invalid",
        "at=2026-10-23T07:01:00Z&extra=x",
        "at=2026-10-23T07:01:00Z&at=2026-10-23T07:02:00Z",
        "at=2026-10-23T07:01:00Z&broadcast_id=0",
        "at=2026-10-23T07:01:00Z&broadcast_id=1&broadcast_id=2",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/channels/caster/metadata/at?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 16_384).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "invalid_query");
    }
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/channels/caster/metadata/at?at=2026-10-23T07:01:00Z")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16_384).await.unwrap()).unwrap();
    assert_eq!(body["reason"], "no_history");
    assert_eq!(body["at"], stamp_at("07:01:00"));
    // A query deadline must cancel this read; it must not use a durable-mutation job.
    store.call_write(Operation::MetadataWrite,|conn| {
        // Remove this index only in the temporary fixture so the actual endpoint crosses
        // the SQLite progress callback threshold. Production indexes are checked above.
        conn.execute_batch("DROP INDEX metadata_checks_channel_time; WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n<2000) INSERT INTO metadata_checks(channel,checked_at,state) SELECT 'caster',printf('2026-10-23T07:%02d:00.000Z',n%60),'unknown' FROM numbers;")?;
        Ok(())
    }).await.unwrap();
    // Force expiration through the existing test hook; do not wait five wall-clock seconds.
    store.expire_next_query_for_test();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/channels/caster/metadata/at?at=2026-10-23T07:01:00Z")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 16_384).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "metadata_storage");
    let failure = store.diagnostics().first_failure.unwrap();
    assert_eq!(failure.job_class, Some(crate::diagnostics::JobClass::Query));
    assert_eq!(failure.operation, Operation::Query);
    assert!(failure.progress_interrupt_requested);
}
