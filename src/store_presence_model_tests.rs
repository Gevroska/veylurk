use super::*;
use crate::store::*;

fn query() -> PresenceModelQuery {
    PresenceModelQuery {
        account: "alice".into(),
        channel: Some("room".into()),
        from: Some("2024-01-01T00:00:00Z".into()),
        to: Some("2024-01-01T00:06:00Z".into()),
        ..Default::default()
    }
}

async fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("presence.sqlite3"))
        .await
        .unwrap();
    store.call_write(Operation::CommunityAppend,|conn| {
        conn.execute_batch("INSERT INTO community_runs(id,started_at_utc,build_identity) VALUES(1,'2024-01-01T00:00:00.000Z','fixture');")?;
        let tx=conn.transaction()?;
        for i in 0..6 {
            let at=format!("2024-01-01T00:0{i}:00.000Z");
            tx.execute("INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,completed_at_utc,received_at_utc,outcome,reported_chatters,returned_accounts,broadcasters,staff,moderators,vips,chatbots,viewers,request_after_trigger_verified,request_after_trigger_ms,request_to_response_ms,request_to_finished_ms,finished_minus_response_ms,from_disk_cache,from_service_worker,from_prefetch_cache,served_from_cache_event,encoded_bytes,transport,server_freshness,http_status) VALUES(?1,1,?1,'room',?2,?2,?2,'success',100,2,0,0,0,0,0,2,1,10,50,40,-10,0,0,0,0,100,'network_response','unknown',200)",params![i+1,at])?;
            tx.execute("INSERT INTO community_members(sample_id,channel,account,roles) VALUES(?1,'room',?2,32),(?1,'room','bob',32)",params![i+1,if i==2{"carol"}else{"alice"}])?;
        }
        tx.commit()?;Ok(())
    }).await.unwrap();
    (directory, store)
}

#[tokio::test]
async fn all_successes_include_misses_and_internal_failures_without_using_receipt_pages() {
    let _serial = test_serial_guard().await;
    let (_directory, store) = fixture().await;
    let input = store
        .call_read(Operation::Query, |conn| {
            extract(
                conn,
                "room",
                "alice",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:06:00.000Z",
                &AtomicBool::new(false),
            )
        })
        .await
        .unwrap();
    assert_eq!(input.successful, 6);
    assert_eq!(input.samples.len(), 6);
    assert_eq!(input.samples[2].target_roles, None);
    assert_eq!(input.samples[2].viewer_only, 2);
    let result = store.community_presence(query()).await.unwrap();
    assert_eq!(result.status, PresenceStatus::Ok);
    assert_eq!(result.sample_counts.eligible_observations, 6);
    assert_eq!(result.sample_counts.target_positive_receipts, 5);
    store.call_write(Operation::CommunityAppend,|conn| {conn.execute_batch("DELETE FROM community_members WHERE sample_id=3;UPDATE community_samples SET outcome='failed',received_at_utc=NULL WHERE id=3;")?;Ok(())}).await.unwrap();
    let result = store.community_presence(query()).await.unwrap();
    assert_eq!(result.sample_counts.attempts_in_id_span, 6);
    assert_eq!(result.excluded_reason_counts["non_success"], 1);
    assert_eq!(result.sample_counts.modeled_segments, 1);
}

#[tokio::test]
async fn context_and_broadcast_projection_match_the_same_channel_axis() {
    let _serial = test_serial_guard().await;
    let (_directory, store) = fixture().await;
    crate::metadata::initialize(store.clone()).await.unwrap();
    store.call_write(Operation::MetadataWrite,|conn| {conn.execute_batch("INSERT INTO metadata_broadcasts(id,stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at) VALUES(1,'1','room','','','en','2024-01-01T00:01:00.000Z','2024-01-01T00:01:00.000Z','2024-01-01T00:03:00.000Z');")?;Ok(())}).await.unwrap();
    let selected = store
        .community_presence(PresenceModelQuery {
            broadcast_id: Some(1),
            from: None,
            to: None,
            channel: None,
            ..query()
        })
        .await
        .unwrap();
    let channel = store
        .community_presence(PresenceModelQuery {
            from: Some(selected.effective_range.from.clone()),
            to: Some(selected.effective_range.to.clone()),
            ..query()
        })
        .await
        .unwrap();
    assert_eq!(selected.effective_range.from, "2024-01-01T00:01:00.000Z");
    assert_eq!(selected.effective_range.to, "2024-01-01T00:03:00.001Z");
    assert_eq!(selected.context_range.from, "2023-12-31T18:01:00.000Z");
    assert_eq!(
        serde_json::to_value(selected.cells).unwrap(),
        serde_json::to_value(channel.cells).unwrap()
    );
    assert!(matches!(
        store
            .community_presence(PresenceModelQuery {
                channel: Some("other".into()),
                broadcast_id: Some(1),
                ..query()
            })
            .await,
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store
            .community_presence(PresenceModelQuery {
                from: Some("2023-01-01T00:00:00Z".into()),
                ..query()
            })
            .await,
        Err(StoreError::Invalid)
    ));
    assert!(matches!(
        store
            .community_presence(PresenceModelQuery {
                channel: None,
                ..query()
            })
            .await,
        Err(StoreError::Invalid)
    ));
}

#[tokio::test]
async fn sql_plans_seek_time_primary_key_and_sample_member_indexes() {
    let _serial = test_serial_guard().await;
    let (_directory, store) = fixture().await;
    store
        .call_read(Operation::Query, |conn| {
            let mut success = conn.prepare(&format!("EXPLAIN QUERY PLAN {SUCCESS_SQL}"))?;
            let plans = success
                .query_map(
                    params![
                        "room",
                        "2024-01-01T00:00:00.000Z",
                        "2024-01-01T00:06:00.000Z",
                        50001
                    ],
                    |r| r.get::<_, String>(3),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("community_samples_channel_receipt")
                        && p.contains("channel=? AND received_at_utc>?")),
                "{plans:?}"
            );
            assert!(
                !plans.iter().any(|p| p.contains("TEMP B-TREE")),
                "{plans:?}"
            );
            let mut attempts = conn.prepare(&format!("EXPLAIN QUERY PLAN {ATTEMPT_SQL}"))?;
            let plans = attempts
                .query_map(params![1, 6, "room", "alice"], |r| r.get::<_, String>(3))?
                .collect::<Result<Vec<_>, _>>()?;
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH s USING INTEGER PRIMARY KEY")),
                "{plans:?}"
            );
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("sample_id=? AND account=?")),
                "{plans:?}"
            );
            let mut members = conn.prepare(&format!("EXPLAIN QUERY PLAN {MEMBER_COUNTS_SQL}"))?;
            let plans = members
                .query_map([1], |r| r.get::<_, String>(3))?
                .collect::<Result<Vec<_>, _>>()?;
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("community_members_sample_member (sample_id=?)")),
                "{plans:?}"
            );
            Ok(())
        })
        .await
        .unwrap();
}

fn capped_fixture(conn: &mut Connection, count: i64, returned: i64) -> Result<(), StoreError> {
    conn.execute("INSERT INTO community_runs(id,started_at_utc,build_identity) VALUES(1,'2024-01-01T00:00:00.000Z','cap')",[])?;
    conn.execute("WITH RECURSIVE n(x) AS(VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<?1) INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,received_at_utc,outcome,returned_accounts) SELECT x,1,x,'room','2024-01-01T00:00:00.000Z','2024-01-01T00:00:01.000Z','success',?2 FROM n",params![count,returned])?;
    Ok(())
}

#[tokio::test]
async fn input_caps_and_sql_interruption_do_not_return_partial_model_results() {
    let _serial = test_serial_guard().await;
    for (count, returned) in [(50001, 0), (1221, 4096)] {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("cap.sqlite3"))
            .await
            .unwrap();
        store
            .call_write(Operation::CommunityAppend, move |conn| {
                capped_fixture(conn, count, returned)
            })
            .await
            .unwrap();
        let result = store.community_presence(query()).await.unwrap();
        assert_eq!(result.status, PresenceStatus::InputLimit);
        assert!(!result.complete && result.cells.is_empty());
    }
    let (_directory, store) = fixture().await;
    // A sparse source ID expands the bounded traversal even when few rows exist.
    store.call_write(Operation::CommunityAppend,|conn| {conn.execute("INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,received_at_utc,outcome,returned_accounts) VALUES(200001,1,200001,'room','2024-01-01T00:00:00.000Z','2024-01-01T00:04:00.000Z','success',0)",[])?;Ok(())}).await.unwrap();
    assert_eq!(
        store.community_presence(query()).await.unwrap().status,
        PresenceStatus::InputLimit
    );
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("cancel.sqlite3"))
        .await
        .unwrap();
    store
        .call_write(Operation::CommunityAppend, |conn| {
            capped_fixture(conn, 2000, 0)
        })
        .await
        .unwrap();
    store.expire_next_query_for_test();
    // Exercise extraction directly so the light viewport read cannot consume the injected expiry.
    let result = store
        .call_read(Operation::Query, |conn| {
            extract(
                conn,
                "room",
                "alice",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:06:00.000Z",
                &AtomicBool::new(false),
            )
        })
        .await;
    assert!(result.is_err());
    store
        .set_community_pause(Some(CommunityPauseReason::SourceDenied))
        .await
        .unwrap();
    assert_eq!(
        store.community_pause().await.unwrap(),
        Some(CommunityPauseReason::SourceDenied)
    );
}

#[tokio::test]
async fn busy_admission_and_pre_cancelled_extraction_are_bounded_and_empty() {
    let _serial = test_serial_guard().await;
    let (_directory, store) = fixture().await;
    let permit = try_permit().unwrap();
    let response = store.community_presence(query()).await.unwrap();
    assert_eq!(response.status, PresenceStatus::AnalysisBusy);
    assert!(!response.complete && response.cells.is_empty());
    drop(permit);
    let extraction = store
        .call_read(Operation::Query, |conn| {
            extract(
                conn,
                "room",
                "alice",
                "2024-01-01T00:00:00.000Z",
                "2024-01-01T00:06:00.000Z",
                &AtomicBool::new(true),
            )
        })
        .await
        .unwrap();
    assert!(extraction.cancelled && extraction.samples.is_empty());
}
