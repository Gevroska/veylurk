//! Optional official broadcast metadata. IRC collection does not depend on this worker.
use crate::{
    history::{TimeRange, MODEL_VERSION},
    store::{self, Store, StoreError},
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{watch, RwLock};

const ASSOCIATION: &str = "Positive chat receipts between the reported broadcast start and last confirmed live check are observed evidence. Earlier positive receipts in the same session without a recorded departure or gap are separately labelled uncertain prior-connected inferences. Receipt time is not event occurrence time; no result proves video watching or continuous presence. Collection gaps remain unknown.";
const MAX_STREAM_PAGES: usize = 4;
const MAX_CURSOR_BYTES: usize = 4096;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS metadata_broadcasts (
 id INTEGER PRIMARY KEY, stream_id TEXT NOT NULL UNIQUE, channel TEXT NOT NULL,
 title TEXT NOT NULL, game_name TEXT NOT NULL, language TEXT NOT NULL,
 started_at TEXT NOT NULL, first_seen_at TEXT NOT NULL, last_live_at TEXT NOT NULL,
 offline_observed_at TEXT);
CREATE INDEX IF NOT EXISTS metadata_broadcasts_channel ON metadata_broadcasts(channel,id);
CREATE TABLE IF NOT EXISTS metadata_checks (
 id INTEGER PRIMARY KEY, channel TEXT NOT NULL, checked_at TEXT NOT NULL,
 state TEXT NOT NULL, broadcast_id INTEGER REFERENCES metadata_broadcasts(id),
 viewer_count INTEGER, error_code TEXT);
CREATE INDEX IF NOT EXISTS metadata_checks_channel ON metadata_checks(channel,id);
CREATE INDEX IF NOT EXISTS metadata_checks_channel_time ON metadata_checks(channel,checked_at,id);
CREATE INDEX IF NOT EXISTS metadata_checks_broadcast ON metadata_checks(broadcast_id,id);
CREATE TABLE IF NOT EXISTS metadata_titles (
 id INTEGER PRIMARY KEY, broadcast_id INTEGER NOT NULL REFERENCES metadata_broadcasts(id),
 seen_at TEXT NOT NULL, title TEXT NOT NULL, game_name TEXT NOT NULL, language TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS metadata_titles_broadcast ON metadata_titles(broadcast_id,id);
CREATE INDEX IF NOT EXISTS metadata_titles_time ON metadata_titles(broadcast_id,seen_at,id);
";

#[derive(Clone)]
pub struct Metadata {
    store: Store,
    credentials: Option<Arc<Credentials>>,
    health: Arc<RwLock<Health>>,
}

struct Credentials {
    client_id: String,
    secret: String,
}

#[derive(Clone, Serialize)]
struct Health {
    configured: bool,
    state: &'static str,
    last_check_at: Option<String>,
    last_error: Option<&'static str>,
}

pub async fn initialize(store: Store) -> Result<Metadata, StoreError> {
    store
        .call(|conn| {
            conn.execute_batch(SCHEMA)?;
            Ok(())
        })
        .await?;
    let id = std::env::var("VEYLURK_TWITCH_CLIENT_ID").ok();
    let secret = std::env::var("VEYLURK_TWITCH_CLIENT_SECRET").ok();
    let clean = |value: &str| {
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
    };
    let supplied = id.is_some() || secret.is_some();
    let credentials = match (id, secret) {
        (Some(client_id), Some(secret)) if clean(&client_id) && clean(&secret) => {
            Some(Arc::new(Credentials { client_id, secret }))
        }
        _ => None,
    };
    let configured = credentials.is_some();
    Ok(Metadata {
        store,
        credentials,
        health: Arc::new(RwLock::new(Health {
            configured,
            state: if configured { "waiting" } else { "unavailable" },
            last_check_at: None,
            last_error: if supplied && !configured {
                Some("Both valid local Twitch app credentials are required.")
            } else {
                None
            },
        })),
    })
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn database_error(_: StoreError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            json!({"error":{"code":"metadata_storage","message":"Broadcast history storage is unavailable."}}),
        ),
    )
}
fn bad_query() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(
            json!({"error":{"code":"invalid_query","message":"Use an exact Twitch login, a positive cursor, and a limit from 1 to 500."}}),
        ),
    )
}

#[derive(Default, Deserialize)]
struct Search {
    channel: Option<String>,
    account: Option<String>,
    before: Option<i64>,
    limit: Option<u32>,
    offset: Option<usize>,
    from: Option<String>,
    to: Option<String>,
}
impl Search {
    fn normalized(mut self) -> Result<Self, (StatusCode, Json<Value>)> {
        for field in [&mut self.channel, &mut self.account] {
            if let Some(value) = field {
                *value = value.trim().to_ascii_lowercase();
                if value.is_empty() {
                    *field = None;
                } else if value.len() > 25
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(bad_query());
                }
            }
        }
        if self.before.is_some_and(|v| v <= 0)
            || self.limit.is_some_and(|v| v == 0 || v > 500)
            || self.offset.is_some_and(|v| v > 1_000_000)
        {
            return Err(bad_query());
        }
        let range = TimeRange::parse(self.from, self.to).map_err(|_| bad_query())?;
        self.from = range.from;
        self.to = range.to;
        Ok(self)
    }
}

#[derive(Clone, Serialize)]
struct Broadcast {
    id: i64,
    stream_id: String,
    channel: String,
    title: String,
    game_name: String,
    language: String,
    started_at: String,
    first_seen_at: String,
    last_live_at: String,
    offline_observed_at: Option<String>,
}
fn broadcast_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Broadcast> {
    Ok(Broadcast {
        id: row.get(0)?,
        stream_id: row.get(1)?,
        channel: row.get(2)?,
        title: row.get(3)?,
        game_name: row.get(4)?,
        language: row.get(5)?,
        started_at: row.get(6)?,
        first_seen_at: row.get(7)?,
        last_live_at: row.get(8)?,
        offline_observed_at: row.get(9)?,
    })
}
const BROADCAST_COLUMNS: &str = "b.id,b.stream_id,b.channel,b.title,b.game_name,b.language,b.started_at,b.first_seen_at,b.last_live_at,b.offline_observed_at";

impl Metadata {
    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/metadata/status", get(status))
            .route("/api/broadcasts", get(broadcasts))
            .route(
                "/api/broadcasts/{id}/observations",
                get(broadcast_observations),
            )
            .route("/api/broadcasts/{id}/accounts", get(broadcast_accounts))
            .route(
                "/api/channels/{login}/metadata/history",
                get(metadata_history),
            )
            .with_state(self.clone())
    }

    /// Returns safe current states for scheduling. Old successful checks never remain live/offline
    /// when metadata is unavailable, unhealthy, stale, or dated in the future.
    pub async fn channel_states(&self) -> Result<Vec<ChannelState>, StoreError> {
        let health = self.health.read().await.clone();
        let available = health.configured && health.state == "ready";
        self.store.call(move|conn| {
            let mut stmt=conn.prepare("SELECT c.login,m.state,m.checked_at,m.broadcast_id,m.viewer_count FROM channels c LEFT JOIN metadata_checks m ON m.id=(SELECT MAX(id) FROM metadata_checks WHERE channel=c.login) WHERE c.tracked=1 ORDER BY c.login")?;
            let rows=stmt.query_map([],|row| {
                let checked_at:Option<String>=row.get(2)?;
                let age=checked_at.as_deref().and_then(|at|DateTime::parse_from_rfc3339(at).ok()).map(|at|Utc::now().signed_duration_since(at).num_seconds());
                let stale=!available||age.is_none_or(|seconds|!(0..=150).contains(&seconds));
                let reported:Option<String>=row.get(1)?;
                let state=if stale {"unknown"}else{match reported.as_deref(){Some("live")=>"live",Some("offline")=>"offline",_=>"unknown"}};
                Ok(ChannelState {channel:row.get(0)?,state:state.into(),last_reported_state:reported,checked_at,stale,broadcast_id:row.get(3)?,viewer_count:row.get(4)?})
            })?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
        }).await
    }

    pub async fn run(
        self,
        mut channels: watch::Receiver<Vec<String>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let Some(credentials) = self.credentials.clone() else {
            return;
        };
        let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Veylurk/0.1")
            .build()
        else {
            self.health.write().await.state = "error";
            self.health.write().await.last_error =
                Some("The metadata HTTP client could not start.");
            return;
        };
        let mut token: Option<Token> = None;
        let endpoints = Endpoints::twitch();
        let mut wait = Duration::ZERO;
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {},
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } else { continue; } }
            }
            let requested = channels.borrow_and_update().clone();
            if requested.is_empty() {
                token = None;
                wait = Duration::from_secs(60);
                continue;
            }
            let response = tokio::select! {
                result = fetch_streams(&client, &credentials, &mut token, &requested, &endpoints) => result,
                _ = shutdown.changed() => break,
            };
            let checked_at = store::now();
            let (result, next_wait, error) = match response {
                Ok(streams) => (Ok(streams), Duration::from_secs(60), None),
                Err(error) => (Err(error.code), error.retry_after, Some(error.code)),
            };
            let saved = self
                .store
                .call(move |conn| {
                    record_check(conn, &requested, &checked_at, result)?;
                    Ok(())
                })
                .await;
            let mut health = self.health.write().await;
            if saved.is_err() {
                health.state = "error";
                health.last_error =
                    Some("Broadcast metadata could not be saved; check storage health.");
                wait = Duration::from_secs(60);
            } else {
                health.state = if error.is_some() { "error" } else { "ready" };
                health.last_check_at = Some(store::now());
                health.last_error = error;
                wait = next_wait;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelState {
    pub channel: String,
    pub state: String,
    pub last_reported_state: Option<String>,
    pub checked_at: Option<String>,
    pub stale: bool,
    pub broadcast_id: Option<i64>,
    pub viewer_count: Option<i64>,
}

async fn status(State(service): State<Metadata>) -> ApiResult {
    let health = service.health.read().await.clone();
    let channels = service.channel_states().await.map_err(database_error)?;
    Ok(Json(
        json!({"configured":health.configured,"state":health.state,"last_check_at":health.last_check_at,"last_error":health.last_error,"channels":channels}),
    ))
}

async fn broadcasts(State(service): State<Metadata>, Query(query): Query<Search>) -> ApiResult {
    let query = query.normalized()?;
    let limit = query.limit.unwrap_or(100) as usize;
    let mut rows = service.store.call(move |conn| {
        let sql = format!("SELECT {BROADCAST_COLUMNS} FROM metadata_broadcasts b WHERE (?1 IS NULL OR b.channel=?1) AND (?2 IS NULL OR b.id<?2) AND (?3 IS NULL OR EXISTS(SELECT 1 FROM observations o WHERE o.channel=b.channel AND o.account=?3 AND o.kind IN ('names','join') AND o.received_at_utc>=b.started_at AND o.received_at_utc<=b.last_live_at AND (?5 IS NULL OR o.received_at_utc>=?5) AND (?6 IS NULL OR o.received_at_utc<?6)) OR EXISTS(SELECT 1 FROM history_visits v JOIN sessions s ON s.id=v.session_id WHERE v.channel=b.channel AND v.account=?3 AND v.first_observed_at<MAX(b.started_at,COALESCE(?5,b.started_at)) AND v.start_basis<>'uncertain_names' AND v.out_of_order=0 AND COALESCE(v.end_basis,'open')<>'out_of_order' AND (v.closed_at IS NULL OR v.closed_at>MAX(b.started_at,COALESCE(?5,b.started_at))) AND (s.ended_at IS NULL OR s.ended_at>MAX(b.started_at,COALESCE(?5,b.started_at))) AND (?6 IS NULL OR MAX(b.started_at,COALESCE(?5,b.started_at))<?6))) AND (?5 IS NULL OR b.last_live_at>=?5) AND (?6 IS NULL OR b.started_at<?6) ORDER BY b.id DESC LIMIT ?4");
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params![query.channel,query.before,query.account,limit+1,query.from,query.to], broadcast_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).await.map_err(database_error)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_before = if more {
        rows.last().map(|row| row.id)
    } else {
        None
    };
    Ok(Json(
        json!({"broadcasts":rows,"next_before":next_before,"association_scope":ASSOCIATION}),
    ))
}

async fn broadcast_observations(
    State(service): State<Metadata>,
    Path(id): Path<i64>,
    Query(query): Query<Search>,
) -> ApiResult {
    let query = query.normalized()?;
    if id <= 0 {
        return Err(bad_query());
    }
    let limit = query.limit.unwrap_or(100) as usize;
    let result = service.store.call(move |conn| {
        let sql = format!("SELECT {BROADCAST_COLUMNS} FROM metadata_broadcasts b WHERE b.id=?1");
        let broadcast = conn.query_row(&sql, [id], broadcast_row).optional()?;
        let Some(broadcast) = broadcast else { return Ok(None); };
        let mut statement = conn.prepare("SELECT id,session_id,channel,account,kind,received_at FROM observations WHERE channel=?1 AND received_at_utc>=?2 AND received_at_utc<=?3 AND (?4 IS NULL OR id<?4) AND (?5 IS NULL OR account=?5) AND (?7 IS NULL OR received_at_utc>=?7) AND (?8 IS NULL OR received_at_utc<?8) ORDER BY id DESC LIMIT ?6")?;
        let mut observations = statement.query_map(params![broadcast.channel,broadcast.started_at,broadcast.last_live_at,query.before,query.account,limit+1,query.from,query.to], |row| {
            Ok(json!({"id":row.get::<_,i64>(0)?,"session_id":row.get::<_,i64>(1)?,"channel":row.get::<_,String>(2)?,"account":row.get::<_,Option<String>>(3)?,"kind":row.get::<_,String>(4)?,"received_at":row.get::<_,String>(5)?}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = observations.len() > limit;
        observations.truncate(limit);
        let next_before = if more { observations.last().and_then(|row| row["id"].as_i64()) } else { None };
        Ok(Some(json!({"broadcast":broadcast,"observations":observations,"next_before":next_before,"association_scope":ASSOCIATION})))
    }).await.map_err(database_error)?;
    result.map(Json).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error":{"code":"not_found","message":"Broadcast not found."}})),
        )
    })
}

async fn metadata_history(
    State(service): State<Metadata>,
    Path(login): Path<String>,
    Query(query): Query<Search>,
) -> ApiResult {
    let query = query.normalized()?;
    let login = store::normalize(&login).map_err(|_| bad_query())?;
    let limit = query.limit.unwrap_or(100) as usize;
    let offset = query.offset.unwrap_or(0);
    let mut rows=service.store.call(move|conn| {
        let mut stmt=conn.prepare("SELECT m.id,m.checked_at,m.state,m.broadcast_id,m.viewer_count,m.error_code,t.title,t.game_name,t.language FROM metadata_checks m LEFT JOIN metadata_titles t ON t.id=(SELECT id FROM metadata_titles WHERE broadcast_id=m.broadcast_id AND seen_at<=m.checked_at ORDER BY seen_at DESC,id DESC LIMIT 1) WHERE m.channel=?1 AND (?2 IS NULL OR m.checked_at>=?2) AND (?3 IS NULL OR m.checked_at<?3) AND (?4 IS NULL OR m.id<?4) ORDER BY m.id DESC LIMIT ?5 OFFSET ?6")?;
        let rows=stmt.query_map(params![login,query.from,query.to,query.before,limit+1,offset as i64],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"checked_at":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"broadcast_id":r.get::<_,Option<i64>>(3)?,"viewer_count":r.get::<_,Option<i64>>(4)?,"error_code":r.get::<_,Option<String>>(5)?,"title":r.get::<_,Option<String>>(6)?,"game_name":r.get::<_,Option<String>>(7)?,"language":r.get::<_,Option<String>>(8)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
    }).await.map_err(database_error)?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last().and_then(|r| r["id"].as_i64())
    } else {
        None
    };
    Ok(Json(
        json!({"observations":rows,"has_more":more,"next_before":next,"next_offset":if more{Some(offset+limit)}else{None},"explanation":"Official metadata checks, newest receipt first. Titles/categories reflect the latest retained change at each check. Viewer count is Twitch's reported video audience at that check, separate from observed chat accounts. Unknown checks do not establish offline status."}),
    ))
}

// Account rows consist of positive receipts inside the selected broadcast window plus
// separately labelled pre-window candidates whose session has no recorded boundary.
const ACCOUNT_CTE:&str="WITH bounds AS (SELECT *,MAX(started_at,COALESCE(?3,started_at)) AS window_start FROM metadata_broadcasts WHERE id=?1 AND (?3 IS NULL OR last_live_at>=?3) AND (?4 IS NULL OR started_at<?4)), eligible AS (SELECT v.*,b.started_at AS broadcast_start,b.last_live_at AS broadcast_last,b.window_start FROM history_visits v JOIN bounds b ON b.channel=v.channel JOIN sessions s ON s.id=v.session_id WHERE (?2 IS NULL OR v.account=?2) AND v.first_observed_at<=b.last_live_at AND (?4 IS NULL OR v.first_observed_at<?4) AND (EXISTS(SELECT 1 FROM observations o WHERE o.session_id=v.session_id AND o.channel=v.channel AND o.account=v.account AND o.id>=v.first_observation_id AND o.id<=v.last_observation_id AND o.kind IN ('names','join') AND o.received_at_utc>=b.window_start AND o.received_at_utc<=b.last_live_at AND (?4 IS NULL OR o.received_at_utc<?4)) OR (v.first_observed_at<b.window_start AND (v.closed_at IS NULL OR v.closed_at>b.window_start) AND COALESCE(v.end_basis,'open')<>'out_of_order' AND v.start_basis<>'uncertain_names' AND v.out_of_order=0 AND (s.ended_at IS NULL OR s.ended_at>b.window_start)))), grouped AS (SELECT v.account,MIN(o.received_at_utc) AS first_observed_at,MAX(o.received_at_utc) AS last_observed_at,COUNT(o.id) AS positive_observation_count,COUNT(DISTINCT CASE WHEN v.start_basis<>'uncertain_names' THEN v.id END) AS visit_count,MAX(CASE WHEN v.first_observed_at<v.window_start THEN (SELECT MAX(p.received_at_utc) FROM observations p WHERE p.account=v.account AND p.channel=v.channel AND p.session_id=v.session_id AND p.id>=v.first_observation_id AND p.id<=v.last_observation_id AND p.kind IN ('names','join') AND p.received_at_utc<v.window_start) END) AS prior_observed_at FROM eligible v LEFT JOIN observations o ON o.session_id=v.session_id AND o.channel=v.channel AND o.account=v.account AND o.id>=v.first_observation_id AND o.id<=v.last_observation_id AND o.kind IN ('names','join') AND o.received_at_utc>=v.window_start AND o.received_at_utc<=v.broadcast_last AND (?4 IS NULL OR o.received_at_utc<?4) GROUP BY v.account) ";

async fn broadcast_accounts(
    State(service): State<Metadata>,
    Path(id): Path<i64>,
    Query(query): Query<Search>,
) -> ApiResult {
    let query = query.normalized()?;
    if id <= 0 {
        return Err(bad_query());
    }
    let limit = query.limit.unwrap_or(100) as usize;
    let offset = query.offset.unwrap_or(0);
    let result=service.store.call(move|conn| {
        let broadcast=conn.query_row(&format!("SELECT {BROADCAST_COLUMNS} FROM metadata_broadcasts b WHERE b.id=?1"),[id],broadcast_row).optional()?;
        let Some(broadcast)=broadcast else{return Ok(None);};
        let mut stmt=conn.prepare(&format!("{ACCOUNT_CTE} SELECT account,first_observed_at,last_observed_at,positive_observation_count,visit_count,prior_observed_at FROM grouped ORDER BY account LIMIT ?5 OFFSET ?6"))?;
        let mut rows=stmt.query_map(params![id,query.account,query.from,query.to,limit+1,offset as i64],|r| {
            let positives:i64=r.get(3)?;
            Ok(json!({"account":r.get::<_,String>(0)?,"first_observed_at":r.get::<_,Option<String>>(1)?,"last_observed_at":r.get::<_,Option<String>>(2)?,"positive_observation_count":positives,"visit_count":r.get::<_,i64>(4)?,"prior_observed_at":r.get::<_,Option<String>>(5)?,"association_basis":if positives>0{"positive_receipts"}else{"prior_connected_inference"},"state":if positives>0{"observed"}else{"inferred"},"uncertain":true,"explanation":ASSOCIATION}))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let summary=conn.query_row(&format!("{ACCOUNT_CTE} SELECT COUNT(*),COALESCE(SUM(positive_observation_count>0),0),COALESCE(SUM(positive_observation_count=0),0),COALESCE(SUM(positive_observation_count),0),COALESCE(SUM(visit_count),0) FROM grouped"),params![id,query.account,query.from,query.to],|r|Ok(json!({"account_count":r.get::<_,i64>(0)?,"observed_account_count":r.get::<_,i64>(1)?,"inferred_only_account_count":r.get::<_,i64>(2)?,"positive_observation_count":r.get::<_,i64>(3)?,"visit_count":r.get::<_,i64>(4)?})))?;
        let more=rows.len()>limit;rows.truncate(limit);
        Ok(Some(json!({"broadcast":broadcast,"accounts":rows,"summary":summary,"has_more":more,"next_offset":if more{Some(offset+limit)}else{None},"model_version":MODEL_VERSION,"explanation":ASSOCIATION})))
    }).await.map_err(database_error)?;
    result.map(Json).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error":{"code":"not_found","message":"Broadcast not found."}})),
        )
    })
}

#[derive(Deserialize)]
struct Stream {
    id: String,
    user_id: String,
    user_login: String,
    r#type: String,
    title: String,
    game_name: String,
    language: String,
    started_at: String,
    viewer_count: u64,
}
#[derive(Deserialize)]
struct Streams {
    data: Vec<Stream>,
    pagination: Pagination,
}
#[derive(Deserialize)]
struct Pagination {
    cursor: Option<String>,
}
#[derive(Deserialize)]
struct Users {
    data: Vec<User>,
}
#[derive(Deserialize)]
struct User {
    id: String,
    login: String,
}
struct Poll {
    streams: Vec<Stream>,
    users: HashMap<String, String>,
}
struct Endpoints {
    token: String,
    validate: String,
    users: String,
    streams: String,
}
impl Endpoints {
    fn twitch() -> Self {
        Self {
            token: "https://id.twitch.tv/oauth2/token".into(),
            validate: "https://id.twitch.tv/oauth2/validate".into(),
            users: "https://api.twitch.tv/helix/users".into(),
            streams: "https://api.twitch.tv/helix/streams".into(),
        }
    }
}
#[derive(Deserialize)]
struct AccessToken {
    access_token: String,
    token_type: String,
    expires_in: u64,
}
#[derive(Deserialize)]
struct Validation {
    client_id: String,
    expires_in: u64,
}
struct Token {
    value: String,
    expires: Instant,
    validated: Instant,
}
struct FetchError {
    code: &'static str,
    retry_after: Duration,
}
impl FetchError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            retry_after: Duration::from_secs(60),
        }
    }
}

async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, FetchError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| FetchError::new("Metadata response could not be read."))?
    {
        if body.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err(FetchError::new(
                "Metadata response exceeded its size limit.",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| FetchError::new("Metadata response was invalid."))
}

fn check_http(response: &reqwest::Response) -> Result<(), FetchError> {
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        let reset = response
            .headers()
            .get("ratelimit-reset")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<i64>().ok());
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let seconds = retry
            .or_else(|| reset.map(|time| time.saturating_sub(Utc::now().timestamp()).max(5) as u64))
            .unwrap_or(60)
            .clamp(5, 3600);
        return Err(FetchError {
            code: "Twitch metadata rate limit reached; waiting before retry.",
            retry_after: Duration::from_secs(seconds),
        });
    }
    if !response.status().is_success() {
        return Err(FetchError::new(
            "Twitch metadata request failed; previous information is not current.",
        ));
    }
    Ok(())
}

async fn new_token(
    client: &reqwest::Client,
    credentials: &Credentials,
    endpoints: &Endpoints,
) -> Result<Token, FetchError> {
    let response = client
        .post(&endpoints.token)
        .form(&[
            ("client_id", credentials.client_id.as_str()),
            ("client_secret", credentials.secret.as_str()),
            ("grant_type", "client_credentials"),
        ])
        .send()
        .await
        .map_err(|_| FetchError::new("Could not connect to Twitch authentication."))?;
    check_http(&response)?;
    let payload: AccessToken = bounded_json(response).await?;
    if payload.access_token.is_empty()
        || payload.access_token.len() > 4096
        || payload.expires_in == 0
        || !payload.token_type.eq_ignore_ascii_case("bearer")
    {
        return Err(FetchError::new("Twitch returned an invalid app token."));
    }
    let now = Instant::now();
    let mut token = Token {
        value: payload.access_token,
        expires: now + Duration::from_secs(payload.expires_in.min(365 * 24 * 3600)),
        validated: now,
    };
    validate_token(client, credentials, &mut token, endpoints).await?;
    Ok(token)
}

async fn validate_token(
    client: &reqwest::Client,
    credentials: &Credentials,
    token: &mut Token,
    endpoints: &Endpoints,
) -> Result<(), FetchError> {
    let response = client
        .get(&endpoints.validate)
        .bearer_auth(&token.value)
        .send()
        .await
        .map_err(|_| FetchError::new("Could not validate the Twitch app token."))?;
    check_http(&response)?;
    let validation: Validation = bounded_json(response).await?;
    if validation.client_id != credentials.client_id || validation.expires_in == 0 {
        return Err(FetchError::new("Twitch app token validation failed."));
    }
    token.validated = Instant::now();
    token.expires =
        token.validated + Duration::from_secs(validation.expires_in.min(365 * 24 * 3600));
    Ok(())
}

async fn fetch_streams(
    client: &reqwest::Client,
    credentials: &Credentials,
    token: &mut Option<Token>,
    channels: &[String],
    endpoints: &Endpoints,
) -> Result<Poll, FetchError> {
    if channels.is_empty() || channels.len() > 100 {
        return Err(FetchError::new(
            "Invalid channel count for a metadata batch.",
        ));
    }
    if token
        .as_ref()
        .is_none_or(|token| token.expires <= Instant::now() + Duration::from_secs(30))
    {
        *token = Some(new_token(client, credentials, endpoints).await?);
    }
    if token
        .as_ref()
        .is_some_and(|token| token.validated.elapsed() >= Duration::from_secs(3600))
    {
        if let Err(error) = validate_token(
            client,
            credentials,
            token.as_mut().expect("token initialized"),
            endpoints,
        )
        .await
        {
            *token = None;
            return Err(error);
        }
    }
    'authorization: for attempt in 0..2 {
        // Resolve the explicitly requested logins first: a missing account is
        // unknown, not evidence of an offline broadcast.
        let user_query: Vec<_> = channels
            .iter()
            .cloned()
            .map(|channel| ("login", channel))
            .collect();
        let response = client
            .get(&endpoints.users)
            .header("Client-Id", &credentials.client_id)
            .bearer_auth(&token.as_ref().expect("token initialized").value)
            .query(&user_query)
            .send()
            .await
            .map_err(|_| FetchError::new("Could not resolve Twitch channels."))?;
        if response.status() == StatusCode::UNAUTHORIZED {
            *token = None;
            if attempt == 0 {
                *token = Some(new_token(client, credentials, endpoints).await?);
                continue;
            }
            return Err(FetchError::new(
                "Twitch rejected the configured app credentials.",
            ));
        }
        check_http(&response)?;
        let resolved: Users = bounded_json(response).await?;
        let mut users = HashMap::new();
        let mut user_ids = HashSet::new();
        for user in resolved.data {
            if !channels.contains(&user.login)
                || user.id.is_empty()
                || user.id.len() > 128
                || !user.id.bytes().all(|byte| byte.is_ascii_digit())
                || !user_ids.insert(user.id.clone())
                || users.insert(user.login, user.id).is_some()
            {
                return Err(FetchError::new(
                    "Twitch returned inconsistent channel identities.",
                ));
            }
        }
        if users.is_empty() {
            return Ok(Poll {
                streams: vec![],
                users,
            });
        }
        let mut query = vec![("first", "100".to_owned())];
        query.extend(
            channels
                .iter()
                .cloned()
                .map(|channel| ("user_login", channel)),
        );
        let mut after = None::<String>;
        let mut cursors = HashSet::new();
        let mut by_channel = HashMap::<String, Stream>::new();
        let mut stream_ids = HashMap::<String, String>::new();
        for _ in 0..MAX_STREAM_PAGES {
            let mut request = client
                .get(&endpoints.streams)
                .header("Client-Id", &credentials.client_id)
                .bearer_auth(&token.as_ref().expect("token initialized").value)
                .query(&query);
            if let Some(cursor) = &after {
                request = request.query(&[("after", cursor)]);
            }
            let response = request
                .send()
                .await
                .map_err(|_| FetchError::new("Could not connect to Twitch metadata."))?;
            if response.status() == StatusCode::UNAUTHORIZED {
                *token = None;
                if attempt == 0 {
                    *token = Some(new_token(client, credentials, endpoints).await?);
                    // Discard partial pages and resolve identities again with the new token.
                    continue 'authorization;
                }
                return Err(FetchError::new(
                    "Twitch rejected the configured app credentials.",
                ));
            }
            check_http(&response)?;
            let payload: Streams = bounded_json(response).await?;
            let mut page_channels = HashSet::new();
            let mut page_ids = HashSet::new();
            for stream in payload.data {
                if !channels.contains(&stream.user_login)
                    || users.get(&stream.user_login) != Some(&stream.user_id)
                    || stream.r#type != "live"
                    || !page_channels.insert(stream.user_login.clone())
                    || !page_ids.insert(stream.id.clone())
                    || stream.id.is_empty()
                    || stream.id.len() > 128
                    || !stream.id.bytes().all(|byte| byte.is_ascii_digit())
                    || stream.title.len() > 8192
                    || stream.game_name.len() > 1024
                    || stream.language.len() > 32
                    || stream.viewer_count > i64::MAX as u64
                    || DateTime::parse_from_rfc3339(&stream.started_at).is_err()
                    || stream_ids
                        .get(&stream.id)
                        .is_some_and(|login| login != &stream.user_login)
                    || by_channel.get(&stream.user_login).is_some_and(|previous| {
                        previous.id != stream.id
                            || previous.user_id != stream.user_id
                            || DateTime::parse_from_rfc3339(&previous.started_at).ok()
                                != DateTime::parse_from_rfc3339(&stream.started_at).ok()
                    })
                {
                    return Err(FetchError::new(
                        "Twitch returned inconsistent broadcast metadata.",
                    ));
                }
                // Viewer ranking can repeat a stream on a later page. Preserve the
                // latest mutable fields only after its immutable identity agrees.
                stream_ids.insert(stream.id.clone(), stream.user_login.clone());
                by_channel.insert(stream.user_login.clone(), stream);
            }
            let Some(cursor) = payload.pagination.cursor.filter(|value| !value.is_empty()) else {
                return Ok(Poll {
                    streams: channels
                        .iter()
                        .filter_map(|channel| by_channel.remove(channel))
                        .collect(),
                    users,
                });
            };
            if cursor.len() > MAX_CURSOR_BYTES || !cursors.insert(cursor.clone()) {
                return Err(FetchError::new(
                    "Twitch returned an incomplete metadata batch.",
                ));
            }
            after = Some(cursor);
        }
        return Err(FetchError::new(
            "Twitch returned an incomplete metadata batch.",
        ));
    }
    Err(FetchError::new("Twitch metadata authorization failed."))
}

fn record_check(
    conn: &mut rusqlite::Connection,
    channels: &[String],
    checked_at: &str,
    result: Result<Poll, &'static str>,
) -> rusqlite::Result<()> {
    let transaction = conn.transaction()?;
    match result {
        Err(code) => {
            for channel in channels {
                transaction.execute("INSERT INTO metadata_checks(channel,checked_at,state,error_code) VALUES(?1,?2,'unknown',?3)",params![channel,checked_at,code])?;
            }
        }
        Ok(poll) => {
            let by_channel: HashMap<_, _> = poll
                .streams
                .into_iter()
                .map(|stream| (stream.user_login.clone(), stream))
                .collect();
            for channel in channels {
                if !poll.users.contains_key(channel) {
                    transaction.execute("INSERT INTO metadata_checks(channel,checked_at,state,error_code) VALUES(?1,?2,'unknown','Channel login could not be resolved.')",params![channel,checked_at])?;
                    continue;
                }
                if let Some(stream) = by_channel.get(channel) {
                    let started_at = DateTime::parse_from_rfc3339(&stream.started_at)
                        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
                        .with_timezone(&Utc)
                        .to_rfc3339_opts(SecondsFormat::Millis, true);
                    let identity: Option<(String, String)> = transaction
                        .query_row(
                            "SELECT channel,started_at FROM metadata_broadcasts WHERE stream_id=?1",
                            [&stream.id],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?;
                    if identity.is_some_and(|(known_channel, known_start)| {
                        known_channel != *channel || known_start != started_at
                    }) {
                        // Do not silently reassign older login-based evidence after an identity change.
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    transaction.execute("UPDATE metadata_broadcasts SET offline_observed_at=COALESCE(offline_observed_at,?1) WHERE channel=?2 AND stream_id<>?3 AND offline_observed_at IS NULL",params![checked_at,channel,stream.id])?;
                    let previous: Option<(i64,String,String,String)> = transaction.query_row("SELECT id,title,game_name,language FROM metadata_broadcasts WHERE stream_id=?1",[&stream.id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
                    transaction.execute("INSERT INTO metadata_broadcasts(stream_id,channel,title,game_name,language,started_at,first_seen_at,last_live_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7) ON CONFLICT(stream_id) DO UPDATE SET title=excluded.title,game_name=excluded.game_name,language=excluded.language,last_live_at=excluded.last_live_at,offline_observed_at=NULL",params![stream.id,channel,stream.title,stream.game_name,stream.language,started_at,checked_at])?;
                    let id: i64 = transaction.query_row(
                        "SELECT id FROM metadata_broadcasts WHERE stream_id=?1",
                        [&stream.id],
                        |row| row.get(0),
                    )?;
                    let changed = previous.is_none_or(|(_, title, game, language)| {
                        title != stream.title
                            || game != stream.game_name
                            || language != stream.language
                    });
                    if changed {
                        transaction.execute("INSERT INTO metadata_titles(broadcast_id,seen_at,title,game_name,language) VALUES(?1,?2,?3,?4,?5)",params![id,checked_at,stream.title,stream.game_name,stream.language])?;
                    }
                    transaction.execute("INSERT INTO metadata_checks(channel,checked_at,state,broadcast_id,viewer_count) VALUES(?1,?2,'live',?3,?4)",params![channel,checked_at,id,stream.viewer_count as i64])?;
                } else {
                    transaction.execute("UPDATE metadata_broadcasts SET offline_observed_at=COALESCE(offline_observed_at,?1) WHERE channel=?2 AND offline_observed_at IS NULL",params![checked_at,channel])?;
                    transaction.execute("INSERT INTO metadata_checks(channel,checked_at,state) VALUES(?1,?2,'offline')",params![channel,checked_at])?;
                }
            }
        }
    }
    transaction.commit()
}

#[cfg(test)]
#[path = "metadata_http_tests.rs"]
mod http_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn live(title: &str) -> Stream {
        Stream {
            id: "1234".into(),
            user_id: "42".into(),
            user_login: "example".into(),
            r#type: "live".into(),
            title: title.into(),
            game_name: "Game".into(),
            language: "en".into(),
            started_at: "2026-10-23T07:00:00Z".into(),
            viewer_count: 123,
        }
    }
    fn poll(streams: Vec<Stream>) -> Poll {
        Poll {
            streams,
            users: HashMap::from([("example".into(), "42".into())]),
        }
    }
    #[test]
    fn metadata_errors_never_end_broadcasts_or_rewrite_title_history() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let channels = vec!["example".to_string()];
        record_check(
            &mut conn,
            &channels,
            "2026-10-23T08:00:00.000Z",
            Ok(poll(vec![live("First")])),
        )
        .unwrap();
        record_check(
            &mut conn,
            &channels,
            "2026-10-23T08:01:00.000Z",
            Err("Network unavailable"),
        )
        .unwrap();
        let state: (String, Option<String>) = conn
            .query_row(
                "SELECT last_live_at,offline_observed_at FROM metadata_broadcasts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("2026-10-23T08:00:00.000Z".into(), None));
        record_check(
            &mut conn,
            &channels,
            "2026-10-23T08:02:00.000Z",
            Ok(poll(vec![live("Second")])),
        )
        .unwrap();
        record_check(
            &mut conn,
            &channels,
            "2026-10-23T08:03:00.000Z",
            Ok(poll(vec![live("Second")])),
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM metadata_titles", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        record_check(
            &mut conn,
            &channels,
            "2026-10-23T08:04:00.000Z",
            Ok(poll(vec![])),
        )
        .unwrap();
        let bounds: (String, String) = conn
            .query_row(
                "SELECT last_live_at,offline_observed_at FROM metadata_broadcasts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            bounds,
            (
                "2026-10-23T08:03:00.000Z".into(),
                "2026-10-23T08:04:00.000Z".into()
            )
        );
    }
    #[test]
    fn metadata_search_normalizes_logins_and_rejects_invalid_cursors() {
        assert_eq!(
            Search {
                account: Some(" Example ".into()),
                ..Search::default()
            }
            .normalized()
            .unwrap()
            .account
            .as_deref(),
            Some("example")
        );
        assert!(Search {
            before: Some(0),
            ..Search::default()
        }
        .normalized()
        .is_err());
        assert!(Search {
            channel: Some("bad?login".into()),
            ..Search::default()
        }
        .normalized()
        .is_err());
    }

    #[tokio::test]
    async fn broadcast_association_has_exact_account_and_conservative_time_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("history.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        let rows = [
            ("alice", "2026-10-23T06:59:59.999Z"),
            ("alice", "2026-10-23T07:00:00.000Z"),
            ("alice", "2026-10-23T08:00:00.000Z"),
            ("alice", "2026-10-23T08:00:00.001Z"),
            ("bob", "2026-10-23T07:30:00.000Z"),
        ]
        .into_iter()
        .map(|(account, at)| crate::store::NewObservation {
            channel: "example".into(),
            account: Some(account.into()),
            kind: "names".into(),
            received_at: at.into(),
        })
        .collect();
        store.append(session, rows).await.unwrap();
        let service = initialize(store.clone()).await.unwrap();
        let broadcast_id = store
            .call(|conn| {
                record_check(
                    conn,
                    &["example".into()],
                    "2026-10-23T08:00:00.000Z",
                    Ok(poll(vec![live("Test")])),
                )?;
                Ok(
                    conn.query_row("SELECT id FROM metadata_broadcasts", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        let Json(first) = broadcast_observations(
            State(service.clone()),
            Path(broadcast_id),
            Query(Search {
                account: Some("ALICE".into()),
                limit: Some(1),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(first["observations"].as_array().unwrap().len(), 1);
        assert_eq!(
            first["observations"][0]["received_at"],
            "2026-10-23T08:00:00.000Z"
        );
        let Json(second) = broadcast_observations(
            State(service.clone()),
            Path(broadcast_id),
            Query(Search {
                account: Some("alice".into()),
                limit: Some(1),
                before: first["next_before"].as_i64(),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(second["observations"].as_array().unwrap().len(), 1);
        assert_eq!(
            second["observations"][0]["received_at"],
            "2026-10-23T07:00:00.000Z"
        );
        assert!(second["next_before"].is_null());
        let Json(absent) = broadcasts(
            State(service.clone()),
            Query(Search {
                account: Some("unseen".into()),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert!(absent["broadcasts"].as_array().unwrap().is_empty());
        store
            .end_session(session, "test_finished".into())
            .await
            .unwrap();
        store.checkpoint().await.unwrap();
    }

    #[tokio::test]
    async fn account_summaries_distinguish_positive_receipts_prior_inference_and_ambiguous_rosters()
    {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("summary.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        let events = [
            ("prior", "join", "06:50:00"),
            ("departed", "join", "06:40:00"),
            ("departed", "part", "06:55:00"),
            ("alice", "join", "07:10:00"),
            ("alice", "part", "07:20:00"),
            ("alice", "join", "07:30:00"),
            ("part_only", "part", "07:40:00"),
            ("late", "join", "06:00:00"),
            ("late", "part", "06:10:00"),
            ("late", "names", "07:50:00"),
        ]
        .into_iter()
        .map(|(account, kind, time)| crate::store::NewObservation {
            channel: "example".into(),
            account: Some(account.into()),
            kind: kind.into(),
            received_at: format!("2026-10-23T{time}.000Z"),
        })
        .collect();
        store.append(session, events).await.unwrap();
        let service = initialize(store.clone()).await.unwrap();
        store
            .call(|conn| {
                record_check(
                    conn,
                    &["example".into()],
                    "2026-10-23T08:00:00.000Z",
                    Ok(poll(vec![live("Title")])),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let Json(result) =
            broadcast_accounts(State(service.clone()), Path(1), Query(Search::default()))
                .await
                .unwrap();
        let rows = result["accounts"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        let alice = rows.iter().find(|r| r["account"] == "alice").unwrap();
        assert_eq!(alice["positive_observation_count"], 2);
        assert_eq!(alice["visit_count"], 2);
        assert_eq!(alice["association_basis"], "positive_receipts");
        let prior = rows.iter().find(|r| r["account"] == "prior").unwrap();
        assert_eq!(prior["association_basis"], "prior_connected_inference");
        assert!(prior["first_observed_at"].is_null());
        assert_eq!(prior["positive_observation_count"], 0);
        assert_eq!(prior["prior_observed_at"], "2026-10-23T06:50:00.000Z");
        let late = rows.iter().find(|r| r["account"] == "late").unwrap();
        assert_eq!(late["positive_observation_count"], 1);
        assert_eq!(late["visit_count"], 0);
        assert_eq!(result["summary"]["observed_account_count"], 2);
        assert_eq!(result["summary"]["inferred_only_account_count"], 1);
        let Json(found) = broadcasts(
            State(service.clone()),
            Query(Search {
                account: Some("prior".into()),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(found["broadcasts"].as_array().unwrap().len(), 1);
        let Json(within_window) = broadcasts(
            State(service.clone()),
            Query(Search {
                account: Some("prior".into()),
                from: Some("2026-10-23T07:35:00Z".into()),
                to: Some("2026-10-23T07:45:00Z".into()),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(within_window["broadcasts"].as_array().unwrap().len(), 1);
        let Json(filtered) = broadcast_accounts(
            State(service),
            Path(1),
            Query(Search {
                from: Some("2026-10-23T07:30:00Z".into()),
                to: Some("2026-10-23T07:50:00Z".into()),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        let rows = filtered["accounts"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!rows.iter().any(|r| r["account"] == "late"));
    }

    #[tokio::test]
    async fn metadata_timeline_retains_title_category_and_viewer_checks_with_range_pagination() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("metadata.sqlite3"))
            .await
            .unwrap();
        let service = initialize(store.clone()).await.unwrap();
        store
            .call(|conn| {
                record_check(
                    conn,
                    &["example".into()],
                    "2026-10-23T08:00:00.000Z",
                    Ok(poll(vec![live("First")])),
                )?;
                let mut changed = live("Second");
                changed.game_name = "Another category".into();
                changed.viewer_count = 321;
                record_check(
                    conn,
                    &["example".into()],
                    "2026-10-23T08:01:00.000Z",
                    Ok(poll(vec![changed])),
                )?;
                record_check(
                    conn,
                    &["example".into()],
                    "2026-10-23T08:02:00.000Z",
                    Err("Unavailable"),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let Json(first) = metadata_history(
            State(service.clone()),
            Path("example".into()),
            Query(Search {
                limit: Some(1),
                from: Some("2026-10-23T10:00:00+02:00".into()),
                to: Some("2026-10-23T08:02:00Z".into()),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(first["observations"][0]["title"], "Second");
        assert_eq!(first["observations"][0]["game_name"], "Another category");
        assert_eq!(first["observations"][0]["viewer_count"], 321);
        assert_eq!(first["has_more"], true);
        let Json(second) = metadata_history(
            State(service.clone()),
            Path("example".into()),
            Query(Search {
                limit: Some(1),
                before: first["next_before"].as_i64(),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(second["observations"][0]["title"], "First");
        assert_eq!(second["has_more"], false);
        let Json(unknown) = metadata_history(
            State(service),
            Path("example".into()),
            Query(Search {
                limit: Some(1),
                ..Search::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(unknown["observations"][0]["state"], "unknown");
        assert!(unknown["observations"][0]["viewer_count"].is_null());
        assert!(unknown["observations"][0]["title"].is_null());
    }

    #[tokio::test]
    async fn scheduling_states_are_unknown_when_unhealthy_stale_or_future_dated() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("states.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let service = initialize(store.clone()).await.unwrap();
        {
            let mut health = service.health.write().await;
            health.configured = true;
            health.state = "ready";
        }
        store.call(|conn|{conn.execute("INSERT INTO metadata_checks(channel,checked_at,state) VALUES('example',?1,'offline')",[store::now()])?;Ok(())}).await.unwrap();
        assert_eq!(service.channel_states().await.unwrap()[0].state, "offline");
        service.health.write().await.state = "error";
        assert_eq!(service.channel_states().await.unwrap()[0].state, "unknown");
        service.health.write().await.state = "ready";
        store
            .call(|conn| {
                conn.execute(
                    "UPDATE metadata_checks SET checked_at='2020-01-01T00:00:00.000Z'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(service.channel_states().await.unwrap()[0].stale);
        store
            .call(|conn| {
                conn.execute(
                    "UPDATE metadata_checks SET checked_at='2099-01-01T00:00:00.000Z'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(service.channel_states().await.unwrap()[0].state, "unknown");
    }
}
