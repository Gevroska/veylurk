//! Optional serial Community sampling. IRC and Community retain separate evidence models.
use crate::store::{self, *};
use axum::{
    extract::{rejection::JsonRejection, rejection::QueryRejection, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{SecondsFormat, Utc};
use community_source::host::{HostEvent, HostHandle, HostOptions, SourceSample};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot, watch, RwLock},
    time::Instant,
};

const START_GAP: Duration = Duration::from_secs(30);
const JOB_TIMEOUT: Duration = Duration::from_secs(85);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Serialize)]
pub struct Status {
    pub settings: CommunitySettings,
    pub state: &'static str,
    pub paused: bool,
    pub last_error: Option<&'static str>,
    pub active_channel: Option<String>,
    pub next_channel: Option<String>,
    pub next_due_at_utc: Option<String>,
    pub last_received_at_utc: Option<String>,
    pub selected_channels: usize,
    pub delayed_channels: usize,
    pub global_start_gap_seconds: u64,
    pub achieved_interval_seconds: Option<f64>,
    pub last_returned_accounts: Option<usize>,
    pub cleanup_confirmed: Option<bool>,
}

enum Command {
    Settings(CommunitySettings),
    Login,
    Resume,
}
type Reply = oneshot::Sender<Result<(), &'static str>>;

#[derive(Clone)]
pub struct Community {
    store: Store,
    status: Arc<RwLock<Status>>,
    commands: mpsc::Sender<(Command, Reply)>,
}

pub struct Coordinator {
    service: Community,
    commands: mpsc::Receiver<(Command, Reply)>,
    profile_dir: PathBuf,
    host: Option<HostHandle>,
    ready: bool,
    run_id: Option<i64>,
    pending: Option<Pending>,
    settings: CommunitySettings,
    paused: bool,
    persisted_pause: Option<CommunityPauseReason>,
    schedule: BTreeMap<String, Schedule>,
    next_start: Instant,
    job_id: u64,
    retiring: bool,
    shutdown: Option<watch::Receiver<bool>>,
    #[cfg(test)]
    host_start_attempts: usize,
}
struct Pending {
    job_id: u64,
    sample_id: i64,
    channel: String,
    started: Instant,
    cancelled: bool,
    started_at_utc: String,
}
struct Schedule {
    due: Instant,
    failures: u32,
    last_received: Option<Instant>,
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(status: StatusCode, code: &str) -> (StatusCode, Json<Value>) {
    let message = match code {
        "invalid_query" => "Check the Community filters and settings.",
        "storage_unavailable" => "Community history or settings could not be saved or read.",
        "busy" => "Community controls are busy. Try again shortly.",
        "disabled" => "Enable Community sampling before resuming.",
        "cleanup_unconfirmed" => {
            "The browser did not confirm shutdown. Restart Veylurk before resuming."
        }
        _ => "The Community source is unavailable. Check its status.",
    };
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
}
fn storage_error(e: StoreError) -> (StatusCode, Json<Value>) {
    match e {
        StoreError::Invalid => error(StatusCode::BAD_REQUEST, "invalid_query"),
        StoreError::NotFound => error(StatusCode::NOT_FOUND, "invalid_query"),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}

impl Community {
    pub async fn initialize(
        store: Store,
        profile_dir: PathBuf,
    ) -> Result<(Self, Coordinator), StoreError> {
        let settings = store.community_settings().await?;
        let persisted_pause = store.community_pause().await?;
        let status = Arc::new(RwLock::new(Status {
            settings,
            state: if persisted_pause.is_some() {
                "paused"
            } else if settings.enabled {
                "starting"
            } else {
                "disabled"
            },
            paused: persisted_pause.is_some(),
            last_error: persisted_pause.map(|reason| reason.code()),
            active_channel: None,
            next_channel: None,
            next_due_at_utc: None,
            last_received_at_utc: None,
            selected_channels: 0,
            delayed_channels: 0,
            global_start_gap_seconds: START_GAP.as_secs(),
            achieved_interval_seconds: None,
            last_returned_accounts: None,
            cleanup_confirmed: None,
        }));
        let (commands, receiver) = mpsc::channel(8);
        let service = Self {
            store,
            status,
            commands,
        };
        let runner = Coordinator {
            service: service.clone(),
            commands: receiver,
            profile_dir,
            host: None,
            ready: false,
            run_id: None,
            pending: None,
            settings,
            paused: persisted_pause.is_some(),
            persisted_pause,
            schedule: BTreeMap::new(),
            next_start: Instant::now(),
            job_id: 0,
            retiring: false,
            shutdown: None,
            #[cfg(test)]
            host_start_attempts: 0,
        };
        Ok((service, runner))
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/community/status", get(status))
            .route(
                "/api/community/settings",
                get(settings).patch(update_settings),
            )
            .route("/api/community/login", post(login))
            .route("/api/community/resume", post(resume))
            .route("/api/community/receipts", get(receipts))
            .with_state(self.clone())
    }
    async fn command(&self, command: Command) -> ApiResult {
        let (reply, response) = oneshot::channel();
        self.commands
            .try_send((command, reply))
            .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "busy"))?;
        match tokio::time::timeout(Duration::from_secs(14), response).await {
            Ok(Ok(Ok(()))) => Ok(Json(json!({"accepted":true}))),
            Ok(Ok(Err(code))) => Err(error(StatusCode::SERVICE_UNAVAILABLE, code)),
            _ => Err(error(StatusCode::SERVICE_UNAVAILABLE, "busy")),
        }
    }
}

async fn status(State(service): State<Community>) -> Json<Status> {
    Json(service.status.read().await.clone())
}
async fn settings(State(service): State<Community>) -> ApiResult {
    Ok(Json(json!(service
        .store
        .community_settings()
        .await
        .map_err(storage_error)?)))
}
async fn update_settings(
    State(service): State<Community>,
    input: Result<Json<CommunitySettings>, JsonRejection>,
) -> ApiResult {
    let Json(input) = input.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_query"))?;
    if !(30..=86_400).contains(&input.target_interval_seconds) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_query"));
    }
    let _ = service.command(Command::Settings(input)).await?;
    Ok(Json(json!(input)))
}
async fn login(
    State(service): State<Community>,
    input: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    empty_body(input)?;
    service.command(Command::Login).await
}
async fn resume(
    State(service): State<Community>,
    input: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    empty_body(input)?;
    service.command(Command::Resume).await
}
fn empty_body(input: Result<Json<Value>, JsonRejection>) -> Result<(), (StatusCode, Json<Value>)> {
    match input {
        Ok(Json(Value::Object(object))) if object.is_empty() => Ok(()),
        _ => Err(error(StatusCode::BAD_REQUEST, "invalid_query")),
    }
}
async fn receipts(
    State(service): State<Community>,
    query: Result<Query<CommunityReceiptQuery>, QueryRejection>,
) -> ApiResult {
    let Query(query) = query.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_query"))?;
    Ok(Json(json!(service
        .store
        .community_receipts(query)
        .await
        .map_err(storage_error)?)))
}

impl Coordinator {
    pub async fn run(
        mut self,
        mut selected: watch::Receiver<Vec<String>>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), StoreError> {
        self.shutdown = Some(shutdown.clone());
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let result = async {
            loop {
                if *shutdown.borrow() { break; }
                self.sync_channels(&selected.borrow_and_update());
                self.events().await?;
                self.step().await?;
                self.refresh_status().await;
                tokio::select! {
                    _ = timer.tick() => {},
                    changed = selected.changed() => { if changed.is_err() { break; } },
                    changed = shutdown.changed() => { if changed.is_err() { break; } },
                    command = self.commands.recv() => {
                        let Some((command, reply)) = command else { break; };
                        if reply.is_closed() { continue; }
                        if matches!(command, Command::Resume) && !self.settings.enabled {
                            let _ = reply.send(Err("disabled"));
                            continue;
                        }
                        if !matches!(command, Command::Settings(CommunitySettings { enabled: false, .. }))
                            && self.service.status.read().await.cleanup_confirmed == Some(false)
                        {
                            let _ = reply.send(Err("cleanup_unconfirmed"));
                            continue;
                        }
                        let answer = self.control(command).await;
                        let fatal = answer.is_err();
                        let _ = reply.send(answer.as_ref().map(|_| ()).map_err(|_| "storage_unavailable"));
                        if fatal { answer?; }
                    }
                }
            }
            Ok(())
        }.await;
        let cleanup = self.retire(CommunityRunEndReason::Shutdown).await;
        let mut status = self.service.status.write().await;
        status.state = if result.is_err() || cleanup.is_err() {
            "storage_error"
        } else {
            "stopped"
        };
        result.and(cleanup)
    }

    fn sync_channels(&mut self, channels: &[String]) {
        self.schedule
            .retain(|channel, _| channels.contains(channel));
        let now = Instant::now();
        for channel in channels {
            self.schedule.entry(channel.clone()).or_insert(Schedule {
                due: now,
                failures: 0,
                last_received: None,
            });
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !channels.contains(&pending.channel) && !pending.cancelled)
        {
            if let Some(host) = &self.host {
                host.cancel();
            }
            if let Some(pending) = &mut self.pending {
                pending.cancelled = true;
            }
        }
    }

    async fn pause(&mut self, code: &'static str) {
        self.paused = true;
        let mut status = self.service.status.write().await;
        status.last_error = Some(code);
        status.paused = true;
        status.state = "paused";
    }

    async fn pause_source(&mut self, code: &'static str) -> Result<(), StoreError> {
        let reason = match code {
            "authentication_denied" | "signed_in_ui_lost" => {
                CommunityPauseReason::AuthenticationDenied
            }
            "integrity_denied" => CommunityPauseReason::IntegrityDenied,
            "challenge_denied" => CommunityPauseReason::ChallengeDenied,
            "rate_limited" => CommunityPauseReason::RateLimited,
            _ => CommunityPauseReason::SourceDenied,
        };
        if self.persisted_pause != Some(reason) {
            self.service.store.set_community_pause(Some(reason)).await?;
            self.persisted_pause = Some(reason);
        }
        self.pause(code).await;
        Ok(())
    }

    async fn ensure_host(&mut self) -> bool {
        if self.host.is_some() {
            return true;
        }
        if self.service.status.read().await.cleanup_confirmed == Some(false) {
            return false;
        }
        if self.stopping() {
            return false;
        }
        #[cfg(test)]
        {
            self.host_start_attempts += 1;
        }
        match start_host(HostOptions {
            profile_dir: self.profile_dir.clone(),
        }) {
            Ok(host) => {
                self.host = Some(host);
                self.ready = false;
                self.service.status.write().await.cleanup_confirmed = None;
                true
            }
            Err(_) => {
                self.pause("host_unavailable").await;
                false
            }
        }
    }

    async fn control(&mut self, command: Command) -> Result<(), StoreError> {
        match command {
            Command::Settings(settings) => {
                self.service.store.set_community_settings(settings).await?;
                let was_enabled = self.settings.enabled;
                self.settings = settings;
                if !settings.enabled {
                    self.retire(CommunityRunEndReason::Disabled).await?;
                }
                if settings.enabled && !was_enabled {
                    // Enabling does not resume an owner-operated sign-in or a denial pause.
                    self.service.status.write().await.state =
                        if self.paused { "paused" } else { "starting" };
                }
                self.service.status.write().await.settings = settings;
            }
            Command::Login => {
                self.paused = true;
                self.retire(CommunityRunEndReason::SignIn).await?;
                if self.stopping() {
                    return Ok(());
                }
                if self.ensure_host().await {
                    if self.stopping() {
                        return Ok(());
                    }
                    if self
                        .host
                        .as_ref()
                        .is_some_and(|host| host.show_login().is_err())
                    {
                        self.pause("host_unavailable").await;
                    } else {
                        self.service.status.write().await.state = "sign_in";
                    }
                }
            }
            Command::Resume => {
                if !self.settings.enabled {
                    self.pause("disabled").await;
                    return Ok(());
                }
                self.retire(CommunityRunEndReason::Paused).await?;
                if self.service.status.read().await.cleanup_confirmed == Some(false) {
                    self.pause("cleanup_unconfirmed").await;
                    return Ok(());
                }
                self.paused = false;
                self.service.store.set_community_pause(None).await?;
                self.persisted_pause = None;
                self.next_start = Instant::now().max(self.next_start);
                self.service.status.write().await.last_error = None;
                self.service.status.write().await.state = "starting";
            }
        }
        Ok(())
    }

    async fn step(&mut self) -> Result<(), StoreError> {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.started.elapsed() >= JOB_TIMEOUT)
        {
            self.pause("job_timeout").await;
            self.retire(CommunityRunEndReason::HostFailure).await?;
        }
        if !self.settings.enabled
            || self.paused
            || self.pending.is_some()
            || self.schedule.is_empty()
        {
            return Ok(());
        }
        if !self.ensure_host().await || !self.ready || Instant::now() < self.next_start {
            return Ok(());
        }
        let Some(channel) = next_channel(&self.schedule, Instant::now()) else {
            return Ok(());
        };
        if !self.prepare_sample(channel.clone()).await? {
            return Ok(());
        }
        let now = Instant::now();
        self.next_start = now + START_GAP;
        if let Some(schedule) = self.schedule.get_mut(&channel) {
            schedule.due = now + Duration::from_secs(self.settings.target_interval_seconds.into());
        }
        if self
            .host
            .as_ref()
            .is_none_or(|host| host.try_sample(self.job_id, &channel).is_err())
        {
            self.finish_failure(CommunityFailureCode::HostUnavailable, false)
                .await?;
            self.pause("host_unavailable").await;
        }
        Ok(())
    }

    async fn prepare_sample(&mut self, channel: String) -> Result<bool, StoreError> {
        let run_id = match self.run_id {
            Some(id) => id,
            None => {
                let id = self
                    .service
                    .store
                    .begin_community_run(format!("veylurk-{}", env!("CARGO_PKG_VERSION")))
                    .await?;
                self.run_id = Some(id);
                id
            }
        };
        if self.stopping() {
            return Ok(false);
        }
        self.job_id = self.job_id.checked_add(1).ok_or(StoreError::Invalid)?;
        let started_at_utc = store::now();
        let sample_id = self
            .service
            .store
            .begin_community_sample(NewCommunitySample {
                run_id,
                job_id: self.job_id,
                channel: channel.clone(),
                started_at_utc: started_at_utc.clone(),
            })
            .await?;
        let now = Instant::now();
        self.pending = Some(Pending {
            job_id: self.job_id,
            sample_id,
            channel: channel.clone(),
            started: now,
            cancelled: false,
            started_at_utc,
        });
        if self.stopping() {
            self.finish_failure(CommunityFailureCode::HostFailure, true)
                .await?;
            return Ok(false);
        }
        Ok(true)
    }

    async fn events(&mut self) -> Result<(), StoreError> {
        // Native status is bounded, and one outstanding job has one reserved completion slot.
        for _ in 0..8 {
            let Some(event) = self.host.as_ref().and_then(|host| host.try_event().ok()) else {
                break;
            };
            self.event(event).await?;
        }
        Ok(())
    }

    async fn event(&mut self, event: HostEvent) -> Result<(), StoreError> {
        match event {
            HostEvent::Ready => self.ready = true,
            HostEvent::LoginShown => {
                self.paused = true;
                self.service.status.write().await.state = "sign_in";
            }
            HostEvent::Paused { code } => self.pause_source(code).await?,
            HostEvent::SampleCompleted {
                job_id,
                channel,
                outcome,
            } => {
                if !self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.job_id == job_id && pending.channel == channel)
                {
                    self.pause("job_identity_mismatch").await;
                    if let Some(pending) = &mut self.pending {
                        pending.cancelled = true;
                    }
                    if let Some(host) = &self.host {
                        host.shutdown();
                    }
                    return Ok(());
                }
                // Persist a denial before any terminal/cancelled attempt write.
                // A crash at either later write must still require explicit Resume.
                if let Err(failure) = &outcome {
                    if failure.pauses_community {
                        self.pause_source(failure.code).await?;
                    }
                }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.cancelled)
                {
                    self.finish_failure(CommunityFailureCode::HostFailure, true)
                        .await?;
                    return Ok(());
                }
                match outcome {
                    Ok(sample) => self.finish_success(sample).await?,
                    Err(failure) => {
                        self.finish_failure(
                            failure_code(failure.code),
                            failure.code == "operator_cancelled",
                        )
                        .await?;
                        self.service.status.write().await.last_error = Some(failure.code);
                    }
                }
            }
            HostEvent::Stopped { cleanup_confirmed } => {
                self.host = None;
                self.ready = false;
                self.service.status.write().await.cleanup_confirmed = Some(cleanup_confirmed);
                self.finish_failure(CommunityFailureCode::HostFailure, false)
                    .await?;
                if !self.retiring || !cleanup_confirmed {
                    let code = if cleanup_confirmed {
                        "browser_closed"
                    } else {
                        "cleanup_unconfirmed"
                    };
                    if cleanup_confirmed {
                        self.pause(code).await;
                    } else {
                        self.pause_source(code).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn finish_success(&mut self, sample: SourceSample) -> Result<(), StoreError> {
        let Some(pending) = self.pending.as_ref() else {
            return Err(StoreError::Invalid);
        };
        let received_at_utc = chrono::DateTime::from_timestamp_millis(sample.received_at_unix_ms)
            .map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, true));
        let count = sample.members.len();
        let projection = received_at_utc
            .as_ref()
            .ok_or(StoreError::Invalid)
            .and_then(|at| completion(sample, at.clone()));
        let saved = match projection {
            Ok(projection) => {
                self.service
                    .store
                    .complete_community_sample(pending.sample_id, projection)
                    .await
            }
            Err(error) => Err(error),
        };
        match saved {
            Ok(()) => {}
            Err(StoreError::Invalid | StoreError::NotFound) => {
                self.pause_source("invalid_source_sample").await?;
                self.finish_failure(CommunityFailureCode::InvalidShape, false)
                    .await?;
                if let Some(host) = &self.host {
                    host.cancel();
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        let pending = self.pending.take().ok_or(StoreError::Invalid)?;
        self.next_start = self.next_start.max(Instant::now() + START_GAP);
        let mut status = self.service.status.write().await;
        status.last_received_at_utc = received_at_utc;
        status.last_returned_accounts = Some(count);
        status.last_error = None;
        if let Some(schedule) = self.schedule.get_mut(&pending.channel) {
            let now = Instant::now();
            status.achieved_interval_seconds = schedule
                .last_received
                .map(|last| now.duration_since(last).as_secs_f64());
            schedule.last_received = Some(now);
            schedule.failures = 0;
        }
        Ok(())
    }

    async fn finish_failure(
        &mut self,
        code: CommunityFailureCode,
        cancelled: bool,
    ) -> Result<(), StoreError> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(());
        };
        // A backwards wall clock invalidates source timing, but cannot prevent
        // recording a failure or make healthy IRC storage appear unavailable.
        let completed_at_utc = store::now().max(pending.started_at_utc.clone());
        let result = if cancelled {
            CommunityCompletion::Cancelled { completed_at_utc }
        } else {
            CommunityCompletion::Failed {
                completed_at_utc,
                code,
            }
        };
        self.service
            .store
            .complete_community_sample(pending.sample_id, result)
            .await?;
        let pending = self.pending.take().ok_or(StoreError::Invalid)?;
        self.next_start = self.next_start.max(Instant::now() + START_GAP);
        if let Some(schedule) = self.schedule.get_mut(&pending.channel) {
            schedule.failures = schedule.failures.saturating_add(1);
            let delay = 30u64
                .saturating_mul(1u64 << schedule.failures.min(6))
                .min(1800);
            schedule.due = schedule
                .due
                .max(Instant::now() + Duration::from_secs(delay));
        }
        Ok(())
    }

    async fn retire(&mut self, reason: CommunityRunEndReason) -> Result<(), StoreError> {
        self.retiring = true;
        if let Some(pending) = &mut self.pending {
            pending.cancelled = true;
        }
        if let Some(host) = &self.host {
            host.shutdown();
        }
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        while self.host.is_some() && Instant::now() < deadline {
            self.events().await?;
            if self.host.is_some() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        if self.host.take().is_some() {
            self.service.status.write().await.cleanup_confirmed = Some(false);
            self.paused = true;
        }
        self.ready = false;
        self.finish_failure(CommunityFailureCode::HostFailure, true)
            .await?;
        if let Some(run_id) = self.run_id {
            self.service.store.end_community_run(run_id, reason).await?;
            self.run_id = None;
        }
        self.retiring = false;
        Ok(())
    }

    fn stopping(&self) -> bool {
        self.shutdown
            .as_ref()
            .is_some_and(|shutdown| *shutdown.borrow())
    }

    async fn refresh_status(&mut self) {
        let now = Instant::now();
        let next = self
            .schedule
            .iter()
            .min_by_key(|(channel, schedule)| (schedule.due, *channel));
        let mut status = self.service.status.write().await;
        status.settings = self.settings;
        status.paused = self.paused;
        status.active_channel = self.pending.as_ref().map(|pending| pending.channel.clone());
        status.selected_channels = self.schedule.len();
        status.delayed_channels = self
            .schedule
            .values()
            .filter(|schedule| schedule.due < now && now.duration_since(schedule.due) >= START_GAP)
            .count();
        status.next_channel = next.map(|(channel, _)| channel.clone());
        status.next_due_at_utc = if self.settings.enabled && !self.paused {
            next.map(|(_, schedule)| {
                let delay = schedule
                    .due
                    .max(self.next_start)
                    .saturating_duration_since(now);
                (Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default())
                    .to_rfc3339_opts(SecondsFormat::Millis, true)
            })
        } else {
            None
        };
        if status.state != "sign_in" || !self.settings.enabled && self.host.is_none() {
            status.state = if !self.settings.enabled {
                "disabled"
            } else if self.paused {
                "paused"
            } else if self.pending.is_some() {
                "sampling"
            } else if self.schedule.is_empty() {
                "idle"
            } else if !self.ready {
                "starting"
            } else {
                "waiting"
            };
        }
    }
}

fn next_channel(schedule: &BTreeMap<String, Schedule>, now: Instant) -> Option<String> {
    schedule
        .iter()
        .filter(|(_, schedule)| schedule.due <= now)
        .min_by_key(|(channel, schedule)| (schedule.due, *channel))
        .map(|(channel, _)| channel.clone())
}

fn completion(
    sample: SourceSample,
    received_at_utc: String,
) -> Result<CommunityCompletion, StoreError> {
    let p = sample.provenance;
    let [broadcasters, staff, moderators, vips, chatbots, viewers]: [u32; 6] = sample
        .category_counts
        .map(u32::try_from)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Invalid)?
        .try_into()
        .map_err(|_| StoreError::Invalid)?;
    let transport = match p.transport {
        "network_response" => CommunityTransport::NetworkResponse,
        "cache_or_service_worker" => CommunityTransport::CacheOrServiceWorker,
        "unknown" => CommunityTransport::Unknown,
        _ => return Err(StoreError::Invalid),
    };
    Ok(CommunityCompletion::Success {
        received_at_utc,
        reported_chatters: sample.reported_chatters,
        members: sample
            .members
            .into_iter()
            .map(|member| CommunityMemberInput {
                account: member.account,
                roles: member.roles.into(),
            })
            .collect(),
        categories: CommunityCategoryCounts {
            broadcasters,
            staff,
            moderators,
            vips,
            chatbots,
            viewers,
        },
        provenance: CommunityProvenance {
            request_after_trigger_verified: p.request_after_trigger_verified,
            request_after_trigger_ms: p.request_after_trigger_ms,
            request_to_response_ms: p.request_to_response_ms,
            request_to_finished_ms: p.request_to_finished_ms,
            finished_minus_response_ms: p.finished_minus_response_ms,
            from_disk_cache: p.from_disk_cache,
            from_service_worker: p.from_service_worker,
            from_prefetch_cache: p.from_prefetch_cache,
            served_from_cache_event: p.served_from_cache_event,
            encoded_bytes: p.encoded_bytes,
            transport,
            server_freshness: CommunityServerFreshness::Unknown,
            http_status: sample.http_status,
        },
    })
}

fn failure_code(code: &str) -> CommunityFailureCode {
    match code {
        "authentication_denied" | "signed_in_ui_lost" => CommunityFailureCode::AuthenticationDenied,
        "integrity_denied" => CommunityFailureCode::IntegrityDenied,
        "challenge_denied" => CommunityFailureCode::ChallengeDenied,
        "rate_limited" => CommunityFailureCode::RateLimited,
        "http_denied" => CommunityFailureCode::HttpDenied,
        "http_error" => CommunityFailureCode::HttpError,
        "request_size_limit" => CommunityFailureCode::RequestSizeLimit,
        "response_size_limit" => CommunityFailureCode::ResponseSizeLimit,
        "invalid_json" => CommunityFailureCode::InvalidJson,
        "ambiguous_source" => CommunityFailureCode::AmbiguousSource,
        "source_errors" => CommunityFailureCode::SourceErrors,
        "missing_chatters" => CommunityFailureCode::MissingChatters,
        "member_limit" => CommunityFailureCode::MemberLimit,
        "verified_player_pause_lost" => CommunityFailureCode::PlayerNotPaused,
        "navigation_failed" => CommunityFailureCode::NavigationFailed,
        code if code.contains("timeout") || code.contains("deadline") => {
            CommunityFailureCode::Timeout
        }
        _ => CommunityFailureCode::HostFailure,
    }
}

#[cfg(test)]
#[path = "community_tests.rs"]
mod tests;

fn start_host(options: HostOptions) -> Result<HostHandle, community_source::host::HostError> {
    #[cfg(not(test))]
    {
        HostHandle::spawn(options)
    }
    #[cfg(test)]
    {
        // Coordinator tests never start a browser or access Twitch, even on regression.
        let _ = options;
        Err(community_source::host::HostError::Unsupported)
    }
}
