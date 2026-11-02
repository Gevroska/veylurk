//! Optional serial Community sampling. IRC and Community retain separate evidence models.
use crate::store::{self, *};
use axum::{
    extract::{rejection::JsonRejection, rejection::QueryRejection, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{SecondsFormat, Utc};
use community_source::host::{
    CleanupDiagnostics, FaultReport, HostEvent, HostHandle, HostOptions, HostTermination,
    PrimaryFailure, SourceSample, WindowDismissal, WorkerDiagnostics,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot, watch, RwLock},
    time::Instant,
};

const START_GAP: Duration = Duration::from_secs(30);
const JOB_TIMEOUT: Duration = Duration::from_secs(85);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(14);

fn worker_retry_delay(attempts: u32) -> Duration {
    Duration::from_secs(
        5u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(4))
            .min(60),
    )
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub settings: CommunitySettings,
    pub state: &'static str,
    pub paused: bool,
    pub persisted_pause_reason: Option<CommunityPauseReason>,
    pub last_error: Option<&'static str>,
    pub active_channel: Option<String>,
    pub next_channel: Option<String>,
    pub next_due_at_utc: Option<String>,
    pub last_received_at_utc: Option<String>,
    pub last_received_channel: Option<String>,
    pub selected_channels: usize,
    pub delayed_channels: usize,
    pub global_start_gap_seconds: u64,
    pub achieved_interval_seconds: Option<f64>,
    pub last_returned_accounts: Option<usize>,
    pub cleanup_confirmed: Option<bool>,
    pub primary_failure: Option<PrimaryFailure>,
    pub cleanup_diagnostics: Option<CleanupDiagnostics>,
    pub host_termination: Option<HostTermination>,
    pub window_dismissal: Option<WindowDismissal>,
    pub worker: Option<WorkerDiagnostics>,
    pub worker_recovery: Option<WorkerRecoveryStatus>,
    pub channels: Vec<ChannelCadenceStatus>,
}

#[derive(Clone, Serialize)]
pub struct WorkerRecoveryStatus {
    pub state: &'static str,
    pub attempts: u32,
    pub next_retry_at_utc: Option<String>,
    pub cleanup_confirmed: bool,
    pub last_fault: FaultReport,
}

#[derive(Clone, Serialize)]
pub struct ChannelCadenceStatus {
    pub channel: String,
    pub effective_interval_seconds: u32,
    pub cadence_basis: CadenceBasis,
    pub reported_chatters: Option<u64>,
    pub viewer_category_rows: Option<u64>,
    pub achieved_interval_seconds: Option<f64>,
    pub failures: u32,
    pub next_eligible_at_utc: Option<String>,
    pub last_received_at_utc: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CadenceBasis {
    Fixed,
    Adaptive,
    NoSample,
    MissingReportedCount,
    ZeroReportedCount,
    NoViewerRows,
    InconsistentCounts,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunitySettingsUpdate {
    enabled: bool,
    target_interval_seconds: u32,
    cadence_mode: Option<CommunityCadenceMode>,
}

impl From<CommunitySettings> for CommunitySettingsUpdate {
    fn from(value: CommunitySettings) -> Self {
        Self {
            enabled: value.enabled,
            target_interval_seconds: value.target_interval_seconds,
            cadence_mode: Some(value.cadence_mode),
        }
    }
}

#[derive(Clone, Copy)]
enum ControlReply {
    Accepted,
    Settings(CommunitySettings),
}

enum Command {
    Settings(CommunitySettingsUpdate),
    Login,
    Resume,
}
type Reply = oneshot::Sender<Result<ControlReply, &'static str>>;

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
    // All recovery work runs in this coordinator, never in a detached timer.
    // Dropping a host also drops its private event receiver/generation.
    recovery_retry_at: Option<Instant>,
    worker_failures: u32,
    worker_fault_pending: bool,
    replacement_safe: bool,
    host_generation: u64,
    quarantine_observed: bool,
    quarantine_ticket: Option<u64>,
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
    cadence_due: Instant,
    retry_not_before: Instant,
    last_attempt: Option<Instant>,
    counts: Option<SampleCounts>,
    failures: u32,
    last_received: Option<Instant>,
    last_received_at_utc: Option<String>,
    achieved_interval_seconds: Option<f64>,
}

#[derive(Clone, Copy)]
struct SampleCounts {
    reported_chatters: Option<u64>,
    viewer_rows: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CadenceDecision {
    seconds: u32,
    basis: CadenceBasis,
}

impl Schedule {
    fn new(now: Instant) -> Self {
        Self {
            cadence_due: now,
            retry_not_before: now,
            last_attempt: None,
            counts: None,
            failures: 0,
            last_received: None,
            last_received_at_utc: None,
            achieved_interval_seconds: None,
        }
    }
    fn eligible(&self) -> Instant {
        self.cadence_due.max(self.retry_not_before)
    }
    fn rebase(&mut self, settings: CommunitySettings, now: Instant) {
        let Some(anchor) = self.last_attempt else {
            return;
        };
        let candidate = anchor + Duration::from_secs(cadence(settings, self.counts).seconds.into());
        // Changing settings never postpones an already overdue opportunity.
        self.cadence_due = if self.cadence_due <= now {
            self.cadence_due.min(candidate)
        } else {
            candidate
        };
    }
}

fn cadence(settings: CommunitySettings, counts: Option<SampleCounts>) -> CadenceDecision {
    let base = settings.target_interval_seconds;
    let fallback = |basis| CadenceDecision {
        seconds: base,
        basis,
    };
    if settings.cadence_mode == CommunityCadenceMode::Fixed {
        return fallback(CadenceBasis::Fixed);
    }
    let Some(counts) = counts else {
        return fallback(CadenceBasis::NoSample);
    };
    let Some(population) = counts.reported_chatters else {
        return fallback(CadenceBasis::MissingReportedCount);
    };
    if population == 0 {
        return fallback(CadenceBasis::ZeroReportedCount);
    }
    if counts.viewer_rows == 0 {
        return fallback(CadenceBasis::NoViewerRows);
    }
    if population < counts.viewer_rows {
        return fallback(CadenceBasis::InconsistentCounts);
    }
    // A scheduling heuristic only. These source counts do not validate a
    // population, independent sampling, coverage or an absence probability.
    let numerator = u128::from(base) * u128::from(counts.viewer_rows);
    let denominator = u128::from(population);
    let seconds = numerator.div_ceil(denominator).max(30);
    CadenceDecision {
        seconds: u32::try_from(seconds).expect("k <= N bounds the target by its validated base"),
        basis: CadenceBasis::Adaptive,
    }
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
        StoreError::ReadWaitTimeout => error(StatusCode::SERVICE_UNAVAILABLE, "read_wait_timeout"),
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
            persisted_pause_reason: persisted_pause,
            last_error: persisted_pause.map(|reason| reason.code()),
            active_channel: None,
            next_channel: None,
            next_due_at_utc: None,
            last_received_at_utc: None,
            last_received_channel: None,
            selected_channels: 0,
            delayed_channels: 0,
            global_start_gap_seconds: START_GAP.as_secs(),
            achieved_interval_seconds: None,
            last_returned_accounts: None,
            cleanup_confirmed: None,
            primary_failure: None,
            cleanup_diagnostics: None,
            host_termination: None,
            window_dismissal: None,
            worker: None,
            worker_recovery: None,
            channels: Vec::new(),
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
            recovery_retry_at: None,
            worker_failures: 0,
            worker_fault_pending: false,
            replacement_safe: false,
            host_generation: 0,
            quarantine_observed: false,
            quarantine_ticket: None,
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
        match tokio::time::timeout(CONTROL_REPLY_TIMEOUT, response).await {
            Ok(Ok(Ok(ControlReply::Accepted))) => Ok(Json(json!({"accepted":true}))),
            Ok(Ok(Ok(ControlReply::Settings(settings)))) => Ok(Json(json!(settings))),
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
    input: Result<Json<CommunitySettingsUpdate>, JsonRejection>,
) -> ApiResult {
    let Json(input) = input.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid_query"))?;
    if !(30..=86_400).contains(&input.target_interval_seconds) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_query"));
    }
    service.command(Command::Settings(input)).await
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
                        if !matches!(command, Command::Settings(CommunitySettingsUpdate { enabled: false, .. }))
                            && self.cleanup_blocks_host().await
                        {
                            if matches!(command, Command::Login) {
                                self.cancel_recovery().await;
                                self.worker_fault_pending = false;
                                self.paused = true;
                            }
                            let _ = reply.send(Err("cleanup_unconfirmed"));
                            continue;
                        }
                        let answer = self.control(command).await;
                        let fatal = answer.is_err();
                        let _ = reply.send(answer.as_ref().copied().map_err(|_| "storage_unavailable"));
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
            self.schedule
                .entry(channel.clone())
                .or_insert_with(|| Schedule::new(now));
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
        self.cancel_recovery().await;
        self.paused = true;
        let mut status = self.service.status.write().await;
        status.last_error = Some(code);
        status.paused = true;
        status.state = "paused";
    }

    async fn pause_source(&mut self, code: &'static str) -> Result<(), StoreError> {
        self.quarantine_ticket = None;
        self.worker_fault_pending = false;
        let requested = match code {
            "authentication_denied" | "signed_in_ui_lost" => {
                CommunityPauseReason::AuthenticationDenied
            }
            "integrity_denied" => CommunityPauseReason::IntegrityDenied,
            "challenge_denied" => CommunityPauseReason::ChallengeDenied,
            "rate_limited" => CommunityPauseReason::RateLimited,
            "cleanup_unconfirmed"
            | "controller_cleanup_unconfirmed"
            | "operator_cancelled"
            | "worker_failed"
            | "worker_start_failed"
            | "worker_protocol_failure"
            | "worker_stalled"
            | "worker_shutdown_timeout" => CommunityPauseReason::CleanupUnconfirmed,
            _ => CommunityPauseReason::SourceDenied,
        };
        // Retain the first source barrier, including historically ambiguous
        // source_denied values. Cleanup has its own independent diagnostics.
        let retained_source = self
            .persisted_pause
            .filter(|reason| *reason != CommunityPauseReason::CleanupUnconfirmed);
        let reason = retained_source.unwrap_or(requested);
        let pause_code = if let Some(retained) = retained_source {
            retained.code()
        } else if requested == CommunityPauseReason::CleanupUnconfirmed {
            requested.code()
        } else {
            code
        };
        // Close admission even if the durable barrier cannot be committed.
        // The caller propagates that storage failure and the coordinator stops.
        if requested == CommunityPauseReason::CleanupUnconfirmed {
            self.replacement_safe = false;
            self.service.status.write().await.cleanup_confirmed = Some(false);
        }
        self.pause(pause_code).await;
        if let Some(recovery) = &mut self.service.status.write().await.worker_recovery {
            if recovery.state == "cancelled" {
                recovery.state = "blocked";
            }
        }
        if self.persisted_pause != Some(reason) {
            self.service.store.set_community_pause(Some(reason)).await?;
            self.persisted_pause = Some(reason);
            self.service.status.write().await.persisted_pause_reason = Some(reason);
        }
        Ok(())
    }

    async fn ensure_host(&mut self) -> bool {
        if self.host.is_some() {
            return true;
        }
        if self.cleanup_blocks_host().await {
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
                self.host_generation = self.host_generation.wrapping_add(1);
                self.quarantine_observed = false;
                self.ready = false;
                self.replacement_safe = false;
                let mut status = self.service.status.write().await;
                status.cleanup_confirmed = None;
                status.host_termination = None;
                status.window_dismissal = None;
                status.worker = None;
                if let Some(recovery) = &mut status.worker_recovery {
                    if recovery.state == "backoff" {
                        recovery.state = "restarting";
                        recovery.next_retry_at_utc = None;
                    }
                }
                true
            }
            Err(_) => {
                if self.recovery_retry_at.is_some() {
                    // spawn returned no host, so it transferred no live owner.
                    self.worker_failures = self.worker_failures.saturating_add(1);
                    self.recovery_retry_at =
                        Some(Instant::now() + worker_retry_delay(self.worker_failures));
                    let mut status = self.service.status.write().await;
                    status.last_error = Some("host_unavailable");
                    if let Some(recovery) = &mut status.worker_recovery {
                        recovery.attempts = self.worker_failures;
                    }
                } else {
                    self.pause("host_unavailable").await;
                }
                false
            }
        }
    }

    async fn control(&mut self, command: Command) -> Result<ControlReply, StoreError> {
        match command {
            Command::Settings(update) => {
                // Resolve omitted legacy fields at dequeue time, after earlier commands.
                let settings = CommunitySettings {
                    enabled: update.enabled,
                    target_interval_seconds: update.target_interval_seconds,
                    cadence_mode: update.cadence_mode.unwrap_or(self.settings.cadence_mode),
                };
                self.service.store.set_community_settings(settings).await?;
                let was_enabled = self.settings.enabled;
                self.settings = settings;
                let now = Instant::now();
                for schedule in self.schedule.values_mut() {
                    schedule.rebase(settings, now);
                }
                if !settings.enabled {
                    self.retire(CommunityRunEndReason::Disabled).await?;
                }
                if settings.enabled && !was_enabled {
                    // Enabling does not resume an owner-operated sign-in or a denial pause.
                    self.service.status.write().await.state =
                        if self.paused { "paused" } else { "starting" };
                }
                self.service.status.write().await.settings = settings;
                return Ok(ControlReply::Settings(settings));
            }
            Command::Login => {
                self.paused = true;
                self.retire(CommunityRunEndReason::SignIn).await?;
                if self.stopping() {
                    return Ok(ControlReply::Accepted);
                }
                if self.ensure_host().await {
                    if self.stopping() {
                        return Ok(ControlReply::Accepted);
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
                    return Ok(ControlReply::Accepted);
                }
                self.retire(CommunityRunEndReason::Paused).await?;
                if self.cleanup_blocks_host().await {
                    self.pause_source("cleanup_unconfirmed").await?;
                    return Ok(ControlReply::Accepted);
                }
                self.service.store.set_community_pause(None).await?;
                self.persisted_pause = None;
                self.paused = false;
                self.next_start = Instant::now().max(self.next_start);
                self.service.status.write().await.persisted_pause_reason = None;
                self.service.status.write().await.last_error = None;
                self.service.status.write().await.state = "starting";
            }
        }
        Ok(ControlReply::Accepted)
    }

    async fn step(&mut self) -> Result<(), StoreError> {
        if self.stopping() {
            self.cancel_recovery().await;
            return Ok(());
        }
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
        if self.recovery_retry_at.is_some_and(|at| Instant::now() < at) {
            return Ok(());
        }
        if !self.ensure_host().await {
            return Ok(());
        }
        self.recovery_retry_at = None;
        if !self.ready || Instant::now() < self.next_start {
            return Ok(());
        }
        let Some(channel) = next_channel(&self.schedule, Instant::now()) else {
            return Ok(());
        };
        if !self.prepare_sample(channel.clone()).await? {
            return Ok(());
        }
        let now = self
            .pending
            .as_ref()
            .expect("durably prepared attempt")
            .started;
        self.next_start = now + START_GAP;
        if let Some(schedule) = self.schedule.get_mut(&channel) {
            schedule.last_attempt = Some(now);
            schedule.cadence_due =
                now + Duration::from_secs(cadence(self.settings, schedule.counts).seconds.into());
        }
        let dispatch = self
            .host
            .as_ref()
            .map(|host| host.try_sample(self.job_id, &channel));
        if !matches!(dispatch, Some(Ok(()))) {
            self.failed_dispatch(dispatch.and_then(Result::err)).await?;
        }
        Ok(())
    }

    async fn failed_dispatch(
        &mut self,
        error: Option<community_source::host::CommandError>,
    ) -> Result<(), StoreError> {
        if error == Some(community_source::host::CommandError::Closed) {
            // A crash or received denial may close admission while the durable
            // pending write is awaited. Preserve recovery intent until this
            // exact host's terminal proof is consumed.
            if let Some(report) = self.host.as_ref().map(HostHandle::fault_report) {
                self.record_fault(report).await?;
            }
            self.finish_failure(CommunityFailureCode::HostUnavailable, false)
                .await?;
            self.pause_event("worker_failed").await?;
        } else {
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
            if let Some(report) = self.host.as_ref().map(HostHandle::fault_report) {
                self.record_fault(report).await?;
            }
            let Some(event) = self.host.as_ref().and_then(|host| host.try_event().ok()) else {
                break;
            };
            self.event(event).await?;
        }
        Ok(())
    }

    async fn record_fault(&mut self, report: FaultReport) -> Result<(), StoreError> {
        // A received source denial outranks infrastructure recovery, even when
        // the coalescing Paused queue or native completion did not survive.
        let barrier = report
            .source_barrier
            .as_ref()
            .map(|barrier| barrier.code)
            .or_else(|| {
                report
                    .primary_failure
                    .as_ref()
                    .filter(|failure| community_source::host::is_source_denial(failure.code))
                    .map(|failure| failure.code)
            });
        if let Some(code) = barrier {
            self.pause_source(code).await?;
        }
        let unknown_worker = report.worker.as_ref().is_some_and(|worker| {
            worker.failure.is_some()
                && !worker_contained(worker)
                && worker.process_id.is_some()
                && worker.started_at_unix_ms.is_some()
        });
        if unknown_worker && !self.quarantine_observed {
            self.quarantine_observed = true;
            let allowed = (!self.paused || self.worker_fault_pending)
                && self.persisted_pause.is_none()
                && self.settings.enabled
                && !self.retiring
                && !self.stopping();
            if allowed {
                // This runtime ticket owns only the cleanup marker it installs.
                // A protected denial or owner control destroys the ticket; an
                // old/ambiguous startup marker can never acquire one.
                self.quarantine_ticket = Some(self.host_generation);
                self.worker_fault_pending = true;
                self.paused = true;
                self.replacement_safe = false;
                {
                    let mut status = self.service.status.write().await;
                    status.cleanup_confirmed = Some(false);
                    status.paused = true;
                    status.last_error = Some("cleanup_unconfirmed");
                    status.worker_recovery = Some(WorkerRecoveryStatus {
                        state: "awaiting_cleanup",
                        attempts: self.worker_failures.saturating_add(1),
                        next_retry_at_utc: None,
                        cleanup_confirmed: false,
                        last_fault: report.clone(),
                    });
                }
                self.service
                    .store
                    .set_community_pause(Some(CommunityPauseReason::CleanupUnconfirmed))
                    .await?;
                self.persisted_pause = Some(CommunityPauseReason::CleanupUnconfirmed);
                self.service.status.write().await.persisted_pause_reason = self.persisted_pause;
                if self.pending.is_none() {
                    self.end_run(CommunityRunEndReason::HostFailure).await?;
                }
            }
        }
        let mut status = self.service.status.write().await;
        if report.primary_failure.is_some()
            && !(status.primary_failure.is_some()
                && (self
                    .persisted_pause
                    .is_some_and(|reason| reason != CommunityPauseReason::CleanupUnconfirmed)
                    || report.primary_failure.as_ref().is_some_and(|failure| {
                        matches!(
                            failure.code,
                            "cleanup_unconfirmed"
                                | "controller_cleanup_unconfirmed"
                                | "operator_cancelled"
                        )
                    })))
        {
            status.primary_failure = report.primary_failure;
        }
        if report.cleanup_diagnostics.is_some() {
            status.cleanup_diagnostics = report.cleanup_diagnostics;
        }
        if report.host_termination.is_some() {
            status.host_termination = report.host_termination;
        }
        if report.window_dismissal.is_some() {
            status.window_dismissal = report.window_dismissal;
        }
        if report.worker.is_some() {
            status.worker = report.worker;
        }
        Ok(())
    }

    async fn event(&mut self, event: HostEvent) -> Result<(), StoreError> {
        match event {
            HostEvent::Ready => {
                self.ready = true;
                if let Some(recovery) = &mut self.service.status.write().await.worker_recovery {
                    if recovery.state == "restarting" {
                        recovery.state = "verifying";
                    }
                }
            }
            HostEvent::LoginShown => {
                self.paused = true;
                let mut status = self.service.status.write().await;
                if status.last_error == Some("sign_in_navigation_failed") {
                    status.last_error = self.persisted_pause.map(|reason| reason.code());
                }
                status.state = "sign_in";
            }
            HostEvent::LoginFailed => self.pause("sign_in_navigation_failed").await,
            HostEvent::Paused { code } => self.pause_event(code).await?,
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
                        self.pause_event(failure.code).await?;
                    }
                }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.cancelled)
                {
                    self.finish_failure(CommunityFailureCode::HostFailure, true)
                        .await?;
                    if self.worker_fault_pending && !self.retiring {
                        self.end_run(CommunityRunEndReason::HostFailure).await?;
                    }
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
                        if self.persisted_pause.is_none() {
                            self.service.status.write().await.last_error = Some(failure.code);
                        }
                    }
                }
                if self.worker_fault_pending && !self.retiring {
                    self.end_run(CommunityRunEndReason::HostFailure).await?;
                }
            }
            HostEvent::Stopped {
                cleanup_confirmed,
                diagnostics,
            } => {
                if let Some(termination) = &diagnostics.host_termination {
                    self.service
                        .store
                        .record_community_host_termination(termination.clone());
                }
                self.record_fault(diagnostics.clone()).await?;
                self.host = None;
                self.ready = false;
                self.service.status.write().await.cleanup_confirmed = Some(cleanup_confirmed);
                let contained_fault = diagnostics.worker.as_ref().is_some_and(worker_contained);
                let replaceable = contained_fault
                    || diagnostics.worker.as_ref().is_some_and(|worker| {
                        worker.failure == Some(community_source::host::WorkerFailure::NotCreated)
                            && worker.process_id.is_none()
                            && worker.started_at_unix_ms.is_none()
                            && worker.exit_code.is_none()
                            && !worker.worker_tree_exited
                            && !worker.containment_confirmed
                    });
                self.replacement_safe = cleanup_confirmed || replaceable;
                let own_cleanup = self.quarantine_ticket == Some(self.host_generation)
                    && self.persisted_pause == Some(CommunityPauseReason::CleanupUnconfirmed);
                let recovery_generation = self.host_generation;
                let mut recover = replaceable
                    && (!self.paused || self.worker_fault_pending)
                    && (self.persisted_pause.is_none() || own_cleanup)
                    && self.settings.enabled
                    && !self.retiring
                    && !self.stopping();
                self.worker_fault_pending = false;
                // Persist the cleanup barrier before attempt/run closure can fail.
                if !cleanup_confirmed && !replaceable {
                    self.pause_source("cleanup_unconfirmed").await?;
                }
                self.finish_failure(CommunityFailureCode::HostFailure, false)
                    .await?;
                // An unsolicited terminal ends coverage now. Retirement already
                // owns its explicit boundary and must retain its chosen reason.
                if !self.retiring {
                    self.end_run(CommunityRunEndReason::HostFailure).await?;
                }
                // Storage can yield while a shutdown signal arrives. Recheck
                // the same generation at the final recovery commit boundary.
                recover &= !self.stopping() && recovery_generation == self.host_generation;
                if recover && own_cleanup {
                    recover &= self.quarantine_ticket == Some(recovery_generation);
                    if recover {
                        self.service.store.set_community_pause(None).await?;
                        if self.stopping() {
                            self.service
                                .store
                                .set_community_pause(Some(CommunityPauseReason::CleanupUnconfirmed))
                                .await?;
                            recover = false;
                        } else {
                            self.persisted_pause = None;
                            self.service.status.write().await.persisted_pause_reason = None;
                        }
                    }
                }
                self.quarantine_ticket = None;
                if recover {
                    self.worker_failures = self.worker_failures.saturating_add(1);
                    self.recovery_retry_at =
                        Some(Instant::now() + worker_retry_delay(self.worker_failures));
                    self.paused = false;
                    let mut status = self.service.status.write().await;
                    status.paused = false;
                    status.last_error = Some("worker_failed");
                    status.worker_recovery = Some(WorkerRecoveryStatus {
                        state: "backoff",
                        attempts: self.worker_failures,
                        next_retry_at_utc: None,
                        cleanup_confirmed,
                        last_fault: diagnostics,
                    });
                } else if !self.retiring && (cleanup_confirmed || replaceable) {
                    self.pause(
                        self.persisted_pause
                            .map_or("browser_closed", |reason| reason.code()),
                    )
                    .await;
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
        let counts = SampleCounts {
            reported_chatters: sample.reported_chatters,
            viewer_rows: sample.category_counts[5],
        };
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
        // A Ready pulse is insufficient. Reset only after the verified sample
        // and its complete membership/provenance transaction committed.
        self.worker_failures = 0;
        self.recovery_retry_at = None;
        self.next_start = self.next_start.max(Instant::now() + START_GAP);
        let mut status = self.service.status.write().await;
        if let Some(recovery) = &mut status.worker_recovery {
            if matches!(recovery.state, "restarting" | "verifying" | "backoff") {
                recovery.state = "recovered";
                recovery.next_retry_at_utc = None;
            }
        }
        status.last_received_at_utc = received_at_utc.clone();
        status.last_received_channel = Some(pending.channel.clone());
        status.last_returned_accounts = Some(count);
        status.last_error = self.persisted_pause.map(|reason| reason.code());
        if let Some(schedule) = self.schedule.get_mut(&pending.channel) {
            let now = Instant::now();
            status.achieved_interval_seconds = schedule
                .last_received
                .map(|last| now.duration_since(last).as_secs_f64());
            schedule.last_received = Some(now);
            schedule.last_received_at_utc = received_at_utc;
            schedule.achieved_interval_seconds = status.achieved_interval_seconds;
            schedule.counts = Some(counts);
            schedule.last_attempt = Some(pending.started);
            schedule.cadence_due = pending.started
                + Duration::from_secs(cadence(self.settings, schedule.counts).seconds.into());
            schedule.retry_not_before = now;
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
            schedule.retry_not_before = Instant::now() + Duration::from_secs(delay);
        }
        Ok(())
    }

    async fn retire(&mut self, reason: CommunityRunEndReason) -> Result<(), StoreError> {
        self.cancel_recovery().await;
        self.quarantine_ticket = None;
        self.worker_fault_pending = false;
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
        let cleanup_timed_out = self.host.take().is_some();
        self.finish_retirement(reason, cleanup_timed_out).await
    }

    async fn finish_retirement(
        &mut self,
        reason: CommunityRunEndReason,
        cleanup_timed_out: bool,
    ) -> Result<(), StoreError> {
        if cleanup_timed_out {
            self.service.status.write().await.cleanup_confirmed = Some(false);
            self.pause_source("cleanup_unconfirmed").await?;
        }
        self.ready = false;
        self.finish_failure(CommunityFailureCode::HostFailure, true)
            .await?;
        self.end_run(reason).await?;
        self.retiring = false;
        Ok(())
    }

    async fn end_run(&mut self, reason: CommunityRunEndReason) -> Result<(), StoreError> {
        if let Some(run_id) = self.run_id {
            self.service.store.end_community_run(run_id, reason).await?;
            self.run_id = None;
        }
        Ok(())
    }

    fn stopping(&self) -> bool {
        self.shutdown
            .as_ref()
            .is_some_and(|shutdown| *shutdown.borrow())
    }

    async fn cleanup_blocks_host(&mut self) -> bool {
        self.service.status.read().await.cleanup_confirmed == Some(false) && !self.replacement_safe
    }

    async fn cancel_recovery(&mut self) {
        self.recovery_retry_at = None;
        self.quarantine_ticket = None;
        if let Some(recovery) = &mut self.service.status.write().await.worker_recovery {
            if matches!(
                recovery.state,
                "backoff" | "restarting" | "verifying" | "awaiting_cleanup"
            ) {
                recovery.state = "cancelled";
                recovery.next_retry_at_utc = None;
            }
        }
    }

    async fn pause_event(&mut self, code: &'static str) -> Result<(), StoreError> {
        if community_source::host::is_worker_failure(code) {
            // Await this generation's terminal physical proof. A worker failure
            // by itself never grants replacement or creates a source denial.
            let allowed = (!self.paused || self.worker_fault_pending)
                && (self.persisted_pause.is_none()
                    || self.quarantine_ticket == Some(self.host_generation))
                && !self.retiring
                && !self.stopping();
            self.recovery_retry_at = None;
            self.paused = true;
            {
                let mut status = self.service.status.write().await;
                status.last_error = Some(self.persisted_pause.map_or(code, |reason| reason.code()));
                status.paused = true;
                status.state = "paused";
            }
            self.worker_fault_pending = allowed;
            if self.pending.is_none() && !self.retiring {
                self.end_run(CommunityRunEndReason::HostFailure).await?;
            }
            Ok(())
        } else {
            self.pause_source(code).await
        }
    }

    async fn refresh_status(&mut self) {
        let now = Instant::now();
        let next = self
            .schedule
            .iter()
            .min_by_key(|(channel, schedule)| (schedule.eligible(), *channel));
        // With a job still active, the completion barrier cannot expire sooner
        // than thirty seconds from this observation. This is only an estimate.
        let global_floor = if self.pending.is_some() {
            self.next_start.max(now + START_GAP)
        } else {
            self.next_start
        }
        .max(self.recovery_retry_at.unwrap_or(self.next_start));
        let allowed = self.settings.enabled && !self.paused;
        let wall_now = Utc::now();
        let estimate = |due: Instant| {
            let delay = due.max(global_floor).saturating_duration_since(now);
            (wall_now + chrono::Duration::from_std(delay).unwrap_or_default())
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        };
        let mut status = self.service.status.write().await;
        if let Some(recovery) = &mut status.worker_recovery {
            recovery.next_retry_at_utc = self.recovery_retry_at.map(|due| {
                (wall_now
                    + chrono::Duration::from_std(due.saturating_duration_since(now))
                        .unwrap_or_default())
                .to_rfc3339_opts(SecondsFormat::Millis, true)
            });
        }
        status.settings = self.settings;
        status.paused = self.paused;
        status.active_channel = self.pending.as_ref().map(|pending| pending.channel.clone());
        status.selected_channels = self.schedule.len();
        status.delayed_channels = self
            .schedule
            .values()
            .filter(|schedule| {
                schedule.eligible() < now && now.duration_since(schedule.eligible()) >= START_GAP
            })
            .count();
        status.next_channel = next.map(|(channel, _)| channel.clone());
        status.next_due_at_utc = if allowed {
            next.map(|(_, schedule)| estimate(schedule.eligible()))
        } else {
            None
        };
        status.channels = self
            .schedule
            .iter()
            .map(|(channel, schedule)| {
                let decision = cadence(self.settings, schedule.counts);
                ChannelCadenceStatus {
                    channel: channel.clone(),
                    effective_interval_seconds: decision.seconds,
                    cadence_basis: decision.basis,
                    reported_chatters: schedule.counts.and_then(|counts| counts.reported_chatters),
                    viewer_category_rows: schedule.counts.map(|counts| counts.viewer_rows),
                    achieved_interval_seconds: schedule.achieved_interval_seconds,
                    failures: schedule.failures,
                    next_eligible_at_utc: allowed.then(|| estimate(schedule.eligible())),
                    last_received_at_utc: schedule.last_received_at_utc.clone(),
                }
            })
            .collect();
        if status.state != "sign_in" || !self.settings.enabled && self.host.is_none() {
            status.state = if !self.settings.enabled {
                "disabled"
            } else if self.paused {
                if status
                    .worker_recovery
                    .as_ref()
                    .is_some_and(|recovery| recovery.state == "awaiting_cleanup")
                {
                    "recovering"
                } else {
                    "paused"
                }
            } else if self.recovery_retry_at.is_some() {
                "recovering"
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
        .filter(|(_, schedule)| schedule.eligible() <= now)
        .min_by_key(|(channel, schedule)| (schedule.eligible(), *channel))
        .map(|(channel, _)| channel.clone())
}

fn worker_contained(worker: &WorkerDiagnostics) -> bool {
    worker.worker_tree_exited
        && worker.containment_confirmed
        && worker.exit_code.is_some()
        && worker.process_id.is_some()
        && worker.started_at_unix_ms.is_some()
        && worker.failure.is_some()
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

#[cfg(test)]
#[path = "community_cadence_tests.rs"]
mod cadence_tests;

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
