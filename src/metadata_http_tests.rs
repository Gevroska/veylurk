//! Offline HTTP contract tests; every request stays on a temporary loopback server.
use super::*;
use axum::{body::Body, extract::Request, http::HeaderValue, response::Response};
use std::{collections::VecDeque, sync::Mutex};

struct Reply {
    path: &'static str,
    status: StatusCode,
    body: Value,
    retry_after: Option<&'static str>,
}
impl Reply {
    fn ok(path: &'static str, body: Value) -> Self {
        Self {
            path,
            status: StatusCode::OK,
            body,
            retry_after: None,
        }
    }
    fn status(path: &'static str, status: StatusCode) -> Self {
        Self {
            status,
            ..Self::ok(path, json!({"message":"untrusted remote detail"}))
        }
    }
}

#[derive(Clone)]
struct FixtureState {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}
struct RecordedRequest {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    authorization: String,
    client_id: String,
    body: String,
}
struct Fixture {
    state: FixtureState,
    endpoints: Endpoints,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = FixtureState {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .fallback(fixture_request)
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            endpoints: Endpoints {
                token: format!("{base}/token"),
                validate: format!("{base}/validate"),
                users: format!("{base}/users"),
                streams: format!("{base}/streams"),
            },
            task,
        }
    }
    fn assert_paths(&self, expected: &[&str]) {
        assert!(self.state.replies.lock().unwrap().is_empty());
        let requests = self.state.requests.lock().unwrap();
        let paths: Vec<_> = requests.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, expected);
        for request in requests.iter() {
            if request.path == "/token" {
                assert_eq!(request.method, "POST");
                assert!(request.authorization.is_empty());
                let fields = reqwest::Url::parse(&format!("http://fixture/?{}", request.body))
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect::<HashMap<_, _>>();
                assert_eq!(fields.get("client_id").unwrap(), "fixture-client");
                assert_eq!(fields.get("client_secret").unwrap(), "fixture-secret");
                assert_eq!(fields.get("grant_type").unwrap(), "client_credentials");
            } else {
                assert_eq!(request.method, "GET");
                assert!(request.authorization.starts_with("Bearer fixture-token"));
                assert!(request.body.is_empty());
                if request.path != "/validate" {
                    assert_eq!(request.client_id, "fixture-client");
                    let key = if request.path == "/users" {
                        "login"
                    } else {
                        "user_login"
                    };
                    assert!(request
                        .query
                        .iter()
                        .any(|(k, v)| k == key && v == "example"));
                    if request.path == "/streams" {
                        assert!(request.query.contains(&("first".into(), "100".into())));
                    }
                }
            }
        }
    }
}

async fn fixture_request(State(state): State<FixtureState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    };
    let query = reqwest::Url::parse(&format!("http://fixture{}", parts.uri))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let recorded = RecordedRequest {
        method: parts.method.to_string(),
        path: path.clone(),
        query,
        authorization: header("authorization"),
        client_id: header("client-id"),
        body: String::from_utf8(axum::body::to_bytes(body, 8192).await.unwrap().to_vec()).unwrap(),
    };
    state.requests.lock().unwrap().push(recorded);
    let reply = state.replies.lock().unwrap().pop_front();
    let Some(reply) = reply else {
        return Response::builder().status(500).body(Body::empty()).unwrap();
    };
    assert_eq!(path, reply.path, "unexpected metadata request order");
    let mut response = Response::builder()
        .status(reply.status)
        .header("content-type", "application/json")
        .body(Body::from(reply.body.to_string()))
        .unwrap();
    if let Some(retry) = reply.retry_after {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static(retry));
    }
    response
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
fn credentials() -> Credentials {
    Credentials {
        client_id: "fixture-client".into(),
        secret: "fixture-secret".into(),
    }
}
fn cached_token() -> Option<Token> {
    Some(Token {
        value: "fixture-token-cached".into(),
        expires: Instant::now() + Duration::from_secs(7200),
        validated: Instant::now(),
    })
}
fn token_reply(value: &str) -> Reply {
    Reply::ok(
        "/token",
        json!({"access_token":value,"token_type":"bearer","expires_in":7200}),
    )
}
fn validation() -> Reply {
    Reply::ok(
        "/validate",
        json!({"client_id":"fixture-client","expires_in":7200}),
    )
}
fn users() -> Reply {
    Reply::ok("/users", json!({"data":[{"id":"42","login":"example"}]}))
}
fn streams(data: Value) -> Reply {
    Reply::ok("/streams", json!({"data":data,"pagination":{}}))
}
fn stream_page(data: Value, cursor: &str) -> Reply {
    Reply::ok(
        "/streams",
        json!({"data":data,"pagination":{"cursor":cursor}}),
    )
}
fn live() -> Value {
    json!({"id":"1234","user_id":"42","user_login":"example","type":"live","title":"Fixture","game_name":"Game","language":"en","started_at":"2026-10-23T07:00:00Z","viewer_count":123})
}

#[tokio::test]
async fn initial_token_is_validated_and_reused_for_filtered_requests() {
    let fixture = Fixture::new(vec![
        token_reply("fixture-token-one"),
        validation(),
        users(),
        streams(json!([live()])),
        users(),
        streams(json!([])),
    ])
    .await;
    let mut token = None;
    let channels = vec!["example".into()];
    let client = client();
    let first = fetch_streams(
        &client,
        &credentials(),
        &mut token,
        &channels,
        &fixture.endpoints,
    )
    .await
    .unwrap_or_else(|error| panic!("first poll failed: {}", error.code));
    assert_eq!(first.streams.len(), 1);
    assert_eq!(first.users.get("example").map(String::as_str), Some("42"));
    let second = fetch_streams(
        &client,
        &credentials(),
        &mut token,
        &channels,
        &fixture.endpoints,
    )
    .await
    .unwrap_or_else(|error| panic!("second poll failed: {}", error.code));
    assert!(second.streams.is_empty());
    fixture.assert_paths(&[
        "/token",
        "/validate",
        "/users",
        "/streams",
        "/users",
        "/streams",
    ]);
}

#[tokio::test]
async fn unauthorized_streams_renews_once_then_retries_both_filtered_queries() {
    let fixture = Fixture::new(vec![
        users(),
        Reply::status("/streams", StatusCode::UNAUTHORIZED),
        token_reply("fixture-token-two"),
        validation(),
        users(),
        streams(json!([live()])),
    ])
    .await;
    let mut token = cached_token();
    let result = fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &["example".into()],
        &fixture.endpoints,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(token.as_ref().unwrap().value, "fixture-token-two");
    fixture.assert_paths(&[
        "/users",
        "/streams",
        "/token",
        "/validate",
        "/users",
        "/streams",
    ]);
    let requests = fixture.state.requests.lock().unwrap();
    assert_eq!(requests[1].authorization, "Bearer fixture-token-cached");
    assert_eq!(requests[5].authorization, "Bearer fixture-token-two");
}

#[tokio::test]
async fn users_and_streams_share_one_unauthorized_renewal_budget() {
    let fixture = Fixture::new(vec![
        users(),
        Reply::status("/streams", StatusCode::UNAUTHORIZED),
        token_reply("fixture-token-two"),
        validation(),
        Reply::status("/users", StatusCode::UNAUTHORIZED),
    ])
    .await;
    let mut token = cached_token();
    let error = fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &["example".into()],
        &fixture.endpoints,
    )
    .await;
    let Err(error) = error else {
        panic!("second 401 must fail");
    };
    assert_eq!(
        error.code,
        "Twitch rejected the configured app credentials."
    );
    assert!(token.is_none());
    fixture.assert_paths(&["/users", "/streams", "/token", "/validate", "/users"]);
}

#[tokio::test]
async fn rate_limit_returns_wait_without_token_renewal_or_immediate_retry() {
    let mut limited = Reply::status("/streams", StatusCode::TOO_MANY_REQUESTS);
    limited.retry_after = Some("137");
    let fixture = Fixture::new(vec![users(), limited]).await;
    let mut token = cached_token();
    let error = fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &["example".into()],
        &fixture.endpoints,
    )
    .await;
    let Err(error) = error else {
        panic!("429 must wait");
    };
    assert_eq!(error.retry_after, Duration::from_secs(137));
    assert!(!error.code.contains("untrusted remote detail"));
    assert_eq!(token.as_ref().unwrap().value, "fixture-token-cached");
    fixture.assert_paths(&["/users", "/streams"]);
}

#[tokio::test]
async fn two_live_rows_with_cursor_and_empty_terminal_page_preserve_both_streams() {
    let mut other = live();
    other["id"] = json!("5678");
    other["user_id"] = json!("43");
    other["user_login"] = json!("second");
    let fixture = Fixture::new(vec![
        Reply::ok(
            "/users",
            json!({"data":[{"id":"42","login":"example"},{"id":"43","login":"second"}]}),
        ),
        stream_page(json!([live(), other]), "next+/=page"),
        streams(json!([])),
    ])
    .await;
    let channels = vec!["example".into(), "second".into()];
    let poll = fetch_streams(
        &client(),
        &credentials(),
        &mut cached_token(),
        &channels,
        &fixture.endpoints,
    )
    .await
    .unwrap_or_else(|error| panic!("complete paginated poll failed: {}", error.code));
    assert_eq!(poll.streams.len(), 2);
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    record_check(&mut conn, &channels, "2026-10-23T08:00:00.000Z", Ok(poll)).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM metadata_checks WHERE state='live'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    fixture.assert_paths(&["/users", "/streams", "/streams"]);
    let requests = fixture.state.requests.lock().unwrap();
    for request in &requests[1..] {
        let logins: Vec<_> = request
            .query
            .iter()
            .filter(|(key, _)| key == "user_login")
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(logins, ["example", "second"]);
    }
    assert!(!requests[1].query.iter().any(|(key, _)| key == "after"));
    assert!(requests[2]
        .query
        .contains(&("after".into(), "next+/=page".into())));
}

#[tokio::test]
async fn continuation_failure_discards_partial_live_rows_and_records_unknown() {
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let mut failure = Reply::status("/streams", status);
        if status == StatusCode::TOO_MANY_REQUESTS {
            failure.retry_after = Some("137");
        }
        let fixture =
            Fixture::new(vec![users(), stream_page(json!([live()]), "next"), failure]).await;
        let mut token = cached_token();
        let result = fetch_streams(
            &client(),
            &credentials(),
            &mut token,
            &["example".into()],
            &fixture.endpoints,
        )
        .await;
        let Err(error) = result else {
            panic!("partial pages must not become a successful poll");
        };
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(error.retry_after, Duration::from_secs(137));
        }
        assert_eq!(token.as_ref().unwrap().value, "fixture-token-cached");
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        record_check(
            &mut conn,
            &["example".into()],
            "2026-10-23T08:00:00.000Z",
            Err(error.code),
        )
        .unwrap();
        assert_eq!(
            conn.query_row("SELECT state FROM metadata_checks", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "unknown"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM metadata_broadcasts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        fixture.assert_paths(&["/users", "/streams", "/streams"]);
    }
}

#[tokio::test]
async fn continuation_unauthorized_restarts_entire_poll_once_without_old_rows_or_cursor() {
    let fixture = Fixture::new(vec![
        users(),
        stream_page(json!([live()]), "old-cursor"),
        Reply::status("/streams", StatusCode::UNAUTHORIZED),
        token_reply("fixture-token-renewed"),
        validation(),
        users(),
        streams(json!([])),
    ])
    .await;
    let mut token = cached_token();
    let poll = fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &["example".into()],
        &fixture.endpoints,
    )
    .await
    .unwrap_or_else(|error| panic!("renewed poll failed: {}", error.code));
    assert!(poll.streams.is_empty());
    fixture.assert_paths(&[
        "/users",
        "/streams",
        "/streams",
        "/token",
        "/validate",
        "/users",
        "/streams",
    ]);
    let requests = fixture.state.requests.lock().unwrap();
    assert!(requests[2]
        .query
        .contains(&("after".into(), "old-cursor".into())));
    assert!(!requests[6].query.iter().any(|(key, _)| key == "after"));
    assert_eq!(requests[6].authorization, "Bearer fixture-token-renewed");
}

#[tokio::test]
async fn continuation_unauthorized_uses_shared_single_renewal_budget() {
    let fixture = Fixture::new(vec![
        users(),
        stream_page(json!([live()]), "first-cursor"),
        Reply::status("/streams", StatusCode::UNAUTHORIZED),
        token_reply("fixture-token-renewed"),
        validation(),
        users(),
        stream_page(json!([live()]), "second-cursor"),
        Reply::status("/streams", StatusCode::UNAUTHORIZED),
    ])
    .await;
    let mut token = cached_token();
    let result = fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &["example".into()],
        &fixture.endpoints,
    )
    .await;
    let Err(error) = result else {
        panic!("a second continuation 401 must fail");
    };
    assert_eq!(
        error.code,
        "Twitch rejected the configured app credentials."
    );
    assert!(token.is_none());
    fixture.assert_paths(&[
        "/users",
        "/streams",
        "/streams",
        "/token",
        "/validate",
        "/users",
        "/streams",
        "/streams",
    ]);
}

#[tokio::test]
async fn pagination_loops_oversized_cursors_and_page_limit_fail_with_bounded_requests() {
    for cursors in [
        vec!["repeat".to_string(), "repeat".to_string()],
        vec!["x".repeat(MAX_CURSOR_BYTES + 1)],
        (0..MAX_STREAM_PAGES)
            .map(|index| format!("page-{index}"))
            .collect(),
    ] {
        let mut replies = vec![users()];
        replies.extend(
            cursors
                .iter()
                .map(|cursor| stream_page(json!([live()]), cursor)),
        );
        let fixture = Fixture::new(replies).await;
        let result = fetch_streams(
            &client(),
            &credentials(),
            &mut cached_token(),
            &["example".into()],
            &fixture.endpoints,
        )
        .await;
        let Err(error) = result else {
            panic!("unterminated pagination must fail");
        };
        assert_eq!(error.code, "Twitch returned an incomplete metadata batch.");
        let mut paths = vec!["/users"];
        paths.extend(std::iter::repeat_n("/streams", cursors.len()));
        fixture.assert_paths(&paths);
    }
}

#[tokio::test]
async fn cross_page_duplicates_allow_mutable_fields_only_for_the_same_identity() {
    let mut duplicate = live();
    duplicate["title"] = json!("Updated title");
    duplicate["viewer_count"] = json!(456);
    duplicate["started_at"] = json!("2026-10-23T09:00:00+02:00");
    let fixture = Fixture::new(vec![
        users(),
        stream_page(json!([live()]), "next"),
        streams(json!([duplicate])),
    ])
    .await;
    let poll = fetch_streams(
        &client(),
        &credentials(),
        &mut cached_token(),
        &["example".into()],
        &fixture.endpoints,
    )
    .await
    .unwrap_or_else(|error| panic!("stable duplicate failed: {}", error.code));
    assert_eq!(poll.streams.len(), 1);
    assert_eq!(poll.streams[0].title, "Updated title");
    assert_eq!(poll.streams[0].viewer_count, 456);
    fixture.assert_paths(&["/users", "/streams", "/streams"]);
    for (field, changed) in [
        ("id", "5678"),
        ("user_id", "99"),
        ("user_login", "other"),
        ("started_at", "2026-10-23T07:01:00Z"),
    ] {
        let mut invalid = live();
        invalid[field] = json!(changed);
        let fixture = Fixture::new(vec![
            users(),
            stream_page(json!([live()]), "next"),
            streams(json!([invalid])),
        ])
        .await;
        let result = fetch_streams(
            &client(),
            &credentials(),
            &mut cached_token(),
            &["example".into()],
            &fixture.endpoints,
        )
        .await;
        let Err(error) = result else {
            panic!("changed {field} identity must fail");
        };
        assert_eq!(
            error.code,
            "Twitch returned inconsistent broadcast metadata."
        );
        fixture.assert_paths(&["/users", "/streams", "/streams"]);
    }
}

#[tokio::test]
async fn incomplete_batches_and_invalid_stream_identity_fail_instead_of_looking_offline() {
    let mut invalid_type = live();
    invalid_type["type"] = json!("");
    let mut wrong_identity = live();
    wrong_identity["user_id"] = json!("999");
    for payload in [
        json!({"data":[]}),
        json!({"data":[],"pagination":null}),
        json!({"data":[live(),live()],"pagination":{}}),
        json!({"data":[invalid_type],"pagination":{}}),
        json!({"data":[wrong_identity],"pagination":{}}),
    ] {
        let fixture = Fixture::new(vec![users(), Reply::ok("/streams", payload)]).await;
        let mut token = cached_token();
        assert!(fetch_streams(
            &client(),
            &credentials(),
            &mut token,
            &["example".into()],
            &fixture.endpoints
        )
        .await
        .is_err());
        fixture.assert_paths(&["/users", "/streams"]);
    }
}

#[tokio::test]
async fn resolved_offline_and_missing_user_are_persisted_as_different_states() {
    let fixture = Fixture::new(vec![users(), streams(json!([]))]).await;
    let channels = vec!["example".into(), "missing".into()];
    let mut token = cached_token();
    let poll = fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &channels,
        &fixture.endpoints,
    )
    .await
    .unwrap_or_else(|error| panic!("partial user resolution failed: {}", error.code));
    assert!(!poll.users.contains_key("missing"));
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    record_check(&mut conn, &channels, "2026-10-23T08:00:00.000Z", Ok(poll)).unwrap();
    let states = conn
        .prepare("SELECT channel,state FROM metadata_checks ORDER BY channel")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        states,
        vec![
            ("example".into(), "offline".into()),
            ("missing".into(), "unknown".into())
        ]
    );
    fixture.assert_paths(&["/users", "/streams"]);
}

#[tokio::test]
async fn hourly_validation_precedes_metadata_requests() {
    let fixture = Fixture::new(vec![validation(), users(), streams(json!([]))]).await;
    let mut token = cached_token();
    token.as_mut().unwrap().validated = Instant::now() - Duration::from_secs(3601);
    assert!(fetch_streams(
        &client(),
        &credentials(),
        &mut token,
        &["example".into()],
        &fixture.endpoints
    )
    .await
    .is_ok());
    fixture.assert_paths(&["/validate", "/users", "/streams"]);
}
