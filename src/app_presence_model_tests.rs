use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;

async fn fixture() -> (tempfile::TempDir, Store, Router) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("model-api.sqlite3"))
        .await
        .unwrap();
    let status = crate::collector::status(&store);
    let (channels, _) = watch::channel(Vec::new());
    let app = router(
        AppState {
            store: store.clone(),
            status,
            channels,
            port: 8794,
            configuration: Arc::new(Mutex::new(())),
        },
        Router::new(),
    );
    (directory, store, app)
}

fn request(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn model_api_empty_result_is_explicitly_uncalibrated_and_preserves_storage() {
    let _serial = crate::presence_model::test_serial_guard().await;
    let (_directory, store, app) = fixture().await;
    let before = store.community_stats().await.unwrap();
    let settings = store.community_settings().await.unwrap();
    let response = app
        .oneshot(request(
            "/api/community/presence?account=alice&channel=example&from=2024-01-01T00:00:00Z&to=2024-01-02T00:00:00Z",
            "127.0.0.1:8794",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 32 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["account"], "alice");
    assert_eq!(body["channel"], "example");
    assert_eq!(body["model_version"], "community_presence_v1");
    assert_eq!(body["calibration_status"], "uncalibrated");
    assert_eq!(body["probability_kind"], "model_based_pointwise");
    assert_eq!(body["inference"], "historical_smoother");
    assert_eq!(body["threshold"], 0.99);
    assert_eq!(body["complete"], true);
    assert_eq!(body["status"], "no_data");
    assert!(body["cells"].as_array().unwrap().is_empty());
    assert_eq!(body["effective_range"]["from"], "2024-01-01T00:00:00.000Z");
    assert_eq!(
        store.community_stats().await.unwrap().sample_count,
        before.sample_count
    );
    assert_eq!(store.community_settings().await.unwrap(), settings);
}

#[tokio::test]
async fn model_api_rejects_invalid_queries_and_nonlocal_hosts() {
    let _serial = crate::presence_model::test_serial_guard().await;
    let (_directory, _store, app) = fixture().await;
    for query in [
        "channel=example",
        "account=alice",
        "account=alice&channel=bad-name",
        "account=alice&channel=example&connected_minutes=4",
        "account=alice&channel=example&disconnected_minutes=29",
        "account=alice&channel=example&detection_scale=NaN",
        "account=alice&channel=example&false_report_ratio=Infinity",
        "account=alice&channel=example&lag_seconds=-1",
        "account=alice&channel=example&max_gap_seconds=21601",
        "account=alice&channel=example&from=invalid",
        "account=alice&channel=example&from=2024-01-01T00:00:00Z&to=2024-03-01T00:00:00Z",
        "account=alice&channel=example&unknown_setting=1",
        "account=alice&broadcast_id=0",
    ] {
        let response = app
            .clone()
            .oneshot(request(
                &format!("/api/community/presence?{query}"),
                "127.0.0.1:8794",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "query: {query}");
    }
    let response = app
        .oneshot(request(
            "/api/community/presence?account=alice&channel=example",
            "untrusted.example",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
