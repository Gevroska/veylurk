use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use tower::ServiceExt;

async fn fixture() -> (tempfile::TempDir, Store, Metadata) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("community-metadata.sqlite3"))
        .await
        .unwrap();
    let metadata = initialize(store.clone()).await.unwrap();
    store.track(vec!["room".into()], true).await.unwrap();
    let session = store
        .begin_session_at(vec!["room".into()], "2024-01-01T00:00:00Z".into())
        .await
        .unwrap();
    store
        .append(
            session,
            [
                ("prior", "00:09:00"),
                ("both", "00:11:00"),
                ("irc_only", "00:12:00"),
            ]
            .into_iter()
            .map(|(account, time)| store::NewObservation {
                channel: "room".into(),
                account: Some(account.into()),
                kind: "join".into(),
                received_at: format!("2024-01-01T{time}Z"),
            })
            .collect(),
        )
        .await
        .unwrap();
    store.call_write(Operation::MetadataWrite,|conn| {
        let tx=conn.transaction()?;
        for (id,channel,start,end) in [(1,"room","00:10:00","00:20:00"),(2,"other","00:10:00","00:20:00"),(3,"room","00:30:00","00:40:00"),(4,"empty","00:10:00","00:20:00")] {
            tx.execute("INSERT INTO metadata_broadcasts(id,stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at,offline_observed_at) VALUES(?1,?2,?3,'Fixture','','en',?4,?4,?5,'2024-01-01T01:00:00.000Z')",params![id,id.to_string(),channel,format!("2024-01-01T{start}.000Z"),format!("2024-01-01T{end}.000Z")])?;
        }
        tx.execute("INSERT INTO community_runs(id,started_at_utc,build_identity) VALUES(1,'2024-01-01T00:00:00.000Z','fixture')",[])?;
        for (id,channel,time,accounts) in [
            (1,"room","00:09:59.999",vec!["too_early"]),
            (2,"room","00:10:00.000",vec!["cm_only","both"]),
            (3,"room","00:15:00.000",vec!["cm_only","both"]),
            (4,"room","00:20:00.000",vec!["last_edge"]),
            (5,"room","00:20:00.001",vec!["too_late"]),
            (6,"other","00:12:00.000",vec!["cm_only"]),
            (7,"room","00:35:00.000",vec!["cm_only"]),
            (8,"unlisted","00:12:00.000",vec!["wrong_channel"]),
        ] {
            tx.execute("INSERT INTO community_samples(id,run_id,job_id,channel,started_at_utc,received_at_utc,outcome) VALUES(?1,1,?1,?2,'2024-01-01T00:00:00.000Z',?3,'success')",params![id,channel,format!("2024-01-01T{time}Z")])?;
            for account in accounts {
                tx.execute("INSERT INTO community_members(sample_id,channel,account,roles) VALUES(?1,?2,?3,32)",params![id,channel,account])?;
            }
        }
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    (directory, store, metadata)
}

async fn get(metadata: &Metadata, uri: &str) -> Value {
    let response = metadata
        .router()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}

fn broadcast_ids(value: &Value) -> Vec<i64> {
    value["broadcasts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn community_broadcast_search_is_source_filtered_deduplicated_and_cursor_stable() {
    let (_directory, _store, metadata) = fixture().await;
    let all = get(&metadata, "/api/broadcasts?account=%20CM_ONLY%20").await;
    assert_eq!(all["source"], "all");
    assert_eq!(broadcast_ids(&all), [3, 2, 1]);
    assert!(
        broadcast_ids(&get(&metadata, "/api/broadcasts?account=cm_only&source=irc").await)
            .is_empty()
    );
    assert_eq!(
        broadcast_ids(
            &get(
                &metadata,
                "/api/broadcasts?account=cm_only&source=community"
            )
            .await
        ),
        [3, 2, 1]
    );
    assert_eq!(
        broadcast_ids(
            &get(
                &metadata,
                "/api/broadcasts?account=cm_only&channel=ROOM&source=community"
            )
            .await
        ),
        [3, 1]
    );
    let first = get(
        &metadata,
        "/api/broadcasts?account=cm_only&source=community&limit=1",
    )
    .await;
    assert_eq!(broadcast_ids(&first), [3]);
    assert_eq!(first["next_before"], 3);
    let second = get(
        &metadata,
        "/api/broadcasts?account=cm_only&source=community&limit=1&before=3",
    )
    .await;
    assert_eq!(broadcast_ids(&second), [2]);
    assert_eq!(second["next_before"], 2);
    let third = get(
        &metadata,
        "/api/broadcasts?account=cm_only&source=community&limit=1&before=2",
    )
    .await;
    assert_eq!(broadcast_ids(&third), [1]);
    assert!(third["next_before"].is_null());
    assert_eq!(
        broadcast_ids(
            &get(
                &metadata,
                "/api/broadcasts?account=both&channel=room&to=2024-01-01T00:21:00Z"
            )
            .await
        ),
        [1]
    );
    assert_eq!(
        broadcast_ids(&get(&metadata, "/api/broadcasts?account=prior&source=irc").await),
        [3, 1]
    );
    assert!(
        broadcast_ids(&get(&metadata, "/api/broadcasts?account=prior&source=community").await)
            .is_empty()
    );
    assert_eq!(
        broadcast_ids(&get(&metadata, "/api/broadcasts?source=community").await),
        [3, 2, 1]
    );
    assert_eq!(
        broadcast_ids(&get(&metadata, "/api/broadcasts?source=irc").await),
        [4, 3, 2, 1]
    );
}

#[tokio::test]
async fn community_associations_clip_to_channel_and_half_open_receipt_window() {
    let (_directory, _store, metadata) = fixture().await;
    for account in ["too_early", "too_late", "wrong_channel"] {
        assert!(broadcast_ids(
            &get(
                &metadata,
                &format!("/api/broadcasts?source=community&account={account}")
            )
            .await
        )
        .is_empty());
    }
    assert_eq!(
        broadcast_ids(
            &get(
                &metadata,
                "/api/broadcasts?source=community&account=last_edge"
            )
            .await
        ),
        [1]
    );
    assert_eq!(broadcast_ids(&get(&metadata,"/api/broadcasts?source=community&account=last_edge&from=2024-01-01T00:20:00Z&to=2024-01-01T00:20:00.001Z").await),[1]);
    assert!(broadcast_ids(
        &get(
            &metadata,
            "/api/broadcasts?source=community&account=last_edge&to=2024-01-01T00:20:00Z"
        )
        .await
    )
    .is_empty());
    assert_eq!(broadcast_ids(&get(&metadata,"/api/broadcasts?source=community&account=cm_only&from=2024-01-01T00:15:00Z&to=2024-01-01T00:20:00Z").await),[1]);
    let account=get(&metadata,"/api/broadcasts/1/accounts?source=community&account=cm_only&from=2024-01-01T01:15:00%2B01:00&to=2024-01-01T00:20:00Z").await;
    assert_eq!(account["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(account["accounts"][0]["community_receipt_count"], 1);
    assert_eq!(
        account["accounts"][0]["community_first_received_at"],
        "2024-01-01T00:15:00.000Z"
    );
    let disjoint = get(
        &metadata,
        "/api/broadcasts/1/accounts?source=community&from=2024-01-01T00:20:00.001Z",
    )
    .await;
    assert!(disjoint["accounts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn combined_accounts_keep_irc_counts_and_receipt_timestamps_independent() {
    let (_directory, _store, metadata) = fixture().await;
    let irc = get(&metadata, "/api/broadcasts/1/accounts?source=irc").await;
    let all = get(&metadata, "/api/broadcasts/1/accounts").await;
    let rows = all["accounts"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row["account"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["both", "cm_only", "irc_only", "last_edge", "prior"]
    );
    let cm = rows.iter().find(|row| row["account"] == "cm_only").unwrap();
    assert_eq!(cm["community_receipt_count"], 2);
    assert_eq!(
        cm["community_first_received_at"],
        "2024-01-01T00:10:00.000Z"
    );
    assert_eq!(cm["community_last_received_at"], "2024-01-01T00:15:00.000Z");
    assert_eq!(cm["sources"], json!(["community"]));
    assert_eq!(cm["positive_observation_count"], 0);
    assert_eq!(cm["visit_count"], 0);
    assert!(
        cm["first_observed_at"].is_null()
            && cm["last_observed_at"].is_null()
            && cm["prior_observed_at"].is_null()
    );
    assert_eq!(cm["association_basis"], "community_receipts");
    assert_eq!(cm["state"], "observed");
    let both = rows.iter().find(|row| row["account"] == "both").unwrap();
    assert_eq!(both["sources"], json!(["irc", "community"]));
    assert_eq!(both["community_receipt_count"], 2);
    assert_eq!(both["positive_observation_count"], 1);
    for old in irc["accounts"].as_array().unwrap() {
        let current = rows
            .iter()
            .find(|row| row["account"] == old["account"])
            .unwrap();
        for field in [
            "positive_observation_count",
            "visit_count",
            "first_observed_at",
            "last_observed_at",
            "prior_observed_at",
        ] {
            assert_eq!(current[field], old[field], "{field}");
        }
        assert_eq!(old["community_receipt_count"], 0);
        assert_eq!(old["sources"], json!(["irc"]));
    }
    for field in ["positive_observation_count", "visit_count"] {
        assert_eq!(all["summary"][field], irc["summary"][field]);
    }
    assert_eq!(all["summary"]["account_count"], 5);
    assert_eq!(all["summary"]["observed_account_count"], 4);
    assert_eq!(all["summary"]["inferred_only_account_count"], 1);
    assert_eq!(all["summary"]["community_account_count"], 3);
    assert_eq!(all["summary"]["community_receipt_count"], 5);
    let community = get(&metadata, "/api/broadcasts/1/accounts?source=community").await;
    assert_eq!(community["accounts"].as_array().unwrap().len(), 3);
    assert_eq!(community["summary"]["positive_observation_count"], 0);
    assert_eq!(community["summary"]["visit_count"], 0);
    assert!(community["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["sources"] == json!(["community"])));
    let mut paged = Vec::new();
    for offset in [0, 2, 4] {
        let page = get(
            &metadata,
            &format!("/api/broadcasts/1/accounts?limit=2&offset={offset}"),
        )
        .await;
        paged.extend(
            page["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["account"].as_str().unwrap().to_string()),
        );
        assert_eq!(page["has_more"], offset < 4);
        assert_eq!(
            page["next_offset"],
            if offset < 4 {
                json!(offset + 2)
            } else {
                Value::Null
            }
        );
    }
    assert_eq!(paged, ["both", "cm_only", "irc_only", "last_edge", "prior"]);
}

#[tokio::test]
async fn broadcast_routes_reject_unknown_source_and_preserve_missing_broadcast_status() {
    let (_directory, _store, metadata) = fixture().await;
    for uri in [
        "/api/broadcasts?source=unknown",
        "/api/broadcasts/1/accounts?source=unknown",
        "/api/broadcasts?before=0",
        "/api/broadcasts?source=community&from=2024-01-02T00:00:00Z&to=2024-01-01T00:00:00Z",
    ] {
        let response = metadata
            .router()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    let response = metadata
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/broadcasts/999/accounts?source=community")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
