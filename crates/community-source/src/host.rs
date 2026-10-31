//! Bounded typed interface. No COM object, URL, script, or credential crosses it.
#![cfg_attr(not(windows), allow(dead_code))]
use crate::{channel_login, is_channel_page, sampling::Provenance, ParsedSample, CATEGORIES};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    time::{Duration, Instant},
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

pub enum HostEvent {
    Ready,
    LoginShown,
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
    },
}

pub(crate) struct Control {
    pub shutdown: AtomicBool,
    pub cancel_version: AtomicU64,
    admission: Mutex<Admission>,
}

struct Admission {
    accepting: bool,
    terminated: bool,
    next_ticket: u64,
    committed_login: Option<(u64, u64)>,
    accepted: Option<AcceptedJob>,
    completion: Option<HostEvent>,
    terminal: Option<HostEvent>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            shutdown: AtomicBool::new(false),
            cancel_version: AtomicU64::new(0),
            admission: Mutex::new(Admission {
                accepting: true,
                terminated: false,
                next_ticket: 0,
                committed_login: None,
                accepted: None,
                completion: None,
                terminal: None,
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
        admission.terminal = Some(HostEvent::Stopped { cleanup_confirmed });
    }
}

/// Covers early startup failure, unexpected pump exit, and thread unwinding.
/// Normal termination is idempotent and preserves its confirmed cleanup result.
pub(crate) struct ServiceExitGuard(pub Arc<Control>);
impl Drop for ServiceExitGuard {
    fn drop(&mut self) {
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
            std::thread::Builder::new()
                .name("community-webview2-sta".into())
                .spawn(move || {
                    crate::native::run_service(options, bridge);
                })
                .map_err(|_| HostError::ThreadStartFailed)?;
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
        self.control.shutdown.store(true, Ordering::Release);
    }

    pub fn try_event(&self) -> Result<HostEvent, TryRecvError> {
        let mut admission = self
            .control
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(event) = admission.completion.take() {
            admission.accepted = None;
            return Ok(event);
        }
        match self.events.try_recv() {
            Ok(event) => Ok(event),
            Err(error) => admission.terminal.take().ok_or(error),
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
                cleanup_confirmed: true
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
                    cleanup_confirmed: true
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
                cleanup_confirmed: false
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
