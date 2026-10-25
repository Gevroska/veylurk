//! Local operating policy and graceful control. Metadata uncertainty never means offline.
use crate::{
    diagnostics::Operation,
    metadata::Metadata,
    store::{Store, StoreError},
};
use axum::{
    extract::{rejection::JsonRejection, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionMode {
    All,
    LiveOnly,
}

#[derive(Clone)]
pub struct Runtime {
    store: Store,
    mode: watch::Sender<CollectionMode>,
    shutdown: watch::Sender<bool>,
    info: Value,
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(status: StatusCode, code: &str, message: &str) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
}
fn storage_error(_: StoreError) -> (StatusCode, Json<Value>) {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_unavailable",
        "Settings could not be read or saved.",
    )
}
fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}

impl Runtime {
    pub async fn initialize(
        store: Store,
        shutdown: watch::Sender<bool>,
        port: u16,
    ) -> Result<Self, StoreError> {
        let mode = store.call_write(Operation::SettingsInitialize, |conn| {
            conn.execute_batch("CREATE TABLE IF NOT EXISTS app_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL); INSERT OR IGNORE INTO app_settings VALUES ('collection_mode','all');")?;
            let value: String = conn.query_row("SELECT value FROM app_settings WHERE key='collection_mode'", [], |row| row.get(0))?;
            match value.as_str() { "all" => Ok(CollectionMode::All), "live_only" => Ok(CollectionMode::LiveOnly), _ => Err(StoreError::Invalid) }
        }).await?;
        let (mode, _) = watch::channel(mode);
        let started_at = crate::store::now();
        let instance_id = format!("{}-{}", std::process::id(), started_at);
        let info = json!({"instance_id":instance_id,"pid":std::process::id(),"executable":std::env::current_exe().ok().map(|p|absolute(&p)),"database":absolute(store.path()),"started_at":started_at,"port":port});
        Ok(Self {
            store,
            mode,
            shutdown,
            info,
        })
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/runtime", get(runtime_info))
            .route("/api/settings", get(settings).patch(update_settings))
            .route("/api/shutdown", axum::routing::post(shutdown))
            .with_state(self.clone())
    }

    pub async fn run_policy(
        self,
        metadata: Metadata,
        mut enabled: watch::Receiver<Vec<String>>,
        selected: watch::Sender<Vec<String>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut modes = self.mode.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *shutdown.borrow() {
                break;
            }
            let requested = enabled.borrow_and_update().clone();
            let mode = *modes.borrow_and_update();
            let states = metadata.channel_states().await.unwrap_or_default();
            let effective = select_channels(
                requested,
                mode,
                states
                    .iter()
                    .map(|state| (state.channel.as_str(), state.state.as_str(), state.stale)),
            );
            selected.send_if_modified(|current| {
                if *current == effective {
                    false
                } else {
                    *current = effective;
                    true
                }
            });
            tokio::select! {
                _ = timer.tick() => {},
                result = enabled.changed() => {if result.is_err() {break;}},
                result = modes.changed() => {if result.is_err() {break;}},
                _ = shutdown.changed() => break,
            }
        }
    }
}

fn select_channels<'a>(
    requested: Vec<String>,
    mode: CollectionMode,
    states: impl Iterator<Item = (&'a str, &'a str, bool)>,
) -> Vec<String> {
    if mode == CollectionMode::All {
        return requested;
    }
    let offline: std::collections::HashSet<_> = states
        .filter(|(_, state, stale)| *state == "offline" && !*stale)
        .map(|(channel, _, _)| channel)
        .collect();
    requested
        .into_iter()
        .filter(|channel| !offline.contains(channel.as_str()))
        .collect()
}

async fn runtime_info(State(runtime): State<Runtime>) -> Json<Value> {
    Json(runtime.info)
}
async fn settings(State(runtime): State<Runtime>) -> Json<Value> {
    Json(
        json!({"collection_mode":*runtime.mode.borrow(),"unknown_policy":"collect","source":"twitch_irc","sampling_mode":"event_driven","scope":"Live-only pauses a channel only after a fresh successful offline check. Unknown or stale metadata keeps collection enabled; membership coverage remains unknown."}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsUpdate {
    collection_mode: CollectionMode,
}
async fn update_settings(
    State(runtime): State<Runtime>,
    body: Result<Json<SettingsUpdate>, JsonRejection>,
) -> ApiResult {
    let Json(body) = body.map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_settings",
            "Choose all or live_only collection mode.",
        )
    })?;
    let value = match body.collection_mode {
        CollectionMode::All => "all",
        CollectionMode::LiveOnly => "live_only",
    };
    // Serialize persistence and publication through the same storage job. Concurrent requests
    // cannot publish an older mode after a later transaction has completed.
    let modes = runtime.mode.clone();
    runtime
        .store
        .call_write(Operation::SettingsWrite, move |conn| {
            conn.execute(
                "UPDATE app_settings SET value=?1 WHERE key='collection_mode'",
                [value],
            )?;
            modes.send_replace(body.collection_mode);
            Ok(())
        })
        .await
        .map_err(storage_error)?;
    Ok(settings(State(runtime)).await)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shutdown {
    instance_id: Option<String>,
}
async fn shutdown(
    State(runtime): State<Runtime>,
    body: Result<Json<Shutdown>, JsonRejection>,
) -> ApiResult {
    let Json(body) = body.map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "Use a JSON object for graceful shutdown.",
        )
    })?;
    if body
        .instance_id
        .as_ref()
        .is_some_and(|id| Some(id.as_str()) != runtime.info["instance_id"].as_str())
    {
        return Err(error(
            StatusCode::CONFLICT,
            "instance_changed",
            "The selected application instance changed; refresh its identity before stopping.",
        ));
    }
    runtime.shutdown.send_replace(true);
    Ok(Json(
        json!({"state":"stopping","instance_id":runtime.info["instance_id"]}),
    ))
}

/// Available bytes on the database's local volume; unsupported platforms report unknown.
pub fn disk_available(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let path = absolute(path);
        let directory = path.parent()?;
        let wide = directory
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut available = 0u64;
        // SAFETY: the directory is NUL-terminated; only the supplied writable u64 is used.
        let success = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (success != 0).then_some(available)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    #[test]
    fn live_policy_pauses_only_fresh_offline_and_keeps_unknown() {
        let requested = vec![
            "live".into(),
            "offline".into(),
            "stale".into(),
            "unknown".into(),
            "unchecked".into(),
        ];
        let states = [
            ("live", "live", false),
            ("offline", "offline", false),
            ("stale", "offline", true),
            ("unknown", "unknown", false),
        ];
        assert_eq!(
            select_channels(
                requested.clone(),
                CollectionMode::LiveOnly,
                states.into_iter()
            ),
            vec!["live", "stale", "unknown", "unchecked"]
        );
        assert_eq!(
            select_channels(requested.clone(), CollectionMode::All, states.into_iter()),
            requested
        );
        assert_eq!(
            select_channels(
                requested.clone(),
                CollectionMode::LiveOnly,
                std::iter::empty()
            ),
            requested
        );
    }

    #[tokio::test]
    async fn settings_persist_validate_and_shutdown_checks_identity() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("test.sqlite3"))
            .await
            .unwrap();
        let (stop, mut stopped) = watch::channel(false);
        let runtime = Runtime::initialize(store.clone(), stop, 8794)
            .await
            .unwrap();
        let router = runtime.router();
        let request = |path: &str, body: &str| {
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap()
        };
        assert_eq!(
            router
                .clone()
                .oneshot(request(
                    "/api/settings",
                    r#"{"collection_mode":"live_only"}"#
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let restored = Runtime::initialize(store, watch::channel(false).0, 8794)
            .await
            .unwrap();
        assert_eq!(*restored.mode.borrow(), CollectionMode::LiveOnly);
        assert_eq!(
            router
                .clone()
                .oneshot(request(
                    "/api/settings",
                    r#"{"collection_mode":"unsupported"}"#
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let stop_request = |body: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/shutdown")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap()
        };
        assert_eq!(
            router
                .clone()
                .oneshot(stop_request(r#"{"instance_id":"wrong"}"#))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert!(!*stopped.borrow());
        let response = router.oneshot(stop_request("{}")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["state"],
            "stopping"
        );
        stopped.changed().await.unwrap();
        assert!(*stopped.borrow());
    }
}
