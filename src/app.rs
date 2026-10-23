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
                "Use a valid account or channel login and bounded query.",
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
        }
        Err(_) => {
            value["storage_health"] = json!("unavailable");
            value["storage_bytes"] = Value::Null;
            value["observation_count"] = Value::Null;
            value["session_count"] = Value::Null;
        }
    }
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
    fn limit(&self) -> Result<usize, ApiError> {
        let limit = self.limit.unwrap_or(100);
        if !(1..=500).contains(&limit)
            || self.before.is_some_and(|v| v < 1)
            || self.offset.is_some_and(|v| v > 1_000_000)
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
    let mut rows = state
        .store
        .search(query.account, query.channel, query.before, limit + 1)
        .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(json!({"observations":rows,"next_before":next})))
}
async fn history(
    State(state): State<AppState>,
    Path(login): Path<String>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let mut rows = state.store.history(login, query.before, limit + 1).await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().map(|r| r.id)
    } else {
        None
    };
    Ok(Json(json!({"sessions":rows,"next_before":next})))
}
async fn relationships(
    State(state): State<AppState>,
    query: Result<Query<Filters>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let query = query_filters(query)?;
    let limit = query.limit()?;
    let offset = query.offset.unwrap_or(0);
    let mut rows = state
        .store
        .relationships(query.account, query.channel, offset, limit + 1)
        .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    Ok(Json(
        json!({"relationships":rows,"has_more":more,"next_offset":if more {Some(offset+limit)} else {None}}),
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
}
