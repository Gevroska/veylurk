use super::*;
use crate::store::NewObservation;

#[tokio::test]
async fn stored_names_are_normalized_distinct_sorted_and_include_retained_rows() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("suggestions.sqlite3"))
        .await
        .unwrap();
    store
        .track(vec!["Room_B".into(), "room_a".into(), "room0".into()], true)
        .await
        .unwrap();
    let session = store
        .begin_session_at(
            vec!["room_a".into(), "room_b".into()],
            "2026-01-01T00:00:00Z".into(),
        )
        .await
        .unwrap();
    let mut events: Vec<_> = [
        "Alpine",
        "Alice",
        "al_1",
        "al9",
        "al0",
        "ALICE",
        "_abc",
        "1alice",
        "abcdefghijklmnopqrstuvwxy",
    ]
    .into_iter()
    .map(|account| NewObservation {
        channel: "room_a".into(),
        account: Some(account.into()),
        kind: "join".into(),
        received_at: "2026-01-01T00:00:01Z".into(),
    })
    .collect();
    events.push(NewObservation {
        channel: "room_b".into(),
        account: Some("alice".into()),
        kind: "join".into(),
        received_at: "2026-01-01T00:00:02Z".into(),
    });
    // A part-only account is retained knowledge even without a positive visit.
    events.push(NewObservation {
        channel: "room_b".into(),
        account: Some("al_part".into()),
        kind: "part".into(),
        received_at: "2026-01-01T00:00:03Z".into(),
    });
    store.append(session, events).await.unwrap();
    store
        .update_channel("room_a".into(), false, false)
        .await
        .unwrap();
    store
        .update_channel("room_b".into(), false, true)
        .await
        .unwrap();

    assert_eq!(
        store
            .suggestions(SuggestionKind::Account, " AL ".into(), 10)
            .await
            .unwrap(),
        ["al0", "al9", "al_1", "al_part", "alice", "alpine"]
    );
    for (prefix, expected) in [
        ("al_", vec!["al_1", "al_part"]),
        ("AL9", vec!["al9"]),
        ("_", vec!["_abc"]),
        ("1", vec!["1alice"]),
        (
            "abcdefghijklmnopqrstuvwxy",
            vec!["abcdefghijklmnopqrstuvwxy"],
        ),
        ("alice", vec!["alice"]),
        ("missing", vec![]),
        ("room", vec![]),
    ] {
        assert_eq!(
            store
                .suggestions(SuggestionKind::Account, prefix.into(), 10)
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        store
            .suggestions(SuggestionKind::Account, "al".into(), 2)
            .await
            .unwrap(),
        ["al0", "al9"]
    );
    assert_eq!(
        store
            .suggestions(SuggestionKind::Channel, " ROOM ".into(), 10)
            .await
            .unwrap(),
        ["room0", "room_a", "room_b"]
    );
    assert_eq!(
        store
            .suggestions(SuggestionKind::Channel, "room_".into(), 10)
            .await
            .unwrap(),
        ["room_a", "room_b"]
    );
}

#[tokio::test]
async fn suggestions_validate_before_storage_and_empty_prefix_never_queries() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("empty.sqlite3"))
        .await
        .unwrap();
    // If the empty request touches the source table it fails, exposing an accidental scan.
    store
        .call(|conn| {
            conn.execute("DROP TABLE relationships", [])?;
            Ok(())
        })
        .await
        .unwrap();
    for prefix in ["", " \t "] {
        assert!(store
            .suggestions(SuggestionKind::Account, prefix.into(), 8)
            .await
            .unwrap()
            .is_empty());
    }
    for prefix in ["a%", "a-b", "é", "abcdefghijklmnopqrstuvwxyz"] {
        assert!(matches!(
            store
                .suggestions(SuggestionKind::Account, prefix.into(), 8)
                .await,
            Err(StoreError::Invalid)
        ));
    }
    for limit in [0, 11, usize::MAX] {
        assert!(matches!(
            store
                .suggestions(SuggestionKind::Channel, String::new(), limit)
                .await,
            Err(StoreError::Invalid)
        ));
    }
    assert!(store
        .suggestions(SuggestionKind::Account, "a".into(), 8)
        .await
        .is_err());
}

#[tokio::test]
async fn suggestions_seek_existing_covering_indexes_without_sorting_history() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("plan.sqlite3"))
        .await
        .unwrap();
    store
        .call(|conn| {
            for (sql, expected_index) in [
                (ACCOUNT_SQL, "relationships_account"),
                (CHANNEL_SQL, "sqlite_autoindex_channels_1"),
                (
                    COMMUNITY_ACCOUNT_SQL,
                    "community_members_account_member_sample",
                ),
                (COMMUNITY_CHANNEL_SQL, "community_samples_channel_receipt"),
            ] {
                let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let plan = statement
                    .query_map(params!["al", "al{", 10], |row| row.get::<_, String>(3))?
                    .collect::<Result<Vec<_>, _>>()?
                    .join("\n");
                assert!(plan.contains("SEARCH"), "{plan}");
                assert!(plan.contains(expected_index), "{plan}");
                assert!(plan.contains("COVERING INDEX"), "{plan}");
                assert!(!plan.contains("SCAN"), "{plan}");
                assert!(!plan.contains("TEMP B-TREE"), "{plan}");
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn community_only_names_merge_after_bounded_source_candidates_without_duplicates() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("community-suggestions.sqlite3"))
        .await
        .unwrap();
    store
        .track(vec!["room_a".into(), "room_c".into()], true)
        .await
        .unwrap();
    store.call_write(crate::diagnostics::Operation::CommunityAppend, |conn| {
        let tx = conn.transaction()?;
        tx.execute_batch("INSERT INTO community_runs(id,started_at_utc,build_identity) VALUES(1,'2020-01-01T00:00:00.000Z','fixture');
            INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,outcome) VALUES(1,1,1,'room_b','2020-01-01T00:00:00.000Z','success'),(2,1,2,'room_c','2020-01-01T00:00:00.000Z','success');")?;
        for number in 0..40 {
            let account = format!("ac{number:02}");
            if number%2==0 {
                tx.execute("INSERT INTO relationships(channel,account,first_observed_at,last_observed_at,observation_count) VALUES('room_a',?1,'2020-01-01T00:00:00Z','2020-01-01T00:00:00Z',1)", [&account])?;
            }
            if number<30 {
                tx.execute("INSERT INTO community_members(sample_id,channel,account,roles) VALUES(1,'room_b',?1,32),(2,'room_c',?1,32)", [&account])?;
            }
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    for limit in [1, 3, 10] {
        assert_eq!(
            store
                .suggestions(SuggestionKind::Account, " AC ".into(), limit)
                .await
                .unwrap(),
            (0..limit)
                .map(|number| format!("ac{number:02}"))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        store
            .suggestions(SuggestionKind::Account, "ac01".into(), 10)
            .await
            .unwrap(),
        ["ac01"]
    );
    assert_eq!(
        store
            .suggestions(SuggestionKind::Account, "ac38".into(), 10)
            .await
            .unwrap(),
        ["ac38"]
    );
    assert_eq!(
        store
            .suggestions(SuggestionKind::Channel, "room_".into(), 10)
            .await
            .unwrap(),
        ["room_a", "room_b", "room_c"]
    );
    assert_eq!(
        store
            .suggestions(SuggestionKind::Channel, "room_".into(), 2)
            .await
            .unwrap(),
        ["room_a", "room_b"]
    );
    assert!(store
        .suggestions(SuggestionKind::Account, "missing".into(), 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn suggestions_use_the_cancellable_query_scope_and_next_query_recovers() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("deadline.sqlite3"))
        .await
        .unwrap();
    store
        .call(|conn| {
            conn.execute_batch("WITH RECURSIVE rooms(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM rooms WHERE n<12000) INSERT INTO relationships(channel,account,first_observed_at,last_observed_at,observation_count) SELECT 'room'||n,'alice','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',1 FROM rooms;")?;
            Ok(())
        })
        .await
        .unwrap();
    store.expire_next_query_for_test();
    let error = store
        .suggestions(SuggestionKind::Account, "al".into(), 8)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if code.extended_code == rusqlite::ffi::SQLITE_INTERRUPT
    ));
    assert_eq!(
        store
            .suggestions(SuggestionKind::Account, "al".into(), 8)
            .await
            .unwrap(),
        ["alice"]
    );
}
