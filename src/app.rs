//! Loopback API. The UI and every writable endpoint share one checked origin.
use crate::{
    collector::SharedStatus,
    store::{Store, StoreError},
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        DefaultBodyLimit, Path, Query, Request, State,
    },
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, patch},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub status: SharedStatus,
    pub channels: watch::Sender<Vec<String>>,
    pub port: u16,
    pub configuration: Arc<Mutex<()>>,
}

#[derive(Debug)]
struct ApiError(StatusCode, &'static str, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error":{"code":self.1,"message":self.2}})),
        )
            .into_response()
    }
}
impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Invalid => Self(
                StatusCode::BAD_REQUEST,
                "invalid_input",
                "Use valid logins, RFC3339 dates, an ordered range, and a bounded query. Point-in-time queries cannot be in the future.",
            ),
            StoreError::ChannelLimit => Self(
                StatusCode::BAD_REQUEST,
                "channel_limit",
                "At most 50 channels can be tracked.",
            ),
            StoreError::NotFound => Self(
                StatusCode::NOT_FOUND,
                "not_found",
                "Tracked channel not found.",
            ),
            _ => Self(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
                "Storage failed; check application status.",
            ),
        }
    }
}

pub fn router(state: AppState, additional: Router) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/theme.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/theme.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route("/api/status", get(status))
        .route("/api/channels", get(channels).post(track))
        .route("/api/channels/{login}", patch(enable).delete(remove))
        .route("/api/channels/{login}/history", get(history))
        .route("/api/search", get(search))
        .route("/api/relationships", get(relationships))
        .route("/api/history", get(visits))
        .route("/api/presence", get(presence))
        .with_state(state.clone())
        .merge(additional)
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "not_found", "Endpoint not found.") })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state, security))
}

async fn security(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if host != format!("127.0.0.1:{}", state.port) && host != format!("localhost:{}", state.port) {
        return ApiError(
            StatusCode::FORBIDDEN,
            "invalid_host",
            "Only the local application host is accepted.",
        )
        .into_response();
    }
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) {
        let expected = format!("http://{host}");
        if request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
        {
            return ApiError(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "Changes must come from the local application page.",
            )
            .into_response();
        }
        if request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(str::trim)
            != Some("application/json")
        {
            return ApiError(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "json_required",
                "Changes require application/json.",
            )
            .into_response();
        }
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,header::HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}

async fn status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let current = state.status.read().await.clone();
    let mut value = serde_json::to_value(current).expect("serializable status");
    match state.store.stats().await {
        Ok(stats) => {
            value["observation_count"] = json!(stats.observation_count);
            value["session_count"] = json!(stats.session_count);
            value["storage_bytes"] = json!(stats.storage_bytes);
            value["storage_health"] = json!("available");
            let available = crate::runtime::disk_available(state.store.path());
            value["disk_available_bytes"] = json!(available);
            value["storage_warning"] = json!(available
                .filter(|bytes| *bytes < 512 * 1024 * 1024)
                .map(|_| {
                    "Low disk space; retained history may stop growing when the disk is full."
                }));
        }
        Err(_) => {
            value["storage_health"] = json!("unavailable");
            value["storage_bytes"] = Value::Null;
            value["observation_count"] = Value::Null;
            value["session_count"] = Value::Null;
        }
    }
    value["diagnostics"] = json!(state.store.diagnostics());
    Ok(Json(value))
}
async fn channels(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({"channels":state.store.channels().await?})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Track {
    login: Option<String>,
    logins: Option<Vec<String>>,
    #[serde(default = "default_true")]
    enabled: bool,
}
fn default_true() -> bool {
    true
}
fn json_body<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|v| v.0).map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "The JSON body is invalid.",
        )
    })
}

async fn refresh(state: &AppState) -> Result<(), ApiError> {
    let enabled = state.store.enabled().await?;
    state.channels.send_if_modified(|current| {
        if *current == enabled {
            false
        } else {
            *current = enabled;
            true
        }
    });
    Ok(())
}
async fn track(
    State(state): State<AppState>,
    body: Result<Json<Track>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let configuration = state.configuration.clone();
    let _guard = configuration.lock().await;
    let body = json_body(body)?;
    let logins = match (body.login, body.logins) {
        (Some(login), None) => vec![login],
        (None, Some(logins)) => logins,
        _ => return Err(StoreError::Invalid.into()),
    };
    state.store.track(logins, body.enabled).await?;
    refresh(&state).await?;
    channels(State(state)).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enable {
    enabled: bool,
}
async fn enable(
    State(state): State<AppState>,
    Path(login): Path<String>,
    body: Result<Json<Enable>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.configuration.lock().await;
    let body = json_body(body)?;
    let login = crate::store::normalize(&login)?;
    state
        .store
        .update_channel(login.clone(), body.enabled, false)
        .await?;
    refresh(&state).await?;
    let channel = state
        .store
        .channels()
        .await?
        .into_iter()
        .find(|c| c.login == login)
        .ok_or(StoreError::NotFound)?;
    Ok(Json(json!({"channel":channel})))
}
async fn remove(
    State(state): State<AppState>,
    Path(login): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.configuration.lock().await;
    state.store.update_channel(login, false, true).await?;
    refresh(&state).await?;
    Ok(Json(json!({"removed":true,"history_retained":true})))
}

#[derive(Default, Deserialize)]
struct Filters {
    account: Option<String>,
    channel: Option<String>,
    before: Option<i64>,
    limit: Option<usize>,
    offset: Option<usize>,
    from: Option<String>,
    to: Option<String>,
    at: Option<String>,
    broadcast_id: Option<i64>,
}
fn query_filters(query: Result<Query<Filters>, QueryRejection>) -> Result<Filters, ApiError> {
    query.map(|v| v.0).map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "Query parameters are invalid.",
        )
    })
}
impl Filters {
    fn range(&self) -> Result<crate::history::TimeRange, ApiError> {
        Ok(crate::history::TimeRange::parse(
            self.from.clone(),
            self.to.clone(),
        )?)
    }
    fn limit(&self) -> Result<usize, ApiError> {
        let limit = self.limit.unwrap_or(100);
        if !(1..=500).contains(&limit)
            || self.before.is_some_and(|v| v < 1)
            || self.offset.is_some_and(|v| v > 1_000_000)
            || self.broadcast_id.is_some_and(|v| v <= 0)
        {
            return Err(StoreError::Invalid.into());
        }
        Ok(limit)
    }
}
async fn search(
    State(state): State<AppState>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let range = query.range()?;
    let mut rows = state
        .store
        .search_range(
            query.account,
            query.channel,
            query.before,
            limit + 1,
            range.clone(),
        )
        .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(
        json!({"observations":rows,"next_before":next,"range":range,"time_scope":"Inclusive from, exclusive to; received timestamps, not event occurrence times."}),
    ))
}
async fn history(
    State(state): State<AppState>,
    Path(login): Path<String>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let range = query.range()?;
    let mut rows = state
        .store
        .history_range(login, query.before, limit + 1, range.clone())
        .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(
        json!({"sessions":rows,"next_before":next,"range":range}),
    ))
}
async fn relationships(
    State(state): State<AppState>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let range = query.range()?;
    let offset = query.offset.unwrap_or(0);
    let mut rows = state
        .store
        .relationships_range(
            query.account,
            query.channel,
            offset,
            limit + 1,
            range.clone(),
        )
        .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    Ok(Json(
        json!({"relationships":rows,"has_more":more,"next_offset":if more {Some(offset+limit)} else {None},"range":range}),
    ))
}

async fn visits(
    State(state): State<AppState>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let mut range = query.range()?;
    let mut channel = query.channel;
    if let Some(id) = query.broadcast_id {
        use rusqlite::OptionalExtension;
        let broadcast=state.store.call_read(crate::diagnostics::Operation::Query,move|conn|Ok(conn.query_row("SELECT channel,started_at,last_live_at FROM metadata_broadcasts WHERE id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?)).await?.ok_or(StoreError::NotFound)?;
        if channel
            .as_deref()
            .is_some_and(|c| !c.eq_ignore_ascii_case(&broadcast.0))
        {
            return Err(StoreError::Invalid.into());
        }
        channel = Some(broadcast.0);
        if range.from.as_ref().is_none_or(|from| from < &broadcast.1) {
            range.from = Some(broadcast.1);
        }
        // The inclusive last live receipt is represented by the following millisecond.
        let end = chrono::DateTime::parse_from_rfc3339(&broadcast.2)
            .map_err(|_| StoreError::Invalid)?
            + chrono::Duration::milliseconds(1);
        let end = end.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        if range.to.as_ref().is_none_or(|to| to > &end) {
            range.to = Some(end);
        }
    }
    let offset = query.offset.unwrap_or(0);
    let empty = matches!((&range.from,&range.to),(Some(from),Some(to)) if from>=to);
    let mut rows = if empty {
        Vec::new()
    } else {
        state
            .store
            .visits(crate::history::VisitQuery {
                account: query.account,
                channel,
                range: range.clone(),
                before: query.before,
                offset,
                limit: limit + 1,
            })
            .await?
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(
        json!({"visits":rows,"has_more":more,"next_before":next,"next_offset":if more{Some(offset+limit)}else{None},"model_version":crate::history::MODEL_VERSION,"explanation":crate::history::EXPLANATION,"range":range}),
    ))
}

async fn presence(
    State(state): State<AppState>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let offset = query.offset.unwrap_or(0);
    let channel = query.channel.ok_or(StoreError::Invalid)?;
    let at = crate::history::utc(query.at.as_deref().ok_or(StoreError::Invalid)?)?;
    let mut rows = state
        .store
        .presence(channel, query.account, at.clone(), offset, limit + 1)
        .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    Ok(Json(
        json!({"accounts":rows,"at":at,"has_more":more,"next_offset":if more{Some(offset+limit)}else{None},"model_version":crate::history::MODEL_VERSION,"explanation":crate::history::PRESENCE_EXPLANATION}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;
    #[tokio::test]
    async fn loopback_api_rejects_cross_site_writes_and_retains_removed_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("test.sqlite")).await.unwrap();
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
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/channels")
            .header("host", "127.0.0.1:8794")
            .header("origin", "https://evil.example")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"login":"test"}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/channels")
            .header("host", "127.0.0.1:8794")
            .header("origin", "http://127.0.0.1:8794")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"logins":["Test","test"]}"#))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(store.channels().await.unwrap().len(), 1);
        let session = store.begin_session(vec!["test".into()]).await.unwrap();
        store
            .append(
                session,
                vec![crate::store::NewObservation {
                    channel: "test".into(),
                    account: Some("alice".into()),
                    kind: "names".into(),
                    received_at: crate::store::now(),
                }],
            )
            .await
            .unwrap();
        store.end_session(session, "fixture".into()).await.unwrap();
        let request = axum::http::Request::builder()
            .method("DELETE")
            .uri("/api/channels/test")
            .header("host", "127.0.0.1:8794")
            .header("origin", "http://127.0.0.1:8794")
            .header("content-type", "application/json")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
        assert!(!store.channels().await.unwrap()[0].tracked);
        let request = axum::http::Request::builder()
            .uri("/api/search?account=alice")
            .header("host", "127.0.0.1:8794")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["observations"][0]["account"], "alice");
        let request = axum::http::Request::builder()
            .uri("/api/search?limit=invalid")
            .header("host", "127.0.0.1:8794")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["error"]["code"], "invalid_query");
        let request = axum::http::Request::builder()
            .uri("/api/status")
            .header("host", "evil.example")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn status_keeps_first_failure_and_exposes_codes_without_sql_error_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("diagnostics.sqlite3"))
            .await
            .unwrap();
        const PRIVATE: &str = "private SQL account credential marker";
        let result = store
            .call(|_| {
                Err::<(), crate::store::StoreError>(crate::store::StoreError::Sqlite(
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
                        Some(PRIVATE.to_owned()),
                    ),
                ))
            })
            .await;
        assert!(result.is_err());
        assert!(store
            .call(|_| Err::<(), crate::store::StoreError>(crate::store::StoreError::Invalid))
            .await
            .is_err());
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
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/status")
                    .header("host", "127.0.0.1:8794")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 32768)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(PRIVATE));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        let first = &value["diagnostics"]["first_failure"];
        let latest = &value["diagnostics"]["latest"];
        assert_eq!(
            first["sqlite_primary_code"],
            rusqlite::ffi::SQLITE_INTERRUPT
        );
        assert_eq!(first["progress_interrupt_requested"], false);
        assert!(first["sequence"].as_u64().unwrap() < latest["sequence"].as_u64().unwrap());
        assert!(value["diagnostics"]["persistence"]["state"].is_string());
    }

    #[tokio::test]
    async fn time_apis_normalize_ranges_and_reject_invalid_dates_and_future_presence() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("ranges.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        store
            .append(
                session,
                [
                    ("alice", "join", "00:00:00"),
                    ("alice", "part", "00:00:30"),
                    ("bob", "join", "00:01:00"),
                ]
                .into_iter()
                .map(|(account, kind, time)| crate::store::NewObservation {
                    channel: "example".into(),
                    account: Some(account.into()),
                    kind: kind.into(),
                    received_at: format!("2026-01-01T{time}Z"),
                })
                .collect(),
            )
            .await
            .unwrap();
        let (channels, _) = watch::channel(Vec::new());
        let app = router(
            AppState {
                status: crate::collector::status(&store),
                store,
                channels,
                port: 8794,
                configuration: Arc::new(Mutex::new(())),
            },
            Router::new(),
        );
        let query = "from=2026-01-01T02%3A00%3A00%2B02%3A00&to=2026-01-01T00%3A01%3A00Z";
        for (path, field, count) in [
            (format!("/api/search?{query}"), "observations", 2),
            (format!("/api/relationships?{query}"), "relationships", 1),
            (format!("/api/history?{query}"), "visits", 1),
        ] {
            let request = axum::http::Request::builder()
                .uri(path)
                .header("host", "127.0.0.1:8794")
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body: Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 65536)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(body[field].as_array().unwrap().len(), count);
        }
        for path in [
            "/api/search?from=not-a-date",
            "/api/history?from=2026-01-02T00:00:00Z&to=2026-01-01T00:00:00Z",
            "/api/presence?channel=example&at=2030-01-01T00:00:00Z",
        ] {
            let request = axum::http::Request::builder()
                .uri(path)
                .header("host", "127.0.0.1:8794")
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[tokio::test]
    async fn broadcast_visit_ranges_return_empty_for_disjoint_windows_despite_a_long_visit() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("intersection.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        store
            .append(
                session,
                ["2026-01-01T00:00:00Z", "2026-01-04T00:00:00Z"]
                    .into_iter()
                    .map(|at| crate::store::NewObservation {
                        channel: "example".into(),
                        account: Some("alice".into()),
                        kind: "join".into(),
                        received_at: at.into(),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        let _metadata = crate::metadata::initialize(store.clone()).await.unwrap();
        store.call(|conn|{conn.execute("INSERT INTO metadata_broadcasts(stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at) VALUES('1','example','Title','Game','en','2026-01-01T10:00:00.000Z','2026-01-01T10:00:00.000Z','2026-01-01T11:00:00.000Z')",[])?;Ok(())}).await.unwrap();
        let (channels, _) = watch::channel(Vec::new());
        let state = AppState {
            status: crate::collector::status(&store),
            store,
            channels,
            port: 8794,
            configuration: Arc::new(Mutex::new(())),
        };
        for (from, to) in [
            ("2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z"),
            ("2025-12-30T00:00:00Z", "2026-01-01T09:00:00Z"),
        ] {
            let Json(result) = visits(
                State(state.clone()),
                Ok(Query(Filters {
                    broadcast_id: Some(1),
                    from: Some(from.into()),
                    to: Some(to.into()),
                    ..Filters::default()
                })),
            )
            .await
            .unwrap();
            assert!(result["visits"].as_array().unwrap().is_empty());
            assert_eq!(result["has_more"], false);
        }
    }
}
