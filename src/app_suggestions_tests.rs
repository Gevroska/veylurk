use super::*;
use axum::{body::Body, http::Request};
use tower::ServiceExt;

async fn fixture() -> (tempfile::TempDir, Router) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("api.sqlite3"))
        .await
        .unwrap();
    store
        .call(|conn| {
            for suffix in 0..13 {
                conn.execute(
                    "INSERT INTO relationships(channel,account,first_observed_at,last_observed_at,observation_count) VALUES ('retained',?1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z',1)",
                    [format!("alice{suffix:02}")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    store.track(vec!["paused".into()], false).await.unwrap();
    let status = crate::collector::status(&store);
    let (channels, _) = watch::channel(Vec::new());
    let app = router(
        AppState {
            store,
            status,
            channels,
            port: 8794,
            configuration: Arc::new(Mutex::new(())),
        },
        Router::new(),
    );
    (directory, app)
}

fn request(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn suggestions_api_defaults_to_eight_caps_at_ten_and_preserves_security_headers() {
    let (_directory, app) = fixture().await;
    for (suffix, expected_count) in [("", 8), ("&limit=1", 1), ("&limit=10", 10)] {
        let response = app
            .clone()
            .oneshot(request(
                &format!("/api/suggestions?kind=account&prefix=%20AL%20{suffix}"),
                "localhost:8794",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY));
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["kind"], "account");
        let rows = body["suggestions"].as_array().unwrap();
        assert_eq!(rows.len(), expected_count);
        assert_eq!(rows[0], "alice00");
        assert_eq!(
            rows[expected_count - 1],
            format!("alice{:02}", expected_count - 1)
        );
    }
    for (prefix, expected) in [("", json!([])), ("p", json!(["paused"]))] {
        let response = app
            .clone()
            .oneshot(request(
                &format!("/api/suggestions?kind=channel&prefix={prefix}"),
                "127.0.0.1:8794",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({"kind":"channel","suggestions":expected}));
    }
    for host in ["evil.example", "127.0.0.1:1", "localhost", "[::1]:8794"] {
        let response = app
            .clone()
            .oneshot(request("/api/suggestions?kind=account&prefix=a", host))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/suggestions?kind=account&prefix=a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn suggestions_api_rejects_invalid_queries_with_bounded_json_errors() {
    let (_directory, app) = fixture().await;
    let mut queries = vec![
        "kind=other&prefix=a".to_owned(),
        "kind=Account&prefix=a".to_owned(),
        "prefix=a".to_owned(),
        "kind=account".to_owned(),
        "kind=account&prefix=a&unknown=1".to_owned(),
        "kind=account&kind=channel&prefix=a".to_owned(),
        "kind=account&prefix=a&prefix=b".to_owned(),
        "kind=account&prefix=a&limit=1&limit=2".to_owned(),
    ];
    for prefix in ["a%25", "a-b", "%C3%A9", "abcdefghijklmnopqrstuvwxyz"] {
        queries.push(format!("kind=account&prefix={prefix}"));
    }
    for limit in ["0", "11", "-1", "many", "", "9999999999999999999999999999"] {
        queries.push(format!("kind=account&prefix=a&limit={limit}"));
    }
    queries.push(format!("kind={}&prefix=a", "private_input".repeat(500)));
    for query in queries {
        let response = app
            .clone()
            .oneshot(request(
                &format!("/api/suggestions?{query}"),
                "127.0.0.1:8794",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = axum::body::to_bytes(response.into_body(), 512)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "invalid_query");
        assert!(!String::from_utf8_lossy(&bytes).contains("private_input"));
    }
}
