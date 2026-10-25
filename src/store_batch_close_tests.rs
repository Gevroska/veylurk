use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

type SessionRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
);
type VisitRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

fn end(session_id: i64, reason: &str, source_at: Option<&str>) -> SessionEnd {
    SessionEnd {
        session_id,
        reason: reason.into(),
        source_at: source_at.map(str::to_owned),
    }
}

fn receipt(channel: &str, account: &str, kind: &str, at: &str) -> NewObservation {
    NewObservation {
        channel: channel.into(),
        account: Some(account.into()),
        kind: kind.into(),
        received_at: at.into(),
    }
}

async fn session_rows(store: &Store) -> Vec<SessionRow> {
    store
        .call(|conn| {
            let mut statement = conn.prepare("SELECT id,started_at,ended_at,end_reason,end_uncertain,closed_recorded_at FROM sessions ORDER BY id")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
}

async fn visit_rows(store: &Store) -> Vec<VisitRow> {
    store
        .call(|conn| {
            let mut statement = conn.prepare("SELECT session_id,account,COALESCE(end_basis,'open'),end_reason,closed_at,end_observed_at,out_of_order FROM history_visits ORDER BY session_id,account,first_observation_id")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn batch_close_preserves_distinct_cutoffs_gaps_raw_evidence_and_first_close() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("source-cutoffs.sqlite3"))
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
        .begin_session_at(vec!["beta".into()], "2026-01-01T00:10:00Z".into())
        .await
        .unwrap();
    store
        .append_sessions(vec![
            (
                alpha,
                vec![
                    receipt("alpha", "alice", "join", "2026-01-01T00:00:30Z"),
                    receipt("alpha", "bob", "join", "2026-01-01T00:00:10Z"),
                    receipt("alpha", "bob", "part", "2026-01-01T00:00:45Z"),
                ],
            ),
            (
                beta,
                vec![receipt("beta", "carol", "join", "2026-01-01T00:10:30Z")],
            ),
        ])
        .await
        .unwrap();
    store
        .room_state(alpha, "alpha".into(), false)
        .await
        .unwrap();
    // Existing uncertainty must never be cleared by a later certain close.
    store
        .call_write(Operation::SessionEnd, move |conn| {
            conn.execute("UPDATE sessions SET end_uncertain=1 WHERE id=?1", [beta])?;
            Ok(())
        })
        .await
        .unwrap();
    let raw = serde_json::to_value(store.search(None, None, None, 100).await.unwrap()).unwrap();
    let boundary = store.call(move |conn| {
        Ok(conn.query_row("SELECT after_observation_id,reason,recorded_at FROM history_boundaries WHERE session_id=?1", [alpha], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?)
    }).await.unwrap();
    let old_visits = visit_rows(&store).await;
    let recorded_after = now();
    store
        .end_sessions(vec![
            end(-1, "missing", None),
            end(
                alpha,
                "alpha_transport_end",
                Some("2026-01-01T02:01:00+02:00"),
            ),
            end(alpha, "duplicate_cleanup", Some("2026-01-01T01:00:00Z")),
            end(-2, "missing", Some("2026-01-01T00:00:00Z")),
            end(beta, "beta_transport_end", Some("2026-01-01T00:11:00Z")),
        ])
        .await
        .unwrap();
    let sessions = session_rows(&store).await;
    assert_eq!(sessions[0].2.as_deref(), Some("2026-01-01T00:01:00.000Z"));
    assert_eq!(sessions[0].3.as_deref(), Some("alpha_transport_end"));
    assert!(!sessions[0].4);
    assert_eq!(sessions[1].2.as_deref(), Some("2026-01-01T00:11:00.000Z"));
    assert_eq!(sessions[1].3.as_deref(), Some("beta_transport_end"));
    assert!(sessions[1].4);
    assert!(sessions.iter().all(|row| row
        .5
        .as_deref()
        .is_some_and(|at| at >= recorded_after.as_str())));
    let visits = visit_rows(&store).await;
    assert_eq!(visits[0].2, "coverage_gap");
    assert_eq!(visits[0].3, old_visits[0].3);
    assert_eq!(visits[0].4.as_deref(), Some("2026-01-01T00:01:00.000Z"));
    assert_eq!(visits[1], old_visits[1], "PART evidence is unchanged");
    assert_eq!(visits[2].2, "session_end");
    assert_eq!(visits[2].3.as_deref(), Some("beta_transport_end"));
    assert_eq!(visits[2].4.as_deref(), Some("2026-01-01T00:11:00.000Z"));
    assert_eq!(
        serde_json::to_value(store.search(None, None, None, 100).await.unwrap()).unwrap(),
        raw
    );
    let retained_boundary = store.call(move |conn| {
        Ok(conn.query_row("SELECT after_observation_id,reason,recorded_at FROM history_boundaries WHERE session_id=?1", [alpha], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?)
    }).await.unwrap();
    assert_eq!(retained_boundary, boundary);

    store
        .end_sessions(vec![
            end(alpha, "later_cleanup", Some("2026-01-02T00:00:00Z")),
            end(beta, "later_fallback", None),
        ])
        .await
        .unwrap();
    assert_eq!(session_rows(&store).await, sessions);
    assert_eq!(visit_rows(&store).await, visits);
    store
        .call_write(Operation::Initialize, crate::history::recompute)
        .await
        .unwrap();
    assert_eq!(visit_rows(&store).await, visits);
}

#[tokio::test]
async fn batch_close_preserves_backwards_cutoffs_and_resolves_missing_clocks_per_session() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("uncertain-cutoffs.sqlite3"))
        .await
        .unwrap();
    let channels = ["alpha", "beta", "gamma", "delta"];
    store
        .track(
            channels.iter().map(|channel| (*channel).into()).collect(),
            true,
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (channel, at) in channels.into_iter().zip([
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:10:00Z",
        "2026-01-01T00:20:00Z",
        "2026-01-01T00:30:00Z",
    ]) {
        ids.push(
            store
                .begin_session_at(vec![channel.into()], at.into())
                .await
                .unwrap(),
        );
    }
    store
        .append_sessions(vec![
            (
                ids[0],
                vec![receipt("alpha", "alice", "join", "2026-01-01T00:00:30Z")],
            ),
            (
                ids[2],
                vec![receipt("gamma", "carol", "join", "2026-01-01T00:20:30Z")],
            ),
        ])
        .await
        .unwrap();
    store
        .room_state(ids[2], "gamma".into(), false)
        .await
        .unwrap();
    store
        .end_sessions(vec![
            end(ids[0], "missing_clock", None),
            end(ids[1], "missing_clock_empty", None),
            end(ids[2], "clock_reversal", Some("2026-01-01T00:20:15Z")),
            end(ids[3], "clock_reversal_empty", Some("2026-01-01T00:29:00Z")),
        ])
        .await
        .unwrap();
    let sessions = session_rows(&store).await;
    for (row, expected) in sessions.iter().zip([
        "2026-01-01T00:00:30.000Z",
        "2026-01-01T00:10:00.000Z",
        "2026-01-01T00:20:15.000Z",
        "2026-01-01T00:29:00.000Z",
    ]) {
        assert_eq!(row.2.as_deref(), Some(expected));
        assert!(row.4);
    }
    let visits = visit_rows(&store).await;
    assert!(!visits[0].6);
    assert_eq!(visits[1].2, "coverage_gap");
    assert!(visits[1].6);
    assert_eq!(visits[1].4.as_deref(), Some("2026-01-01T00:20:15.000Z"));
    store
        .call_write(Operation::Initialize, crate::history::recompute)
        .await
        .unwrap();
    assert_eq!(visit_rows(&store).await, visits);
    assert_eq!(session_rows(&store).await, sessions);
}

#[tokio::test]
async fn batch_close_rolls_back_every_session_and_visit_when_second_sql_update_fails() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("close-rollback.sqlite3"))
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
    store
        .append_sessions(vec![
            (
                alpha,
                vec![receipt("alpha", "alice", "join", "2026-01-01T00:00:30Z")],
            ),
            (
                beta,
                vec![receipt("beta", "bob", "join", "2026-01-01T00:00:30Z")],
            ),
        ])
        .await
        .unwrap();
    store.call_write(Operation::SessionEnd, move |conn| {
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_second_close BEFORE UPDATE OF ended_at ON sessions WHEN OLD.id={beta} AND NEW.ended_at IS NOT NULL BEGIN SELECT RAISE(ABORT,'second close failed after first update') WHERE EXISTS(SELECT 1 FROM sessions WHERE id={alpha} AND ended_at IS NOT NULL); SELECT RAISE(FAIL,'first close did not run'); END;"
        ))?;
        Ok(())
    }).await.unwrap();
    let before_sessions = session_rows(&store).await;
    let before_visits = visit_rows(&store).await;
    let error = store
        .end_sessions(vec![
            end(alpha, "transport_end", Some("2026-01-01T00:01:00Z")),
            end(beta, "transport_end", Some("2026-01-01T00:01:00Z")),
        ])
        .await
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, Some(message))) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER && message == "second close failed after first update")
    );
    assert_eq!(session_rows(&store).await, before_sessions);
    assert_eq!(visit_rows(&store).await, before_visits);
    let failure = store.diagnostics().first_failure.unwrap();
    assert_eq!(failure.operation, Operation::SessionEnd);
    assert_eq!(failure.job_class, Some(JobClass::DurableMutation));
    assert_eq!(failure.autocommit_after, Some(true));
    assert!(!failure.progress_interrupt_requested);
    store
        .call_write(Operation::SessionEnd, |conn| {
            conn.execute_batch("DROP TRIGGER fail_second_close")?;
            Ok(())
        })
        .await
        .unwrap();
    store
        .end_sessions(vec![
            end(alpha, "next_explicit_job", Some("2026-01-01T00:01:00Z")),
            end(beta, "next_explicit_job", Some("2026-01-01T00:01:00Z")),
        ])
        .await
        .unwrap();
    assert!(session_rows(&store).await.iter().all(|row| row.2.is_some()));
}

#[tokio::test]
async fn fifty_session_close_uses_one_commit_and_validates_the_whole_bound_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("bounded-close.sqlite3"))
        .await
        .unwrap();
    let channels = (0..50)
        .map(|index| format!("room{index}"))
        .collect::<Vec<_>>();
    store.track(channels.clone(), true).await.unwrap();
    let mut ids = Vec::new();
    for channel in channels {
        ids.push(
            store
                .begin_session_at(vec![channel], "2026-01-01T00:00:00Z".into())
                .await
                .unwrap(),
        );
    }
    let commits = Arc::new(AtomicUsize::new(0));
    let counted = commits.clone();
    store
        .call(move |conn| {
            conn.commit_hook(Some(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                false
            }));
            Ok(())
        })
        .await
        .unwrap();
    store.end_sessions(Vec::new()).await.unwrap();
    assert!(matches!(
        store
            .end_sessions(vec![end(ids[0], "oversize", None); 51])
            .await,
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store
            .end_sessions(vec![
                end(ids[0], "valid_first", Some("2026-01-01T00:01:00Z")),
                end(ids[1], "invalid_second", Some("invalid")),
            ])
            .await,
        Err(StoreError::Invalid)
    ));
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert!(session_rows(&store).await.iter().all(|row| row.2.is_none()));
    store
        .end_sessions(
            ids.into_iter()
                .map(|id| end(id, "transport_end", Some("2026-01-01T00:01:00Z")))
                .collect(),
        )
        .await
        .unwrap();
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert!(session_rows(&store)
        .await
        .iter()
        .all(|row| row.2.as_deref() == Some("2026-01-01T00:01:00.000Z")));
}
