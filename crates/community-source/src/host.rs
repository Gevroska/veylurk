//! Bounded typed interface. No COM object, URL, script, or credential crosses it.
#![cfg_attr(not(windows), allow(dead_code))]
use crate::worker::StructuralCode;
use crate::{channel_login, is_channel_page, sampling::Provenance, ParsedSample, CATEGORIES};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const ROLE_MASK: u8 = 63;
pub const RECYCLE_NAVIGATIONS: usize = 48;

#[derive(Clone, Debug)]
pub struct HostOptions {
    /// Explicit application-owned WebView2 user-data directory. Never imported.
    pub profile_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    Unsupported,
    InvalidProfileDirectory,
    ThreadStartFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    Busy,
    Closed,
    InvalidChannel,
}

#[derive(Debug)]
pub enum HostCommand {
    Sample { job_id: u64, channel: String },
    ShowLogin,
    Cancel,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub account: String,
    pub roles: u8,
}

/// This payload deliberately has no Serialize/Debug implementation: the proof
/// must never accidentally write account names into aggregate artifacts.
pub struct SourceSample {
    pub received_at_unix_ms: i64,
    pub reported_chatters: Option<u64>,
    pub members: Vec<Member>,
    pub category_counts: [u64; 6],
    pub http_status: u16,
    pub provenance: Provenance,
}

impl SourceSample {
    pub(crate) fn from_parsed(
        parsed: ParsedSample,
        body: &crate::sampling::BodyRequest,
        received_at_unix_ms: i64,
    ) -> Self {
        Self {
            received_at_unix_ms,
            reported_chatters: Some(parsed.count),
            members: parsed
                .roles
                .into_iter()
                .map(|(account, roles)| Member {
                    account,
                    roles: roles & ROLE_MASK,
                })
                .collect(),
            category_counts: CATEGORIES
                .map(|category| parsed.categories.get(category).copied().unwrap_or(0) as u64),
            http_status: body.status,
            provenance: body.provenance.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFailure {
    pub code: &'static str,
    pub pauses_community: bool,
}

/// Allowlisted structural state only. Never a page string, URL, account or token.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PrimaryFailure {
    #[serde(deserialize_with = "crate::worker::fixed_string")]
    pub code: StructuralCode,
    #[serde(deserialize_with = "crate::worker::fixed_string")]
    pub stage: StructuralCode,
    pub observed_at_unix_ms: i64,
    pub controller_epoch: u64,
    pub job_id: Option<u64>,
    pub navigation_ready: bool,
    pub last_player_observation: Option<crate::sampling::PlayerObservation>,
    pub script_pending: bool,
    pub network_pending: bool,
    pub response_pending: bool,
    pub trigger_pending: bool,
    pub navigation_failure: Option<NavigationFailure>,
}

/// Fixed origin and native numbers only; never a URL or runtime error string.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum NavigationFailure {
    Navigate { hresult: i32 },
    NavigationCompleted { web_error_status: i32 },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeOperationFailure {
    #[serde(deserialize_with = "crate::worker::fixed_string")]
    pub operation: StructuralCode,
    pub hresult: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CleanupDiagnostics {
    /// True only when no browser/controller creation remains outstanding.
    pub no_browser_cleanup_required: bool,
    pub controller_epoch: u64,
    pub expected_browser_pid: Option<u32>,
    pub observed_exit_pid: Option<u32>,
    pub observed_exit_kind: Option<i32>,
    pub normal_exit_observed: bool,
    pub close_hresult: Option<i32>,
    pub first_detach_failure: Option<NativeOperationFailure>,
    pub unsubscribe_hresult: Option<i32>,
    pub elapsed_ms: u64,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FaultReport {
    pub primary_failure: Option<PrimaryFailure>,
    pub cleanup_diagnostics: Option<CleanupDiagnostics>,
    pub host_termination: Option<HostTermination>,
    pub window_dismissal: Option<WindowDismissal>,
    pub worker: Option<WorkerDiagnostics>,
}

/// Process containment is independent of normal WebView2 cleanup evidence.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WorkerDiagnostics {
    pub process_id: Option<u32>,
    pub started_at_unix_ms: Option<i64>,
    pub exit_code: Option<u32>,
    pub worker_tree_exited: bool,
    pub containment_confirmed: bool,
    pub failure: Option<WorkerFailure>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailure {
    Start,
    UnexpectedExit,
    Protocol,
    Stalled,
    ShutdownTimeout,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostTerminationReason {
    ShutdownRequested,
    CommandChannelClosed,
    SourceFailureWithoutController,
    CleanupUnconfirmed,
    StartupFailed,
    MessagePumpQuit,
    MessagePumpError,
    NativeHostInterrupted,
    ProofWindowClose,
    WorkerFailed,
}

/// The first observed stop trigger, before cancellation, cleanup or epoch changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HostTermination {
    pub reason: HostTerminationReason,
    pub observed_at_unix_ms: i64,
    pub controller_epoch: u64,
    pub job_id: Option<u64>,
}

/// Presentation-only dismissal of the service window; never a stop trigger.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct WindowDismissal {
    pub count: u64,
    pub last_observed_at_unix_ms: i64,
}

pub(crate) fn unix_milliseconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// An exit belongs to a browser identity, not to the time retirement was requested.
#[derive(Default)]
pub(crate) struct BrowserLifecycle {
    pub diagnostics: CleanupDiagnostics,
    bound: bool,
    retiring: bool,
}

impl BrowserLifecycle {
    pub fn bind(&mut self, epoch: u64, pid: u32) {
        *self = Self {
            diagnostics: CleanupDiagnostics {
                controller_epoch: epoch,
                expected_browser_pid: (pid != 0).then_some(pid),
                ..Default::default()
            },
            bound: true,
            retiring: false,
        };
    }

    pub fn retire(&mut self) -> bool {
        if self.retiring {
            return false;
        }
        self.retiring = true;
        true
    }

    pub fn observe_exit(&mut self, epoch: u64, pid: u32, kind: i32, normal: bool) {
        if !self.bound || epoch != self.diagnostics.controller_epoch {
            return;
        }
        if self.diagnostics.observed_exit_pid == self.diagnostics.expected_browser_pid
            && self.diagnostics.observed_exit_kind.is_some()
        {
            return;
        }
        self.diagnostics.observed_exit_pid = Some(pid);
        self.diagnostics.observed_exit_kind = Some(kind);
        self.diagnostics.normal_exit_observed |=
            normal && self.diagnostics.expected_browser_pid == Some(pid) && pid != 0;
    }

    pub fn detach_result(&mut self, operation: &'static str, hresult: i32) {
        if hresult < 0 && self.diagnostics.first_detach_failure.is_none() {
            self.diagnostics.first_detach_failure =
                Some(NativeOperationFailure { operation, hresult });
        }
    }

    pub fn finish(
        &mut self,
        elapsed_ms: u64,
        unsubscribe_hresult: Option<i32>,
    ) -> CleanupDiagnostics {
        self.diagnostics.elapsed_ms = elapsed_ms;
        self.diagnostics.unsubscribe_hresult = unsubscribe_hresult;
        self.diagnostics.confirmed = self.diagnostics.normal_exit_observed
            && self
                .diagnostics
                .close_hresult
                .is_some_and(|result| result >= 0)
            && self.diagnostics.first_detach_failure.is_none()
            && unsubscribe_hresult.is_some_and(|result| result >= 0);
        self.diagnostics.clone()
    }
}

// Storage is fixed: four queued statuses plus one login failure, completion and terminal
// slot. Keeping the payload inline avoids an extra allocation per receipt.
#[allow(clippy::large_enum_variant)]
pub enum HostEvent {
    /// Runtime/profile preflight passed and commands may be admitted. This is
    /// not evidence that a browser, signed-in session or source page is ready.
    Ready,
    LoginShown,
    LoginFailed,
    SampleCompleted {
        job_id: u64,
        channel: String,
        outcome: Result<SourceSample, SourceFailure>,
    },
    Paused {
        code: &'static str,
    },
    Stopped {
        cleanup_confirmed: bool,
        diagnostics: FaultReport,
    },
}

pub(crate) struct Control {
    pub shutdown: AtomicBool,
    pub cancel_version: AtomicU64,
    pub native_progress: AtomicU64,
    controller_epoch: AtomicU64,
    admission: Mutex<Admission>,
}

struct Admission {
    accepting: bool,
    terminated: bool,
    next_ticket: u64,
    committed_login: Option<(u64, u64)>,
    accepted: Option<AcceptedJob>,
    completion: Option<HostEvent>,
    login_failed: bool,
    terminal: Option<HostEvent>,
    diagnostics: FaultReport,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            cancel_version: AtomicU64::new(0),
            native_progress: AtomicU64::new(0),
            controller_epoch: AtomicU64::new(0),
            admission: Mutex::new(Admission {
                accepting: true,
                terminated: false,
                next_ticket: 0,
                committed_login: None,
                accepted: None,
                completion: None,
                login_failed: false,
                terminal: None,
                diagnostics: FaultReport::default(),
            }),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AcceptedJob {
    pub ticket: u64,
    pub job_id: u64,
    pub channel: String,
    pub cancel_version: u64,
    pub queued_at: Instant,
}

impl AcceptedJob {
    pub fn queue_deadline_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.queued_at)
            >= Duration::from_millis(crate::sampling::NAVIGATION_UI_MS)
    }
}

impl Admission {
    fn admit_sample(
        &mut self,
        control: &Control,
        commands: &SyncSender<QueuedCommand>,
        job_id: u64,
        channel: String,
    ) -> Result<(), CommandError> {
        if !self.accepting || control.shutdown.load(Ordering::Acquire) {
            return Err(CommandError::Closed);
        }
        if self.accepted.is_some() {
            return Err(CommandError::Busy);
        }
        self.next_ticket = self
            .next_ticket
            .checked_add(1)
            .ok_or(CommandError::Closed)?;
        let job = AcceptedJob {
            ticket: self.next_ticket,
            job_id,
            channel,
            cancel_version: control.cancel_version.load(Ordering::Acquire),
            queued_at: Instant::now(),
        };
        // Keep admission locked through publication. A receiver can dequeue
        // immediately, but cannot complete/terminate before ownership is saved.
        commands
            .try_send(QueuedCommand::Sample { job: job.clone() })
            .map_err(|error| match error {
                TrySendError::Full(_) => CommandError::Busy,
                TrySendError::Disconnected(_) => CommandError::Closed,
            })?;
        self.accepted = Some(job);
        Ok(())
    }
}

impl Control {
    /// The coordinator remains the admission authority across the pipe. Never
    /// issue another ticket, cancellation generation or queue clock here.
    pub(crate) fn adopt_sample(
        &self,
        commands: &SyncSender<QueuedCommand>,
        job: AcceptedJob,
    ) -> Result<(), ()> {
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if !admission.accepting
            || admission.accepted.is_some()
            || job.ticket <= admission.next_ticket
        {
            return Err(());
        }
        admission.next_ticket = job.ticket;
        admission.accepted = Some(job.clone());
        commands
            .try_send(QueuedCommand::Sample { job })
            .map_err(|_| ())
    }

    pub(crate) fn adopt_login(
        &self,
        commands: &SyncSender<QueuedCommand>,
        publication: u64,
        cancel_version: u64,
    ) -> Result<(), ()> {
        let mut admission = self.admission.lock().unwrap_or_else(|e| e.into_inner());
        if !admission.accepting || publication <= admission.next_ticket {
            return Err(());
        }
        admission.next_ticket = publication;
        admission.committed_login = Some((publication, cancel_version));
        // A later Cancel may have overtaken this queued publication.
        self.cancel_version
            .fetch_max(cancel_version, Ordering::AcqRel);
        commands
            .try_send(QueuedCommand::ShowLogin { publication })
            .map_err(|_| ())
    }

    pub(crate) fn fault_report(&self) -> FaultReport {
        self.admission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .diagnostics
            .clone()
    }

    pub fn record_fault(&self, mut report: FaultReport) {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Later cleanup snapshots cannot replace the original stop trigger.
        if admission.diagnostics.host_termination.is_some() {
            report.host_termination = admission.diagnostics.host_termination.clone();
        }
        if report.window_dismissal.is_none() {
            report.window_dismissal = admission.diagnostics.window_dismissal.clone();
        }
        if report.worker.is_none() {
            report.worker = admission.diagnostics.worker.clone();
        }
        admission.diagnostics = report;
    }

    pub fn set_controller_epoch(&self, epoch: u64) {
        self.controller_epoch.store(epoch, Ordering::Release);
    }

    pub fn observe_termination(&self, reason: HostTerminationReason) -> HostTermination {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(termination) = &admission.diagnostics.host_termination {
            return termination.clone();
        }
        let termination = HostTermination {
            reason,
            observed_at_unix_ms: unix_milliseconds(),
            controller_epoch: self.controller_epoch.load(Ordering::Acquire),
            job_id: admission.accepted.as_ref().map(|job| job.job_id),
        };
        admission.diagnostics.host_termination = Some(termination.clone());
        termination
    }

    pub fn login_cancel_version(&self, publication: u64) -> Option<u64> {
        // A receiver may dequeue before show_login returns, but this lock
        // fences it until that publication has committed its exact version.
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !admission.accepting || self.shutdown.load(Ordering::Acquire) {
            return None;
        }
        admission
            .committed_login
            .filter(|(ticket, version)| {
                *ticket == publication && *version == self.cancel_version.load(Ordering::Acquire)
            })
            .map(|(_, version)| version)
    }

    pub fn pending_job(&self) -> Option<AcceptedJob> {
        let admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if admission.completion.is_some() {
            None
        } else {
            admission.accepted.clone()
        }
    }

    pub fn is_pending(&self, ticket: u64) -> bool {
        self.pending_job().is_some_and(|job| job.ticket == ticket)
    }

    pub fn stop_accepting(&self) {
        self.admission
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .accepting = false;
    }

    pub fn complete(&self, ticket: u64, outcome: Result<SourceSample, SourceFailure>) -> bool {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if admission.completion.is_some()
            || !admission
                .accepted
                .as_ref()
                .is_some_and(|job| job.ticket == ticket)
        {
            return false;
        }
        let job = admission.accepted.as_ref().unwrap();
        admission.completion = Some(HostEvent::SampleCompleted {
            job_id: job.job_id,
            channel: job.channel.clone(),
            outcome,
        });
        true
    }

    /// The same lock serializes admission, accepted ownership and termination.
    /// No queue-drain snapshot is used to decide which job needs completion.
    pub fn terminate(&self, cleanup_confirmed: bool, code: &'static str) {
        let mut admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        admission.accepting = false;
        if admission.terminated {
            return;
        }
        admission.terminated = true;
        if admission.completion.is_none() {
            if let Some(job) = &admission.accepted {
                admission.completion = Some(HostEvent::SampleCompleted {
                    job_id: job.job_id,
                    channel: job.channel.clone(),
                    outcome: Err(SourceFailure {
                        code,
                        pauses_community: !cleanup_confirmed,
                    }),
                });
            }
        }
        admission.terminal = Some(HostEvent::Stopped {
            cleanup_confirmed,
            diagnostics: admission.diagnostics.clone(),
        });
    }
}

/// Covers early startup failure, unexpected pump exit, and thread unwinding.
/// Normal termination is idempotent and preserves its confirmed cleanup result.
pub(crate) struct ServiceExitGuard(pub Arc<Control>);
impl Drop for ServiceExitGuard {
    fn drop(&mut self) {
        self.0
            .observe_termination(HostTerminationReason::NativeHostInterrupted);
        self.0.terminate(false, "native_host_interrupted");
    }
}

pub(crate) enum QueuedCommand {
    Sample { job: AcceptedJob },
    ShowLogin { publication: u64 },
}

pub(crate) enum Status {
    Ready,
    LoginShown,
    LoginFailed,
    Paused { code: &'static str },
}

pub(crate) struct Bridge {
    pub commands: Receiver<QueuedCommand>,
    pub control: Arc<Control>,
    pub events: SyncSender<HostEvent>,
}

impl Bridge {
    /// Status messages can coalesce when the bounded status queue is full.
    /// The single outstanding completion and terminal cleanup have independent
    /// reserved slots, so neither account payloads nor shutdown block the STA.
    pub fn emit(&self, status: Status) {
        let event = match status {
            Status::Ready => HostEvent::Ready,
            Status::LoginShown => HostEvent::LoginShown,
            Status::LoginFailed => {
                self.control
                    .admission
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .login_failed = true;
                return;
            }
            Status::Paused { code } => HostEvent::Paused { code },
        };
        let _ = self.events.try_send(event);
    }
}

/// Send, but intentionally neither Clone nor Sync. One coordinator owns it.
pub struct HostHandle {
    commands: SyncSender<QueuedCommand>,
    events: Receiver<HostEvent>,
    control: Arc<Control>,
}

impl HostHandle {
    pub(crate) fn command_sender(&self) -> SyncSender<QueuedCommand> {
        self.commands.clone()
    }
    pub fn fault_report(&self) -> FaultReport {
        self.control
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .diagnostics
            .clone()
    }
    pub fn spawn(options: HostOptions) -> Result<Self, HostError> {
        if !options.profile_dir.is_absolute() {
            return Err(HostError::InvalidProfileDirectory);
        }
        #[cfg(not(windows))]
        {
            let _ = options;
            Err(HostError::Unsupported)
        }
        #[cfg(windows)]
        {
            let (handle, bridge) = Self::channel();
            crate::worker::spawn(options, bridge)?;
            Ok(handle)
        }
    }

    pub(crate) fn channel() -> (Self, Bridge) {
        let (commands, receiver) = mpsc::sync_channel(2);
        let (events, event_receiver) = mpsc::sync_channel(4);
        let control = Arc::new(Control::default());
        (
            Self {
                commands,
                events: event_receiver,
                control: control.clone(),
            },
            Bridge {
                commands: receiver,
                control,
                events,
            },
        )
    }

    pub fn try_command(&self, command: HostCommand) -> Result<(), CommandError> {
        match command {
            HostCommand::Sample { job_id, channel } => self.try_sample(job_id, &channel),
            HostCommand::ShowLogin => self.show_login(),
            HostCommand::Cancel => {
                self.cancel();
                Ok(())
            }
            HostCommand::Shutdown => {
                self.shutdown();
                Ok(())
            }
        }
    }

    pub fn try_sample(&self, job_id: u64, channel: &str) -> Result<(), CommandError> {
        let channel = channel_login(channel)
            .filter(|channel| is_channel_page(&format!("https://www.twitch.tv/{channel}"), channel))
            .ok_or(CommandError::InvalidChannel)?;
        let mut admission = self
            .control
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        admission.admit_sample(&self.control, &self.commands, job_id, channel)
    }

    pub fn show_login(&self) -> Result<(), CommandError> {
        let mut admission = self
            .control
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !admission.accepting || self.control.shutdown.load(Ordering::Acquire) {
            return Err(CommandError::Closed);
        }
        let publication = admission
            .next_ticket
            .checked_add(1)
            .ok_or(CommandError::Closed)?;
        // Commit cancellation only after a successful enqueue. Native adoption
        // resolves the publication under this same lock, so it cannot run
        // before commit and a rejected enqueue cannot stale an accepted login.
        self.commands
            .try_send(QueuedCommand::ShowLogin { publication })
            .map_err(|error| match error {
                TrySendError::Full(_) => CommandError::Busy,
                TrySendError::Disconnected(_) => CommandError::Closed,
            })?;
        let cancel_version = self.control.cancel_version.fetch_add(1, Ordering::AcqRel) + 1;
        admission.next_ticket = publication;
        admission.committed_login = Some((publication, cancel_version));
        Ok(())
    }

    /// Independent of command/event queue capacity. The 250ms native timer
    /// observes these flags even if no command can be enqueued.
    pub fn cancel(&self) {
        self.control.cancel_version.fetch_add(1, Ordering::AcqRel);
    }
    pub fn shutdown(&self) {
        self.control
            .observe_termination(HostTerminationReason::ShutdownRequested);
        self.control.shutdown.store(true, Ordering::Release);
    }

    pub fn try_event(&self) -> Result<HostEvent, TryRecvError> {
        self.try_ticketed_event().map(|(_, event)| event)
    }

    /// The wire ticket is captured under the same lock that consumes the
    /// completion. A command reader may admit a job between any other locks.
    pub(crate) fn try_ticketed_event(&self) -> Result<(Option<u64>, HostEvent), TryRecvError> {
        let mut admission = self
            .control
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(event) = admission.completion.take() {
            let ticket = admission.accepted.as_ref().map(|job| job.ticket);
            admission.accepted = None;
            return Ok((ticket, event));
        }
        match self.events.try_recv() {
            Ok(event) => Ok((None, event)),
            Err(_) if admission.login_failed => {
                admission.login_failed = false;
                Ok((None, HostEvent::LoginFailed))
            }
            Err(error) => admission
                .terminal
                .take()
                .map(|event| (None, event))
                .ok_or(error),
        }
    }
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn worker_completion_consumes_its_ticket_atomically_after_late_adoption() {
        let (handle, bridge) = HostHandle::channel();
        assert!(handle.try_ticketed_event().is_err());
        let queued_at = Instant::now() - Duration::from_secs(2);
        bridge
            .control
            .adopt_sample(
                &handle.command_sender(),
                AcceptedJob {
                    ticket: 29,
                    job_id: 4,
                    channel: "room".into(),
                    cancel_version: 7,
                    queued_at,
                },
            )
            .unwrap();
        let job = take_job(&bridge);
        assert_eq!(job.ticket, 29);
        assert_eq!(job.cancel_version, 7);
        assert_eq!(job.queued_at, queued_at);
        assert!(bridge.control.complete(29, failure()));
        let Ok((Some(ticket), HostEvent::SampleCompleted { job_id: 4, .. })) =
            handle.try_ticketed_event()
        else {
            panic!("missing ticketed completion");
        };
        assert_eq!(ticket, 29);
        assert!(!bridge.control.complete(29, failure()));
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn remote_login_never_rolls_back_a_later_cancellation() {
        let (handle, bridge) = HostHandle::channel();
        bridge.control.cancel_version.store(9, Ordering::Release);
        bridge
            .control
            .adopt_login(&handle.command_sender(), 3, 8)
            .unwrap();
        assert_eq!(bridge.control.cancel_version.load(Ordering::Acquire), 9);
        assert_eq!(bridge.control.login_cancel_version(3), None);
    }

    #[test]
    fn login_failure_has_one_reserved_slot_after_older_login_status() {
        let (handle, bridge) = HostHandle::channel();
        bridge.emit(Status::LoginShown);
        for _ in 0..8 {
            bridge.emit(Status::Ready);
            bridge.emit(Status::LoginFailed);
        }
        assert!(matches!(handle.try_event(), Ok(HostEvent::LoginShown)));
        for _ in 0..3 {
            assert!(matches!(handle.try_event(), Ok(HostEvent::Ready)));
        }
        assert!(matches!(handle.try_event(), Ok(HostEvent::LoginFailed)));
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn first_termination_snapshot_survives_cleanup_and_exit_guard_with_full_status_queue() {
        for reason in [
            HostTerminationReason::ShutdownRequested,
            HostTerminationReason::CommandChannelClosed,
            HostTerminationReason::SourceFailureWithoutController,
            HostTerminationReason::CleanupUnconfirmed,
            HostTerminationReason::StartupFailed,
            HostTerminationReason::MessagePumpQuit,
            HostTerminationReason::MessagePumpError,
            HostTerminationReason::NativeHostInterrupted,
        ] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(91, "private_room").unwrap();
            bridge.control.set_controller_epoch(7);
            for _ in 0..8 {
                bridge.emit(Status::Ready);
            }
            let first = bridge.control.observe_termination(reason);
            assert!(first.observed_at_unix_ms > 0);
            assert_eq!((first.controller_epoch, first.job_id), (7, Some(91)));
            bridge.control.set_controller_epoch(8);
            bridge.control.record_fault(FaultReport {
                cleanup_diagnostics: Some(CleanupDiagnostics {
                    controller_epoch: 7,
                    ..Default::default()
                }),
                ..Default::default()
            });
            bridge
                .control
                .observe_termination(HostTerminationReason::CleanupUnconfirmed);
            bridge.control.terminate(false, "native_host_interrupted");
            drop(ServiceExitGuard(bridge.control.clone()));
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::SampleCompleted { job_id: 91, .. })
            ));
            for _ in 0..4 {
                assert!(matches!(handle.try_event(), Ok(HostEvent::Ready)));
            }
            let Ok(HostEvent::Stopped { diagnostics, .. }) = handle.try_event() else {
                panic!("missing reserved terminal report");
            };
            assert_eq!(diagnostics.host_termination.unwrap(), first);
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn early_identity_bound_exit_survives_retirement_and_close_transition() {
        let mut browser = BrowserLifecycle::default();
        browser.bind(7, 100);
        browser.observe_exit(7, 100, 0, true);
        assert!(browser.retire());
        assert!(
            !browser.retire(),
            "recycle-to-close must keep its retirement identity"
        );
        browser.diagnostics.close_hresult = Some(0);
        assert!(browser.finish(12, Some(0)).confirmed);
        browser.bind(8, 100);
        browser.observe_exit(7, 100, 0, true);
        browser.diagnostics.close_hresult = Some(0);
        assert!(!browser.finish(5000, Some(0)).confirmed);
    }

    #[test]
    fn stale_mismatched_and_abnormal_exits_never_confirm_cleanup() {
        for (epoch, pid, kind, normal) in [(6, 100, 0, true), (7, 200, 0, true), (7, 100, 1, false)]
        {
            let mut browser = BrowserLifecycle::default();
            browser.bind(7, 100);
            browser.observe_exit(epoch, pid, kind, normal);
            browser.retire();
            browser.diagnostics.close_hresult = Some(0);
            assert!(!browser.finish(5000, Some(0)).confirmed);
            if kind == 1 {
                browser.observe_exit(7, 100, 0, true);
                assert_eq!(browser.diagnostics.observed_exit_kind, Some(1));
                assert!(!browser.finish(5001, Some(0)).confirmed);
            }
        }
        let mut browser = BrowserLifecycle::default();
        browser.bind(7, 0);
        browser.observe_exit(7, 0, 0, true);
        browser.diagnostics.close_hresult = Some(0);
        assert!(!browser.finish(5000, Some(0)).confirmed);
    }

    #[test]
    fn every_cleanup_operation_is_required_and_first_detach_failure_is_retained() {
        for failure in 0..4 {
            let mut browser = BrowserLifecycle::default();
            browser.bind(7, 100);
            browser.retire();
            browser.observe_exit(7, 100, 0, true);
            browser.diagnostics.close_hresult = Some(if failure == 0 { -1 } else { 0 });
            if failure == 1 {
                browser.detach_result("remove_protocol_event", -2);
                browser.detach_result("remove_navigation_starting", -3);
                assert_eq!(
                    browser
                        .diagnostics
                        .first_detach_failure
                        .as_ref()
                        .unwrap()
                        .operation,
                    "remove_protocol_event"
                );
            }
            let unsubscribe = match failure {
                2 => Some(-4),
                3 => None,
                _ => Some(0),
            };
            let result = browser.finish(5000, unsubscribe);
            assert!(!result.confirmed);
            assert!(result.normal_exit_observed);
        }
    }

    #[test]
    fn reserved_terminal_preserves_fault_report_when_status_queue_is_full() {
        let (handle, bridge) = HostHandle::channel();
        for _ in 0..12 {
            bridge.emit(Status::Ready);
        }
        bridge.control.record_fault(FaultReport {
            primary_failure: Some(PrimaryFailure {
                code: "navigation_ui_deadline",
                stage: "preparing",
                observed_at_unix_ms: 1,
                controller_epoch: 7,
                job_id: Some(1),
                navigation_ready: true,
                last_player_observation: None,
                script_pending: true,
                network_pending: false,
                response_pending: false,
                trigger_pending: false,
                navigation_failure: None,
            }),
            cleanup_diagnostics: Some(CleanupDiagnostics {
                controller_epoch: 7,
                close_hresult: Some(-1),
                ..Default::default()
            }),
            ..Default::default()
        });
        bridge.control.terminate(false, "operator_cancelled");
        for _ in 0..4 {
            assert!(matches!(handle.try_event(), Ok(HostEvent::Ready)));
        }
        let Ok(HostEvent::Stopped {
            cleanup_confirmed,
            diagnostics,
        }) = handle.try_event()
        else {
            panic!("missing reserved terminal");
        };
        assert!(!cleanup_confirmed);
        assert_eq!(
            diagnostics.primary_failure.unwrap().code,
            "navigation_ui_deadline"
        );
        assert_eq!(
            diagnostics.cleanup_diagnostics.unwrap().close_hresult,
            Some(-1)
        );
    }

    fn failure() -> Result<SourceSample, SourceFailure> {
        Err(SourceFailure {
            code: "operator_cancelled",
            pauses_community: false,
        })
    }

    fn take_job(bridge: &Bridge) -> AcceptedJob {
        match bridge
            .commands
            .try_recv()
            .unwrap_or_else(|_| panic!("missing command"))
        {
            QueuedCommand::Sample { job } => job,
            QueuedCommand::ShowLogin { .. } => panic!("expected sample"),
        }
    }

    #[test]
    fn cancellation_and_shutdown_bypass_full_queues_and_results_have_a_reserved_slot() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(4, "room").unwrap();
        let job = bridge.control.pending_job().unwrap();
        handle.show_login().unwrap();
        assert_eq!(handle.show_login(), Err(CommandError::Busy));
        for _ in 0..8 {
            bridge.emit(Status::Ready);
        }
        let before = bridge.control.cancel_version.load(Ordering::Acquire);
        handle.cancel();
        handle.shutdown();
        assert!(bridge.control.shutdown.load(Ordering::Acquire));
        assert_eq!(
            bridge.control.cancel_version.load(Ordering::Acquire),
            before + 1
        );
        assert!(bridge.control.complete(job.ticket, failure()));
        bridge.control.terminate(true, "operator_cancelled");
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted { job_id: 4, .. })
        ));
        for _ in 0..4 {
            assert!(matches!(handle.try_event(), Ok(HostEvent::Ready)));
        }
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::Stopped {
                cleanup_confirmed: true,
                ..
            })
        ));
        bridge.control.terminate(false, "native_host_interrupted");
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn one_job_is_owned_until_completion_is_consumed_and_old_tickets_cannot_complete_a_reused_id() {
        let (handle, bridge) = HostHandle::channel();
        for channel in ["https://twitch.tv/room", "login", "../room"] {
            assert_eq!(
                handle.try_sample(1, channel),
                Err(CommandError::InvalidChannel)
            );
        }
        handle.try_sample(7, "Room").unwrap();
        assert_eq!(handle.try_sample(8, "other"), Err(CommandError::Busy));
        let first = take_job(&bridge);
        assert_eq!(first.channel, "room");
        assert!(bridge.control.complete(first.ticket, failure()));
        assert!(!bridge.control.complete(first.ticket, failure()));
        assert_eq!(handle.try_sample(8, "other"), Err(CommandError::Busy));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted { job_id: 7, .. })
        ));
        handle.try_sample(7, "room").unwrap();
        let second = take_job(&bridge);
        assert_ne!(second.ticket, first.ticket);
        assert!(!bridge.control.complete(first.ticket, failure()));
        assert!(bridge.control.complete(second.ticket, failure()));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted { job_id: 7, .. })
        ));
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn admission_racing_terminal_shutdown_has_exactly_one_completion_if_accepted() {
        for job_id in 0..64 {
            let (handle, bridge) = HostHandle::channel();
            let barrier = Arc::new(Barrier::new(2));
            let control = bridge.control.clone();
            let native_barrier = barrier.clone();
            let native = std::thread::spawn(move || {
                native_barrier.wait();
                control.terminate(true, "operator_cancelled");
            });
            barrier.wait();
            let admitted = handle.try_sample(job_id, "room");
            native.join().unwrap();
            match admitted {
                Ok(()) => assert!(
                    matches!(handle.try_event(), Ok(HostEvent::SampleCompleted { job_id: completed, .. }) if completed == job_id)
                ),
                Err(error) => assert_eq!(error, CommandError::Closed),
            }
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::Stopped {
                    cleanup_confirmed: true,
                    ..
                })
            ));
            assert!(handle.try_event().is_err());
            assert_eq!(
                handle.try_sample(job_id + 1, "room"),
                Err(CommandError::Closed)
            );
        }
    }

    #[test]
    fn admission_after_last_queue_drain_is_owned_and_early_startup_failure_survives_bridge_drop() {
        let (handle, bridge) = HostHandle::channel();
        let guard = ServiceExitGuard(bridge.control.clone());
        assert!(bridge.commands.try_iter().next().is_none());
        // This is the former finish_service drain race: a successful enqueue
        // occurs after its final empty receive, before native termination.
        handle.try_sample(19, "room").unwrap();
        drop(bridge);
        drop(guard);
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 19,
                outcome: Err(SourceFailure {
                    code: "native_host_interrupted",
                    ..
                }),
                ..
            })
        ));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::Stopped {
                cleanup_confirmed: false,
                ..
            })
        ));
        assert!(handle.try_event().is_err());
        assert_eq!(handle.try_sample(20, "room"), Err(CommandError::Closed));
    }

    #[test]
    fn termination_cannot_pass_an_admission_lock_holder_between_check_and_publication() {
        let (handle, bridge) = HostHandle::channel();
        let mut admission = bridge.control.admission.lock().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let control = bridge.control.clone();
        let native_barrier = barrier.clone();
        let native = std::thread::spawn(move || {
            native_barrier.wait();
            control.terminate(true, "operator_cancelled");
        });
        barrier.wait();
        // Simulate the sender paused while holding the real admission lock
        // after its accepting check; native termination must use that lock.
        assert!(admission.accepting);
        admission
            .admit_sample(&bridge.control, &handle.commands, 21, "room".into())
            .unwrap();
        drop(admission);
        native.join().unwrap();
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted { job_id: 21, .. })
        ));
        assert!(matches!(handle.try_event(), Ok(HostEvent::Stopped { .. })));
        assert_eq!(handle.try_sample(22, "room"), Err(CommandError::Closed));
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn login_publication_commits_its_version_and_later_cancellation_supersedes_it() {
        let (handle, bridge) = HostHandle::channel();
        let timer_snapshot = bridge.control.cancel_version.load(Ordering::Acquire);
        handle.show_login().unwrap();
        let QueuedCommand::ShowLogin { publication } = bridge
            .commands
            .try_recv()
            .unwrap_or_else(|_| panic!("missing login"))
        else {
            panic!("expected login");
        };
        let committed = bridge.control.login_cancel_version(publication).unwrap();
        assert!(committed > timer_snapshot);
        assert_eq!(
            bridge.control.cancel_version.load(Ordering::Acquire),
            committed
        );
        handle.cancel();
        assert_eq!(bridge.control.login_cancel_version(publication), None);
    }

    #[test]
    fn busy_login_does_not_invalidate_the_earlier_accepted_login_in_a_full_queue() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(30, "room").unwrap();
        handle.show_login().unwrap();
        let committed = bridge.control.cancel_version.load(Ordering::Acquire);
        assert_eq!(handle.show_login(), Err(CommandError::Busy));
        assert_eq!(
            bridge.control.cancel_version.load(Ordering::Acquire),
            committed
        );
        let sample = take_job(&bridge);
        let QueuedCommand::ShowLogin { publication } = bridge
            .commands
            .try_recv()
            .unwrap_or_else(|_| panic!("missing accepted login"))
        else {
            panic!("expected login");
        };
        assert_eq!(
            bridge.control.login_cancel_version(publication),
            Some(committed)
        );
        assert_ne!(sample.cancel_version, committed);
        // A later real Cancel still supersedes the accepted publication.
        handle.cancel();
        assert_eq!(bridge.control.login_cancel_version(publication), None);
    }

    #[test]
    fn queued_sample_uses_its_original_admission_deadline() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(22, "room").unwrap();
        let job = take_job(&bridge);
        assert!(!job.queue_deadline_expired(
            job.queued_at + Duration::from_millis(crate::sampling::NAVIGATION_UI_MS - 1)
        ));
        assert!(job.queue_deadline_expired(
            job.queued_at + Duration::from_millis(crate::sampling::NAVIGATION_UI_MS)
        ));
        assert!(job.queue_deadline_expired(job.queued_at + Duration::from_secs(60)));
    }
}
