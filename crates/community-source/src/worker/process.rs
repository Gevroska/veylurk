//! Windows process ownership. The worker cannot reach COM before its Start
//! frame, and that frame is sent only after assignment to this exact Job handle.
use super::*;
use crate::host::{
    AcceptedJob, Bridge, HostError, HostEvent, HostHandle, HostOptions, HostTerminationReason,
    QueuedCommand, ServiceExitGuard, Status, WorkerDiagnostics, WorkerFailure,
};
use std::os::windows::io::AsRawHandle;
use std::process::{Command, ExitCode};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

mod launch;
use launch::ChildProcess;

const POLL: Duration = Duration::from_millis(25);
const STALL_LIMIT: Duration = Duration::from_secs(15);
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(6);
const PIPE_WRITE_LIMIT: Duration = Duration::from_secs(3);
const TREE_EXIT_LIMIT: Duration = Duration::from_secs(5);
const EXIT_GRACE_LIMIT: Duration = Duration::from_millis(500);
const PROCESS_EXIT_LIMIT: Duration = Duration::from_secs(1);
const QUARANTINE_POLL: Duration = Duration::from_millis(100);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const KILL_ON_JOB_CLOSE: u32 = 0x2000;

type Handle = *mut std::ffi::c_void;
#[repr(C)]
#[derive(Default)]
struct BasicLimits {
    process_user_time: i64,
    job_user_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}
#[repr(C)]
#[derive(Default)]
struct ExtendedLimits {
    basic: BasicLimits,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}
#[repr(C)]
#[derive(Default)]
struct Accounting {
    user_time: i64,
    kernel_time: i64,
    period_user_time: i64,
    period_kernel_time: i64,
    page_faults: u32,
    total_processes: u32,
    active_processes: u32,
    terminated_processes: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(
        job: Handle,
        class: i32,
        data: *const std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn QueryInformationJobObject(
        job: Handle,
        class: i32,
        data: *mut std::ffi::c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetTickCount64() -> u64;
    fn SetErrorMode(mode: u32) -> u32;
}
fn tick_ms() -> u64 {
    unsafe { GetTickCount64() }
}

struct Job(Handle);
impl Job {
    fn create() -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let limits = ExtendedLimits {
            basic: BasicLimits {
                flags: KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        if unsafe {
            SetInformationJobObject(
                job.0,
                9,
                &limits as *const _ as _,
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
    fn assign(&self, child: &ChildProcess) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn active(&self) -> io::Result<u32> {
        let mut accounting = Accounting::default();
        if unsafe {
            QueryInformationJobObject(
                self.0,
                1,
                &mut accounting as *mut _ as _,
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.active_processes)
    }
    fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.0, 0xE001_0001);
        }
    }
    fn wait_empty(&self) -> bool {
        let started = Instant::now();
        loop {
            match self.active() {
                Ok(0) => return true,
                Err(_) => return false,
                _ => {}
            }
            if started.elapsed() >= TREE_EXIT_LIMIT {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }
    fn close(&mut self) {
        if !self.0.is_null() {
            let handle = std::mem::replace(&mut self.0, std::ptr::null_mut());
            unsafe {
                CloseHandle(handle);
            }
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.close();
    }
}

struct OwnedWorker {
    child: ChildProcess,
    job: Job,
}

#[derive(Debug)]
enum LaunchFailure {
    NotCreated,
    OwnershipUnknown,
}

impl OwnedWorker {
    fn launch(command: &mut Command) -> Result<Self, LaunchFailure> {
        let job = Job::create().map_err(|_| LaunchFailure::NotCreated)?;
        // ChildProcess::spawn has no fallible return after successful
        // CreateProcessW. Every Err here proves that no child was created.
        let mut child = ChildProcess::spawn(command).map_err(|_| LaunchFailure::NotCreated)?;
        if job.assign(&child).is_err() {
            // Still gated on stdin: this child has never touched COM or a profile.
            let _ = child.kill();
            let started = Instant::now();
            while matches!(child.try_wait(), Ok(None)) && started.elapsed() < Duration::from_secs(1)
            {
                std::thread::sleep(POLL);
            }
            return Err(LaunchFailure::OwnershipUnknown);
        }
        Ok(Self { child, job })
    }
    fn finish(&mut self, allow_graceful: bool) -> (Option<u32>, bool) {
        // Pipe EOF can precede final process-handle signaling. Give an already
        // exiting worker a bounded chance to preserve its own numeric reason.
        let _ = self.child.wait_bounded(if allow_graceful {
            EXIT_GRACE_LIMIT.as_millis() as u32
        } else {
            100
        });
        self.job.terminate();
        let empty = self.job.wait_empty();
        // Job accounting can reach zero before the process object signals.
        // Read the reason from the retained handle, never a reused PID.
        let code = self
            .child
            .wait_bounded(PROCESS_EXIT_LIMIT.as_millis() as u32)
            .ok()
            .flatten()
            .and_then(|status| status.code())
            .map(|n| n as u32);
        (code, empty && code.is_some())
    }

    fn probe_containment(&mut self) -> (Option<u32>, bool) {
        let code = self
            .child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code())
            .map(|code| code as u32);
        let empty = self.job.active().is_ok_and(|active| active == 0);
        (code, empty && code.is_some())
    }
}
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        self.job.terminate();
    }
}

pub(crate) fn spawn(options: HostOptions, bridge: Bridge) -> Result<(), HostError> {
    std::thread::Builder::new()
        .name("community-worker-supervisor".into())
        .spawn(move || supervise(options, bridge))
        .map(|_| ())
        .map_err(|_| HostError::ThreadStartFailed)
}

fn worker_command() -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(WORKER_ARGUMENT).env_clear();
    // Only OS/runtime discovery and the explicitly supplied browser profile are
    // needed. Twitch application credentials never enter the child environment.
    for key in [
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    Ok(command)
}

#[allow(clippy::large_enum_variant)]
enum Inbound {
    Frame(WorkerFrame),
    Eof,
    Invalid,
}

fn supervise(options: HostOptions, bridge: Bridge) {
    if has_environment_override(|name| std::env::var_os(name).is_some()) {
        refuse_environment_override(&bridge);
        return;
    }
    let launched = worker_command()
        .map_err(|_| LaunchFailure::NotCreated)
        .and_then(|mut command| OwnedWorker::launch(&mut command));
    supervise_owned(options, bridge, launched);
}

fn has_environment_override(mut present: impl FnMut(&str) -> bool) -> bool {
    WEBVIEW_ENVIRONMENT_OVERRIDES
        .iter()
        .any(|name| present(name))
}

fn refuse_environment_override(bridge: &Bridge) {
    use crate::host::{CleanupDiagnostics, PrimaryFailure};
    let code = "webview2_environment_override";
    bridge
        .control
        .observe_termination(HostTerminationReason::StartupFailed);
    let job = bridge.control.pending_job();
    bridge.control.record_fault(FaultReport {
        primary_failure: Some(PrimaryFailure {
            code,
            stage: "initializing",
            observed_at_unix_ms: crate::host::unix_milliseconds(),
            controller_epoch: 0,
            job_id: job.as_ref().map(|job| job.job_id),
            navigation_ready: false,
            last_player_observation: None,
            script_pending: false,
            network_pending: false,
            response_pending: false,
            trigger_pending: false,
            navigation_failure: None,
        }),
        cleanup_diagnostics: Some(CleanupDiagnostics {
            no_browser_cleanup_required: true,
            confirmed: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    if let Some(job) = job {
        bridge.control.complete(
            job.ticket,
            Err(SourceFailure {
                code,
                pauses_community: false,
            }),
        );
    }
    bridge.emit(Status::Paused { code });
    bridge.control.terminate(true, code);
}

fn supervise_owned(
    options: HostOptions,
    bridge: Bridge,
    launched: Result<OwnedWorker, LaunchFailure>,
) {
    let control = bridge.control.clone();
    let _guard = ServiceExitGuard(control.clone());
    let mut worker = match launched {
        Ok(worker) => worker,
        Err(failure) => {
            fail_worker(
                &bridge,
                WorkerDiagnostics {
                    failure: Some(match failure {
                        LaunchFailure::NotCreated => WorkerFailure::NotCreated,
                        LaunchFailure::OwnershipUnknown => WorkerFailure::Start,
                    }),
                    ..Default::default()
                },
            );
            return;
        }
    };
    let mut worker_diagnostics = WorkerDiagnostics {
        process_id: Some(worker.child.id()),
        started_at_unix_ms: worker.child.started_at_unix_ms,
        ..Default::default()
    };
    let mut initial = control.fault_report();
    initial.worker = Some(worker_diagnostics.clone());
    control.record_fault(initial);
    let (outbound, outgoing) = mpsc::sync_channel::<ParentFrame>(4);
    let (incoming, inbound) = mpsc::sync_channel::<Inbound>(8);
    let write_started = Arc::new(AtomicU64::new(0));
    let writer_failed = Arc::new(AtomicBool::new(false));
    let mut stdin = worker.child.stdin.take().unwrap();
    let mut stdout = worker.child.stdout.take().unwrap();
    let writing = write_started.clone();
    let failed = writer_failed.clone();
    let writer = std::thread::Builder::new()
        .name("community-worker-pipe-write".into())
        .spawn(move || {
            while let Ok(frame) = outgoing.recv() {
                writing.store(tick_ms().max(1), Ordering::Release);
                if write_frame(&mut stdin, &frame).is_err() {
                    failed.store(true, Ordering::Release);
                    break;
                }
                writing.store(0, Ordering::Release);
            }
        });
    let reader = std::thread::Builder::new()
        .name("community-worker-pipe-read".into())
        .spawn(move || loop {
            let message = match read_frame(&mut stdout) {
                Ok(Some(frame)) => Inbound::Frame(frame),
                Ok(None) => Inbound::Eof,
                Err(_) => Inbound::Invalid,
            };
            let terminal = !matches!(message, Inbound::Frame(_));
            if incoming.send(message).is_err() || terminal {
                break;
            }
        });
    let mut failure = if writer.is_err() || reader.is_err() {
        Some(WorkerFailure::Start)
    } else {
        None
    };
    // This is the sole startup gate; Job assignment has already succeeded.
    if failure.is_none()
        && outbound
            .try_send(ParentFrame::Start {
                version: VERSION,
                profile_dir: options.profile_dir,
            })
            .is_err()
    {
        failure = Some(WorkerFailure::Start);
    }
    let mut sent_control = (u64::MAX, false);
    let mut pending_command = None;
    let mut terminal = None;
    let mut shutdown_started = None;
    let mut last_progress = (0, Instant::now());
    let mut process_exited_at = None;
    let mut native_failure = None;
    while failure.is_none() {
        // FIFO pipe delivery is consumed before examining process exit. A full
        // valid receipt remains completed even if the worker then crashes.
        match inbound.recv_timeout(POLL) {
            Ok(Inbound::Frame(frame)) => {
                match accept_frame(
                    &bridge,
                    frame,
                    &mut last_progress,
                    &mut terminal,
                    &mut worker_diagnostics,
                ) {
                    Ok(observed_failure) => {
                        if let Some(observed_failure) = observed_failure {
                            native_failure = Some(observed_failure);
                            shutdown_started.get_or_insert_with(Instant::now);
                        }
                    }
                    Err(()) => {
                        failure = Some(WorkerFailure::Protocol);
                        break;
                    }
                }
            }
            Ok(Inbound::Eof) => {
                if terminal.is_none() {
                    failure = Some(WorkerFailure::UnexpectedExit);
                }
                break;
            }
            Ok(Inbound::Invalid) => {
                failure = Some(WorkerFailure::Protocol);
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                failure = Some(WorkerFailure::UnexpectedExit);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let shutdown = control.shutdown.load(Ordering::Acquire);
        if shutdown || terminal.is_some() {
            shutdown_started.get_or_insert_with(Instant::now);
        }
        if shutdown_started.is_some_and(|at| at.elapsed() >= SHUTDOWN_LIMIT) {
            failure = Some(WorkerFailure::ShutdownTimeout);
            break;
        }
        if last_progress.1.elapsed() >= STALL_LIMIT {
            failure = Some(WorkerFailure::Stalled);
            break;
        }
        let write_at = write_started.load(Ordering::Acquire);
        if write_at != 0
            && tick_ms().saturating_sub(write_at) >= PIPE_WRITE_LIMIT.as_millis() as u64
        {
            failure = Some(WorkerFailure::Stalled);
            break;
        }
        if writer_failed.load(Ordering::Acquire) {
            // Keep reading through EOF, so already delivered receipts are not lost.
            process_exited_at.get_or_insert_with(Instant::now);
        }
        match worker.child.try_wait() {
            Ok(Some(_)) => {
                process_exited_at.get_or_insert_with(Instant::now);
            }
            Err(_) => {
                failure = Some(WorkerFailure::UnexpectedExit);
                break;
            }
            _ => {}
        }
        if process_exited_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(1)) {
            failure = Some(WorkerFailure::UnexpectedExit);
            break;
        }
        let snapshot = (control.cancel_version.load(Ordering::Acquire), shutdown);
        if snapshot != sent_control {
            if outbound
                .try_send(ParentFrame::Control {
                    cancel_version: snapshot.0,
                    shutdown: snapshot.1,
                })
                .is_ok()
            {
                sent_control = snapshot;
            } else {
                continue;
            }
        }
        if terminal.is_some() || shutdown || native_failure.is_some() {
            continue;
        }
        if pending_command.is_none() {
            pending_command = bridge.commands.try_recv().ok();
        }
        if let Some(command) = pending_command.take() {
            let frame = match &command {
                QueuedCommand::Sample { job } => {
                    let code = if job.cancel_version != snapshot.0 {
                        Some("operator_cancelled")
                    } else if job.queue_deadline_expired(Instant::now()) {
                        Some("sample_queue_deadline")
                    } else {
                        None
                    };
                    if let Some(code) = code {
                        control.complete(
                            job.ticket,
                            Err(SourceFailure {
                                code,
                                pauses_community: false,
                            }),
                        );
                        continue;
                    }
                    ParentFrame::Sample {
                        ticket: job.ticket,
                        job_id: job.job_id,
                        channel: job.channel.clone(),
                        cancel_version: job.cancel_version,
                        queued_tick_ms: tick_ms().saturating_sub(
                            job.queued_at.elapsed().as_millis().min(u64::MAX as u128) as u64,
                        ),
                    }
                }
                QueuedCommand::ShowLogin { publication } => {
                    let Some(cancel_version) = control.login_cancel_version(*publication) else {
                        continue;
                    };
                    ParentFrame::Login {
                        publication: *publication,
                        cancel_version,
                    }
                }
            };
            if outbound.try_send(frame).is_err() {
                pending_command = Some(command);
            }
        }
    }
    drop(outbound);
    // Even after a native normal-cleanup report, physical ownership must end.
    // The exact Job, not a PID lookup, proves that no profile owner survived.
    let (exit_code, mut contained) = worker.finish(failure.is_none() && terminal.is_some());
    worker_diagnostics.exit_code = exit_code;
    worker_diagnostics.worker_tree_exited = contained;
    worker_diagnostics.containment_confirmed = contained;
    if failure.is_none() && exit_code != Some(0) {
        failure = Some(WorkerFailure::UnexpectedExit);
    }
    worker_diagnostics.failure = native_failure.or(failure);
    failure = worker_diagnostics.failure;
    if !contained && !control.shutdown.load(Ordering::Acquire) {
        // Keep the exact Job and process handles while cleanup is unknown.
        // The one failed completion is delivered now; the sole terminal waits
        // for actual proof, so the parent cannot launch a second profile owner.
        contained = quarantine_worker(
            &mut worker,
            &bridge,
            &mut worker_diagnostics,
            OwnedWorker::probe_containment,
        );
        failure = worker_diagnostics.failure;
    }
    if failure.is_some() || !contained || terminal.is_none() {
        fail_worker(&bridge, worker_diagnostics);
    } else if let Some((cleanup_confirmed, mut report)) = terminal {
        report.worker = Some(worker_diagnostics);
        control.record_fault(report);
        control.terminate(cleanup_confirmed, "operator_cancelled");
    }
    // Killing the exact Job closes both pipe ends. Threads are detached rather
    // than joined without a deadline; each has only bounded channel storage.
    drop(inbound);
    drop(writer);
    drop(reader);
}

fn quarantine_worker(
    worker: &mut OwnedWorker,
    bridge: &Bridge,
    diagnostics: &mut WorkerDiagnostics,
    mut probe: impl FnMut(&mut OwnedWorker) -> (Option<u32>, bool),
) -> bool {
    diagnostics
        .failure
        .get_or_insert(WorkerFailure::ShutdownTimeout);
    publish_worker_failure(bridge, diagnostics.clone(), false);
    let mut delay = Duration::from_secs(1);
    loop {
        if bridge.control.shutdown.load(Ordering::Acquire) {
            return false;
        }
        let (code, contained) = probe(worker);
        if contained {
            diagnostics.exit_code = code;
            diagnostics.worker_tree_exited = true;
            diagnostics.containment_confirmed = true;
            return true;
        }
        worker.job.terminate();
        let next_probe = Instant::now() + delay;
        while Instant::now() < next_probe {
            if bridge.control.shutdown.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(QUARANTINE_POLL);
        }
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

fn accept_frame(
    bridge: &Bridge,
    frame: WorkerFrame,
    progress: &mut (u64, Instant),
    terminal: &mut Option<(bool, FaultReport)>,
    worker: &mut WorkerDiagnostics,
) -> Result<Option<WorkerFailure>, ()> {
    let mut observed_failure = None;
    match frame {
        WorkerFrame::Pulse {
            native_progress,
            mut diagnostics,
        } if terminal.is_none() => {
            if native_progress < progress.0 {
                return Err(());
            }
            if native_progress > progress.0 {
                *progress = (native_progress, Instant::now());
            }
            // Only this supervisor owns process identity and containment proof.
            observed_failure =
                reported_browser_failure(&diagnostics).then_some(WorkerFailure::BrowserProcess);
            diagnostics.worker = None;
            bridge.control.record_fault(diagnostics);
            if let Some(failure) = observed_failure {
                observe_native_failure(bridge, worker, failure);
            }
        }
        WorkerFrame::Ready if terminal.is_none() => bridge.emit(Status::Ready),
        WorkerFrame::LoginShown if terminal.is_none() => bridge.emit(Status::LoginShown),
        WorkerFrame::LoginFailed if terminal.is_none() => bridge.emit(Status::LoginFailed),
        WorkerFrame::Paused { code } if terminal.is_none() => {
            observed_failure = native_failure_code(code);
            if let Some(failure) = observed_failure {
                observe_native_failure(bridge, worker, failure);
            }
            bridge.emit(Status::Paused { code });
        }
        WorkerFrame::Completed {
            ticket,
            job_id,
            channel,
            outcome,
        } if terminal.is_none() => {
            let Some(job) = bridge.control.pending_job() else {
                return Err(());
            };
            if job.ticket != ticket || job.job_id != job_id || job.channel != channel {
                return Err(());
            }
            let outcome = outcome.into_source().map_err(|_| ())?;
            observed_failure = outcome
                .as_ref()
                .err()
                .and_then(|failure| native_failure_code(failure.code));
            if let Some(failure) = observed_failure {
                observe_native_failure(bridge, worker, failure);
            }
            if !bridge.control.complete(ticket, outcome) {
                return Err(());
            }
        }
        WorkerFrame::Stopped {
            cleanup_confirmed,
            mut diagnostics,
        } if terminal.is_none() => {
            bridge.control.stop_accepting();
            observed_failure = if reported_browser_failure(&diagnostics) {
                Some(WorkerFailure::BrowserProcess)
            } else if !cleanup_confirmed
                && diagnostics
                    .host_termination
                    .as_ref()
                    .is_some_and(|termination| {
                        termination.reason == HostTerminationReason::CleanupUnconfirmed
                    })
            {
                Some(WorkerFailure::CleanupUnconfirmed)
            } else {
                None
            };
            diagnostics.worker = None;
            // Preserve a completely received final source/cleanup snapshot even
            // if the worker then crashes in its remaining native teardown.
            bridge.control.record_fault(diagnostics.clone());
            if let Some(failure) = observed_failure {
                observe_native_failure(bridge, worker, failure);
            }
            *terminal = Some((cleanup_confirmed, diagnostics));
        }
        _ => return Err(()),
    }
    // Browser-process proof takes precedence over a later generic cleanup code.
    Ok(observed_failure.map(|_| worker.failure.unwrap()))
}

fn native_failure_code(code: &str) -> Option<WorkerFailure> {
    match code {
        "browser_process_failed" => Some(WorkerFailure::BrowserProcess),
        "cleanup_unconfirmed" | "controller_cleanup_unconfirmed" => {
            Some(WorkerFailure::CleanupUnconfirmed)
        }
        _ => None,
    }
}

fn observe_native_failure(bridge: &Bridge, worker: &mut WorkerDiagnostics, failure: WorkerFailure) {
    bridge.control.stop_accepting();
    if worker.failure != Some(WorkerFailure::BrowserProcess) {
        worker.failure = Some(failure);
    }
    // Publish the parent-authored retirement cause before forwarding Paused or
    // Completed. Neither child diagnostics nor a stale cleanup pulse can grant
    // process identity, containment, or automatic replacement.
    let mut report = bridge.control.fault_report();
    report.worker = Some(worker.clone());
    bridge.control.record_fault(report);
}

fn reported_browser_failure(report: &FaultReport) -> bool {
    report
        .primary_failure
        .as_ref()
        .is_some_and(|failure| failure.code == "browser_process_failed")
        || report
            .cleanup_diagnostics
            .as_ref()
            .is_some_and(crate::host::CleanupDiagnostics::browser_process_failed)
}

fn fail_worker(bridge: &Bridge, diagnostics: WorkerDiagnostics) {
    publish_worker_failure(bridge, diagnostics, true);
}

fn publish_worker_failure(bridge: &Bridge, diagnostics: WorkerDiagnostics, terminal: bool) {
    let code = match diagnostics.failure {
        Some(WorkerFailure::Start | WorkerFailure::NotCreated) => "worker_start_failed",
        Some(WorkerFailure::BrowserProcess) => "browser_process_failed",
        Some(WorkerFailure::CleanupUnconfirmed) => "cleanup_unconfirmed",
        Some(WorkerFailure::Protocol) => "worker_protocol_failure",
        Some(WorkerFailure::Stalled) => "worker_stalled",
        Some(WorkerFailure::ShutdownTimeout) => "worker_shutdown_timeout",
        _ => "worker_failed",
    };
    bridge
        .control
        .observe_termination(HostTerminationReason::WorkerFailed);
    let mut report = bridge.control.fault_report();
    report.worker = Some(diagnostics);
    // Do not manufacture BrowserProcessExited, normal exit, Close HRESULT or
    // COM cleanup success from process containment. Preserve original source faults.
    bridge.control.record_fault(report);
    if !terminal {
        bridge.control.stop_accepting();
        if let Some(job) = bridge.control.pending_job() {
            bridge.control.complete(
                job.ticket,
                Err(SourceFailure {
                    code,
                    pauses_community: true,
                }),
            );
        }
    }
    bridge.emit(Status::Paused { code });
    if terminal {
        bridge.control.terminate(false, code);
    }
}

fn in_containment_job() -> bool {
    let mut limits = ExtendedLimits::default();
    unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            9,
            &mut limits as *mut _ as _,
            std::mem::size_of_val(&limits) as u32,
            std::ptr::null_mut(),
        ) != 0
            && limits.basic.flags & KILL_ON_JOB_CLOSE != 0
    }
}

pub(super) fn run_worker() -> ExitCode {
    // Suppress per-process error dialogs so an AV cannot await desktop input.
    unsafe {
        SetErrorMode(0x0001 | 0x0002);
    }
    let mut input = io::stdin();
    let start = read_frame::<ParentFrame>(&mut input);
    let Ok(Some(ParentFrame::Start {
        version: VERSION,
        profile_dir,
    })) = start
    else {
        return ExitCode::from(70);
    };
    if !in_containment_job() || !profile_dir.is_absolute() || profile_dir.as_os_str().len() > 32768
    {
        return ExitCode::from(70);
    }
    let (handle, bridge) = HostHandle::channel();
    let commands = handle.command_sender();
    let control = bridge.control.clone();
    let read_control = control.clone();
    let reader = std::thread::Builder::new()
        .name("community-worker-command".into())
        .spawn(move || loop {
            let accepted = match read_frame::<ParentFrame>(&mut input) {
                Ok(Some(ParentFrame::Control {
                    cancel_version,
                    shutdown,
                })) => {
                    read_control
                        .cancel_version
                        .fetch_max(cancel_version, Ordering::AcqRel);
                    if shutdown {
                        read_control.shutdown.store(true, Ordering::Release);
                    }
                    Ok(())
                }
                Ok(Some(ParentFrame::Sample {
                    ticket,
                    job_id,
                    channel,
                    cancel_version,
                    queued_tick_ms,
                })) => {
                    let now = tick_ms();
                    if ticket == 0
                        || queued_tick_ms > now
                        || crate::channel_login(&channel).as_ref() != Some(&channel)
                        || !crate::is_channel_page(
                            &format!("https://www.twitch.tv/{channel}"),
                            &channel,
                        )
                    {
                        Err(())
                    } else if let Some(queued_at) =
                        Instant::now().checked_sub(Duration::from_millis(now - queued_tick_ms))
                    {
                        read_control.adopt_sample(
                            &commands,
                            AcceptedJob {
                                ticket,
                                job_id,
                                channel,
                                cancel_version,
                                queued_at,
                            },
                        )
                    } else {
                        Err(())
                    }
                }
                Ok(Some(ParentFrame::Login {
                    publication,
                    cancel_version,
                })) => read_control.adopt_login(&commands, publication, cancel_version),
                _ => Err(()),
            };
            if accepted.is_err() {
                std::process::exit(71);
            }
        });
    let writer = std::thread::Builder::new()
        .name("community-worker-event".into())
        .spawn(move || {
            let mut output = io::stdout();
            let mut last_pulse = Instant::now();
            loop {
                let frame = match handle.try_ticketed_event() {
                    Ok((_, HostEvent::Ready)) => Some(WorkerFrame::Ready),
                    Ok((_, HostEvent::LoginShown)) => Some(WorkerFrame::LoginShown),
                    Ok((_, HostEvent::LoginFailed)) => Some(WorkerFrame::LoginFailed),
                    Ok((_, HostEvent::Paused { code })) => Some(WorkerFrame::Paused { code }),
                    Ok((
                        Some(ticket),
                        HostEvent::SampleCompleted {
                            job_id,
                            channel,
                            outcome,
                        },
                    )) => Some(WorkerFrame::Completed {
                        ticket,
                        job_id,
                        channel,
                        outcome: WireOutcome::from_source(outcome),
                    }),
                    Ok((None, HostEvent::SampleCompleted { .. })) => std::process::exit(75),
                    Ok((
                        _,
                        HostEvent::Stopped {
                            cleanup_confirmed,
                            diagnostics,
                        },
                    )) => {
                        if write_frame(
                            &mut output,
                            &WorkerFrame::Stopped {
                                cleanup_confirmed,
                                diagnostics,
                            },
                        )
                        .is_err()
                        {
                            std::process::exit(72);
                        }
                        return;
                    }
                    Err(_) if last_pulse.elapsed() >= Duration::from_millis(250) => {
                        last_pulse = Instant::now();
                        Some(WorkerFrame::Pulse {
                            native_progress: control.native_progress.load(Ordering::Acquire),
                            diagnostics: handle.fault_report(),
                        })
                    }
                    Err(_) => None,
                };
                if let Some(frame) = frame {
                    if write_frame(&mut output, &frame).is_err() {
                        std::process::exit(72);
                    }
                } else {
                    std::thread::sleep(POLL);
                }
            }
        });
    if reader.is_err() || writer.is_err() {
        return ExitCode::from(73);
    }
    // This is the sole production service call. It runs on the worker's main
    // STA thread, after both the OS Job and protocol startup gates succeeded.
    crate::native::run_service(HostOptions { profile_dir }, bridge);
    if writer.unwrap().join().is_err() {
        return ExitCode::from(74);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};

    #[repr(C)]
    struct Security {
        size: u32,
        descriptor: *mut std::ffi::c_void,
        inherit: i32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateEventW(
            security: *const Security,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> Handle;
        fn GetHandleInformation(handle: Handle, flags: *mut u32) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    }

    fn fixture_path() -> PathBuf {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples")
            .join("community-worker-fixture.exe")
    }

    fn fixture(mode: &str) -> OwnedWorker {
        let binary = fixture_path();
        assert!(
            binary.is_file(),
            "build the offline community-worker-fixture example before testing"
        );
        let mut command = Command::new(binary);
        command.arg(mode).env_clear();
        OwnedWorker::launch(&mut command).unwrap()
    }

    fn options() -> HostOptions {
        // Never created: the executable fixture has no native/browser code.
        HostOptions {
            profile_dir: PathBuf::from(r"C:\offline-worker-fixture-unused"),
        }
    }

    fn terminal(handle: &HostHandle) -> (bool, FaultReport) {
        loop {
            match handle
                .try_event()
                .unwrap_or_else(|_| panic!("missing reserved terminal"))
            {
                HostEvent::Stopped {
                    cleanup_confirmed,
                    diagnostics,
                } => return (cleanup_confirmed, diagnostics),
                HostEvent::SampleCompleted { .. } => panic!("unexpected extra completion"),
                _ => {}
            }
        }
    }

    #[test]
    fn unexpected_child_exit_completes_once_and_preserves_reserved_terminal() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(41, "room").unwrap();
        for _ in 0..16 {
            bridge.emit(Status::Ready);
        }
        supervise_owned(options(), bridge, Ok(fixture("unexpected_exit")));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 41,
                outcome: Err(SourceFailure {
                    code: "worker_failed",
                    pauses_community: true
                }),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        let worker = report.worker.unwrap();
        assert_eq!(worker.exit_code, Some(23));
        assert_eq!(worker.failure, Some(WorkerFailure::UnexpectedExit));
        assert!(worker.worker_tree_exited && worker.containment_confirmed);
        assert!(report.cleanup_diagnostics.is_none());
        assert!(handle.try_event().is_err());
        assert_eq!(
            handle.try_sample(42, "room"),
            Err(crate::host::CommandError::Closed)
        );
    }

    #[test]
    fn launch_failure_before_any_child_is_distinct_from_unknown_post_spawn_ownership() {
        let mut missing = Command::new(r"C:\offline-worker-fixture-does-not-exist.exe");
        assert!(matches!(
            OwnedWorker::launch(&mut missing),
            Err(LaunchFailure::NotCreated)
        ));
        for (failure, expected) in [
            (LaunchFailure::NotCreated, WorkerFailure::NotCreated),
            (LaunchFailure::OwnershipUnknown, WorkerFailure::Start),
        ] {
            let (handle, bridge) = HostHandle::channel();
            supervise_owned(options(), bridge, Err(failure));
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            let diagnostic = report.worker.unwrap();
            assert_eq!(diagnostic.failure, Some(expected));
            assert!(diagnostic.process_id.is_none() && diagnostic.exit_code.is_none());
            assert!(!diagnostic.worker_tree_exited && !diagnostic.containment_confirmed);
        }
    }

    #[test]
    fn native_access_violation_is_contained_without_fabricated_com_cleanup() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(45, "room").unwrap();
        supervise_owned(options(), bridge, Ok(fixture("access_violation")));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                outcome: Err(_),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        assert!(report.cleanup_diagnostics.is_none());
        let worker = report.worker.unwrap();
        assert_eq!(worker.exit_code, Some(0xC000_0005));
        assert!(worker.containment_confirmed && worker.worker_tree_exited);
    }

    #[test]
    fn temporarily_unknown_containment_keeps_exact_handles_and_completes_only_once() {
        for failure in [
            WorkerFailure::UnexpectedExit,
            WorkerFailure::BrowserProcess,
            WorkerFailure::CleanupUnconfirmed,
        ] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(46, "room").unwrap();
            let mut worker = fixture("unexpected_exit");
            let mut diagnostics = WorkerDiagnostics {
                process_id: Some(worker.child.id()),
                started_at_unix_ms: worker.child.started_at_unix_ms,
                failure: Some(failure),
                ..Default::default()
            };
            let mut probes = 0;
            assert!(quarantine_worker(
                &mut worker,
                &bridge,
                &mut diagnostics,
                |owned| {
                    probes += 1;
                    if probes == 1 {
                        assert!(matches!(
                            handle.try_event(),
                            Ok(HostEvent::SampleCompleted {
                                job_id: 46,
                                outcome: Err(_),
                                ..
                            })
                        ));
                        assert!(!handle.fault_report().worker.unwrap().containment_confirmed);
                        assert!(owned.job.active().is_ok());
                        (None, false)
                    } else {
                        owned.probe_containment()
                    }
                }
            ));
            assert!(probes >= 2);
            assert!(diagnostics.worker_tree_exited && diagnostics.containment_confirmed);
            assert!(diagnostics.exit_code.is_some());
            fail_worker(&bridge, diagnostics);
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            assert!(report.worker.unwrap().containment_confirmed);
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn shutdown_cancels_quarantine_without_fabricating_late_containment() {
        for failure in [
            WorkerFailure::UnexpectedExit,
            WorkerFailure::BrowserProcess,
            WorkerFailure::CleanupUnconfirmed,
        ] {
            let (handle, bridge) = HostHandle::channel();
            let mut worker = fixture("unexpected_exit");
            let mut diagnostics = WorkerDiagnostics {
                process_id: Some(worker.child.id()),
                started_at_unix_ms: worker.child.started_at_unix_ms,
                failure: Some(failure),
                ..Default::default()
            };
            handle.shutdown();
            assert!(!quarantine_worker(
                &mut worker,
                &bridge,
                &mut diagnostics,
                |_| { panic!("cancelled quarantine must not run another probe") }
            ));
            assert!(!diagnostics.containment_confirmed);
            fail_worker(&bridge, diagnostics);
            assert!(!terminal(&handle).1.worker.unwrap().containment_confirmed);
            // Test cleanup observes the actual owned objects after cancellation.
            assert!(worker.finish(false).1);
        }
    }

    #[test]
    fn fully_delivered_receipt_survives_a_later_unexpected_child_exit() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(51, "room").unwrap();
        supervise_owned(options(), bridge, Ok(fixture("success_then_exit")));
        let Ok(HostEvent::SampleCompleted {
            job_id: 51,
            outcome: Ok(sample),
            ..
        }) = handle.try_event()
        else {
            panic!("fully delivered receipt lost");
        };
        assert_eq!(sample.received_at_unix_ms, 123456);
        assert_eq!(sample.members[0].account, "offline_account");
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        assert_eq!(report.worker.unwrap().exit_code, Some(23));
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn validated_browser_failure_frames_retire_even_an_exit_zero_worker_and_keep_source_barriers() {
        for mode in [
            "browser_pulse_exit0",
            "browser_stopped_exit0",
            "browser_paused_exit0",
            "browser_completed_exit0",
            "browser_denial_before",
            "browser_denial_after",
        ] {
            let (handle, bridge) = HostHandle::channel();
            let control = bridge.control.clone();
            handle.try_sample(53, "room").unwrap();
            for _ in 0..16 {
                bridge.emit(Status::Ready);
            }
            supervise_owned(options(), bridge, Ok(fixture(mode)));
            assert!(
                matches!(
                    handle.try_event(),
                    Ok(HostEvent::SampleCompleted {
                        job_id: 53,
                        outcome: Err(SourceFailure {
                            code: "browser_process_failed",
                            ..
                        }),
                        ..
                    })
                ),
                "{mode}"
            );
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            assert!(!control.shutdown.load(Ordering::Acquire));
            let worker = report.worker.unwrap();
            assert_eq!(
                worker.failure,
                Some(WorkerFailure::BrowserProcess),
                "{mode}"
            );
            assert_eq!(worker.exit_code, Some(0), "{mode}");
            assert!(worker.worker_tree_exited && worker.containment_confirmed);
            assert_eq!(
                report.source_barrier.as_ref().map(|barrier| barrier.code),
                mode.starts_with("browser_denial_")
                    .then_some("authentication_denied")
            );
            assert_eq!(
                handle.try_sample(54, "room"),
                Err(crate::host::CommandError::Closed)
            );
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn browser_failure_pulse_with_no_paused_frame_bounds_stuck_teardown_before_exact_job_retirement(
    ) {
        let (handle, bridge) = HostHandle::channel();
        let control = bridge.control.clone();
        handle.try_sample(55, "room").unwrap();
        let started = Instant::now();
        let supervisor = std::thread::spawn(move || {
            supervise_owned(options(), bridge, Ok(fixture("browser_pulse_hang")));
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let report = handle.fault_report();
            if report
                .worker
                .as_ref()
                .is_some_and(|worker| worker.failure == Some(WorkerFailure::BrowserProcess))
            {
                let worker = report.worker.unwrap();
                assert!(!worker.worker_tree_exited && !worker.containment_confirmed);
                assert!(worker.exit_code.is_none());
                assert!(report.source_barrier.is_none());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "browser failure was not published before retirement"
            );
            std::thread::sleep(POLL);
        }
        assert!(!control.shutdown.load(Ordering::Acquire));
        assert_eq!(
            handle.try_sample(56, "room"),
            Err(crate::host::CommandError::Closed)
        );
        supervisor.join().unwrap();
        assert!(started.elapsed() < SHUTDOWN_LIMIT + TREE_EXIT_LIMIT + Duration::from_secs(3));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 55,
                outcome: Err(SourceFailure {
                    code: "browser_process_failed",
                    ..
                }),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        let worker = report.worker.unwrap();
        assert_eq!(worker.failure, Some(WorkerFailure::BrowserProcess));
        assert!(worker.worker_tree_exited && worker.containment_confirmed);
        assert!(worker.exit_code.is_some());
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn delivered_positive_receipt_survives_a_later_browser_subprocess_failure() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(57, "room").unwrap();
        supervise_owned(options(), bridge, Ok(fixture("success_then_browser_exit0")));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 57,
                outcome: Ok(_),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        let worker = report.worker.unwrap();
        assert_eq!(worker.failure, Some(WorkerFailure::BrowserProcess));
        assert_eq!(worker.exit_code, Some(0));
        assert!(worker.worker_tree_exited && worker.containment_confirmed);
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn a_positive_then_matched_failed_browser_exit_retires_the_worker_without_a_browser_primary() {
        for mode in [
            "success_then_failed_cleanup_exit0",
            "success_then_failed_cleanup_pulse_exit0",
            "success_then_failed_cleanup_paused_exit0",
            "success_then_failed_cleanup_pulse_denial_before_exit0",
            "success_then_failed_cleanup_pulse_denial_after_exit0",
        ] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(58, "room").unwrap();
            supervise_owned(options(), bridge, Ok(fixture(mode)));
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::SampleCompleted {
                    job_id: 58,
                    outcome: Ok(_),
                    ..
                })
            ));
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            assert!(report.primary_failure.is_none());
            assert!(report.cleanup_diagnostics.unwrap().browser_process_failed());
            let worker = report.worker.unwrap();
            assert_eq!(
                worker.failure,
                Some(WorkerFailure::BrowserProcess),
                "{mode}"
            );
            assert_eq!(worker.exit_code, Some(0));
            assert!(worker.worker_tree_exited && worker.containment_confirmed);
            assert_eq!(
                report.source_barrier.as_ref().map(|barrier| barrier.code),
                mode.contains("denial_").then_some("authentication_denied")
            );
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn unknown_mismatched_or_normal_cleanup_after_success_never_acquires_a_browser_failure_cause() {
        for negative in [
            "mismatch",
            "zero",
            "missing",
            "normal",
            "unknown",
            "contradictory",
        ] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(59, "room").unwrap();
            let mode = format!("success_then_cleanup_{negative}_exit0");
            supervise_owned(options(), bridge, Ok(fixture(&mode)));
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::SampleCompleted {
                    job_id: 59,
                    outcome: Ok(_),
                    ..
                })
            ));
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            assert!(!report.cleanup_diagnostics.unwrap().browser_process_failed());
            assert!(report.primary_failure.is_none() && report.source_barrier.is_none());
            let worker = report.worker.unwrap();
            assert_eq!(worker.failure, None, "{negative}");
            assert_eq!(worker.exit_code, Some(0));
            assert!(worker.worker_tree_exited && worker.containment_confirmed);
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn successful_receipt_then_fresh_cleanup_failure_keeps_exit_zero_and_missing_exit_evidence() {
        for suffix in [
            "paused_exit0",
            "terminal_only_exit0",
            "close_exit0",
            "detach_exit0",
            "unsubscribe_exit0",
        ] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(60, "room").unwrap();
            for _ in 0..16 {
                bridge.emit(Status::Ready);
            }
            let mode = format!("success_then_cleanup_only_{suffix}");
            supervise_owned(options(), bridge, Ok(fixture(&mode)));
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::SampleCompleted {
                    job_id: 60,
                    outcome: Ok(_),
                    ..
                })
            ));
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup, "{suffix}");
            assert!(report.primary_failure.is_none() && report.source_barrier.is_none());
            let cleanup = report.cleanup_diagnostics.unwrap();
            assert!(!cleanup.confirmed && !cleanup.browser_process_failed());
            assert_eq!(cleanup.expected_browser_pid, Some(100));
            assert_eq!(cleanup.elapsed_ms, 5050);
            if !cleanup.normal_exit_observed {
                assert!(
                    cleanup.observed_exit_pid.is_none() && cleanup.observed_exit_kind.is_none()
                );
                assert_eq!(
                    (cleanup.close_hresult, cleanup.unsubscribe_hresult),
                    (Some(0), Some(0))
                );
                assert!(cleanup.first_detach_failure.is_none());
            }
            let worker = report.worker.unwrap();
            assert_eq!(
                worker.failure,
                Some(WorkerFailure::CleanupUnconfirmed),
                "{suffix}"
            );
            assert_eq!(worker.exit_code, Some(0));
            assert!(worker.worker_tree_exited && worker.containment_confirmed);
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn every_received_source_barrier_survives_cleanup_ordering_and_a_full_status_queue() {
        for code in [
            "authentication_denied",
            "integrity_denied",
            "challenge_denied",
            "rate_limited",
            "source_errors",
        ] {
            for order in ["before", "after"] {
                let (handle, bridge) = HostHandle::channel();
                handle.try_sample(61, "room").unwrap();
                for _ in 0..16 {
                    bridge.emit(Status::Ready);
                }
                let mode = format!("success_then_cleanup_only_{code}_{order}_exit0");
                supervise_owned(options(), bridge, Ok(fixture(&mode)));
                assert!(matches!(
                    handle.try_event(),
                    Ok(HostEvent::SampleCompleted {
                        job_id: 61,
                        outcome: Ok(_),
                        ..
                    })
                ));
                let (cleanup, report) = terminal(&handle);
                assert!(!cleanup);
                assert_eq!(report.source_barrier.unwrap().code, code, "{order}");
                assert_eq!(
                    report.worker.unwrap().failure,
                    Some(WorkerFailure::CleanupUnconfirmed)
                );
                assert!(handle.try_event().is_err());
            }
        }
    }

    #[test]
    fn cleanup_completion_and_bounded_stuck_retirement_publish_parent_failure_before_terminal() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(62, "room").unwrap();
        supervise_owned(options(), bridge, Ok(fixture("cleanup_completed_exit0")));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 62,
                outcome: Err(SourceFailure {
                    code: "cleanup_unconfirmed",
                    pauses_community: false
                }),
                ..
            })
        ));
        let (_, report) = terminal(&handle);
        assert_eq!(
            report.worker.unwrap().failure,
            Some(WorkerFailure::CleanupUnconfirmed)
        );
        assert!(handle.try_event().is_err());

        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(63, "room").unwrap();
        let started = Instant::now();
        let supervisor = std::thread::spawn(move || {
            supervise_owned(
                options(),
                bridge,
                Ok(fixture("success_then_cleanup_only_hang")),
            );
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let report = handle.fault_report();
            if report
                .worker
                .as_ref()
                .is_some_and(|worker| worker.failure == Some(WorkerFailure::CleanupUnconfirmed))
            {
                assert!(!report.worker.as_ref().unwrap().containment_confirmed);
                assert!(report.worker.as_ref().unwrap().exit_code.is_none());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cleanup failure was not published before terminal"
            );
            std::thread::sleep(POLL);
        }
        supervisor.join().unwrap();
        assert!(started.elapsed() < SHUTDOWN_LIMIT + TREE_EXIT_LIMIT + Duration::from_secs(3));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 63,
                outcome: Ok(_),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        let diagnostic = report.cleanup_diagnostics.unwrap();
        assert_eq!(
            diagnostic.elapsed_ms, 5050,
            "empty progress must not erase cleanup evidence"
        );
        assert!(diagnostic.observed_exit_pid.is_none() && diagnostic.observed_exit_kind.is_none());
        let worker = report.worker.unwrap();
        assert_eq!(worker.failure, Some(WorkerFailure::CleanupUnconfirmed));
        assert!(worker.worker_tree_exited && worker.containment_confirmed);
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn stale_cleanup_progress_and_unrelated_false_terminal_do_not_create_a_retirement_failure() {
        for reason in [
            None,
            Some(HostTerminationReason::ShutdownRequested),
            Some(HostTerminationReason::NativeHostInterrupted),
        ] {
            let (handle, bridge) = HostHandle::channel();
            let mut progress = (0, Instant::now());
            let mut terminal = None;
            let mut worker = WorkerDiagnostics {
                process_id: Some(100),
                started_at_unix_ms: Some(10),
                ..Default::default()
            };
            let stale = FaultReport {
                cleanup_diagnostics: Some(crate::host::CleanupDiagnostics {
                    expected_browser_pid: Some(200),
                    elapsed_ms: 5050,
                    ..Default::default()
                }),
                host_termination: Some(crate::host::HostTermination {
                    reason: HostTerminationReason::CleanupUnconfirmed,
                    observed_at_unix_ms: 1,
                    controller_epoch: 1,
                    job_id: None,
                }),
                ..Default::default()
            };
            assert_eq!(
                accept_frame(
                    &bridge,
                    WorkerFrame::Pulse {
                        native_progress: 1,
                        diagnostics: stale
                    },
                    &mut progress,
                    &mut terminal,
                    &mut worker
                )
                .unwrap(),
                None
            );
            let report = FaultReport {
                host_termination: reason.map(|reason| crate::host::HostTermination {
                    reason,
                    observed_at_unix_ms: 2,
                    controller_epoch: 2,
                    job_id: None,
                }),
                ..Default::default()
            };
            assert_eq!(
                accept_frame(
                    &bridge,
                    WorkerFrame::Stopped {
                        cleanup_confirmed: false,
                        diagnostics: report
                    },
                    &mut progress,
                    &mut terminal,
                    &mut worker
                )
                .unwrap(),
                None
            );
            assert!(worker.failure.is_none());
            assert!(handle.fault_report().worker.is_none());
        }
    }

    #[test]
    fn final_source_fault_survives_an_abnormal_exit_after_the_stopped_frame() {
        let (handle, bridge) = HostHandle::channel();
        supervise_owned(options(), bridge, Ok(fixture("stopped_then_exit")));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        assert_eq!(
            report.primary_failure.unwrap().code,
            "authentication_denied"
        );
        assert_eq!(report.worker.unwrap().exit_code, Some(23));
    }

    #[test]
    fn received_pulse_or_stopped_denial_survives_empty_pulses_and_a_native_av_without_paused() {
        for (mode, expected) in [
            ("denial_pulse_then_av", "authentication_denied"),
            ("source_denied_pulse_then_av", "source_denied"),
            ("denial_stopped_then_av", "authentication_denied"),
        ] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(52, "room").unwrap();
            for _ in 0..16 {
                bridge.emit(Status::Ready);
            }
            supervise_owned(options(), bridge, Ok(fixture(mode)));
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::SampleCompleted {
                    job_id: 52,
                    outcome: Err(SourceFailure {
                        code: "worker_failed",
                        ..
                    }),
                    ..
                })
            ));
            assert_eq!(handle.fault_report().source_barrier.unwrap().code, expected);
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            assert_eq!(report.source_barrier.unwrap().code, expected);
            let worker = report.worker.unwrap();
            assert_eq!(worker.exit_code, Some(0xC000_0005));
            assert!(worker.worker_tree_exited && worker.containment_confirmed);
            assert!(report.cleanup_diagnostics.is_none());
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn malformed_oversized_and_partial_child_frames_never_complete_a_sample_successfully() {
        for mode in ["malformed", "oversized", "partial", "wrong_ticket"] {
            let (handle, bridge) = HostHandle::channel();
            handle.try_sample(61, "room").unwrap();
            supervise_owned(options(), bridge, Ok(fixture(mode)));
            assert!(matches!(
                handle.try_event(),
                Ok(HostEvent::SampleCompleted {
                    outcome: Err(SourceFailure {
                        code: "worker_protocol_failure",
                        ..
                    }),
                    ..
                })
            ));
            let (cleanup, report) = terminal(&handle);
            assert!(!cleanup);
            assert_eq!(
                report.worker.unwrap().failure,
                Some(WorkerFailure::Protocol)
            );
            assert!(handle.try_event().is_err());
        }
    }

    #[test]
    fn cancellation_and_shutdown_cross_the_pipe_even_when_the_command_queue_is_full() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(71, "room").unwrap();
        handle.show_login().unwrap();
        for _ in 0..16 {
            bridge.emit(Status::Ready);
        }
        handle.cancel();
        handle.shutdown();
        supervise_owned(options(), bridge, Ok(fixture("shutdown")));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                job_id: 71,
                outcome: Err(SourceFailure {
                    code: "operator_cancelled",
                    ..
                }),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(cleanup);
        let worker = report.worker.unwrap();
        assert_eq!(worker.exit_code, Some(0));
        assert!(worker.containment_confirmed);
        assert!(handle.try_event().is_err());
    }

    #[test]
    fn exact_job_contains_a_gated_child_before_any_start_frame() {
        let mut worker = fixture("unexpected_exit");
        assert_eq!(worker.job.active().unwrap(), 1);
        assert!(worker.child.try_wait().unwrap().is_none());
        let (_, contained) = worker.finish(false);
        assert!(contained);
        assert_ne!(worker.child.try_wait().unwrap().unwrap().code(), Some(0));
    }

    #[test]
    fn worker_exit_does_not_prove_descendant_exit_but_the_exact_job_does() {
        let mut worker = fixture("descendant_owner");
        write_frame(
            worker.child.stdin.as_mut().unwrap(),
            &ParentFrame::Start {
                version: VERSION,
                profile_dir: options().profile_dir,
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<WorkerFrame>(worker.child.stdout.as_mut().unwrap()).unwrap(),
            Some(WorkerFrame::Ready)
        ));
        let started = Instant::now();
        while worker.child.try_wait().unwrap().is_none() {
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::sleep(POLL);
        }
        assert!(
            worker.job.active().unwrap() >= 1,
            "child PID exit is not a tree barrier"
        );
        let (exit, contained) = worker.finish(false);
        assert_eq!(exit, Some(0));
        assert!(contained);
        assert_eq!(worker.job.active().unwrap(), 0);
    }

    #[test]
    fn closing_the_only_job_handle_kills_the_worker_and_its_descendant() {
        let mut worker = fixture("descendant_owner_wait");
        write_frame(
            worker.child.stdin.as_mut().unwrap(),
            &ParentFrame::Start {
                version: VERSION,
                profile_dir: options().profile_dir,
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<WorkerFrame>(worker.child.stdout.as_mut().unwrap()).unwrap(),
            Some(WorkerFrame::Ready)
        ));
        let message: serde_json::Value = read_frame(worker.child.stdout.as_mut().unwrap())
            .unwrap()
            .unwrap();
        let descendant_pid = message["descendant"].as_u64().unwrap() as u32;
        let descendant = unsafe { OpenProcess(0x0010_0000, 0, descendant_pid) };
        assert!(!descendant.is_null());
        let descendant = unsafe { OwnedHandle::from_raw_handle(descendant) };
        assert!(worker.job.active().unwrap() >= 2);
        worker.job.close();
        assert_eq!(
            unsafe { WaitForSingleObject(worker.child.as_raw_handle(), 5000) },
            0
        );
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5000) },
            0
        );
    }

    #[test]
    fn stdio_handle_list_excludes_an_inheritable_sentinel_and_the_job_handle() {
        let security = Security {
            size: std::mem::size_of::<Security>() as u32,
            descriptor: std::ptr::null_mut(),
            inherit: 1,
        };
        let sentinel = unsafe { CreateEventW(&security, 1, 0, std::ptr::null()) };
        assert!(!sentinel.is_null());
        let sentinel = unsafe { OwnedHandle::from_raw_handle(sentinel) };
        let mut flags = 0;
        assert_ne!(
            unsafe { GetHandleInformation(sentinel.as_raw_handle(), &mut flags) },
            0
        );
        assert_eq!(flags & 1, 1);
        let mut command = Command::new(fixture_path());
        command
            .arg("sentinel")
            .arg((sentinel.as_raw_handle() as usize).to_string())
            .env_clear();
        let mut worker = OwnedWorker::launch(&mut command).unwrap();
        assert_ne!(unsafe { GetHandleInformation(worker.job.0, &mut flags) }, 0);
        assert_eq!(
            flags & 1,
            0,
            "the worker cannot keep the kill-on-close Job alive"
        );
        write_frame(
            worker.child.stdin.as_mut().unwrap(),
            &ParentFrame::Start {
                version: VERSION,
                profile_dir: options().profile_dir,
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame::<WorkerFrame>(worker.child.stdout.as_mut().unwrap()).unwrap(),
            Some(WorkerFrame::Ready)
        ));
        assert_eq!(
            unsafe { WaitForSingleObject(sentinel.as_raw_handle(), 0) },
            0x102,
            "worker inherited an unrelated parent handle"
        );
        assert!(worker.finish(true).1);
    }

    #[test]
    fn live_ipc_cannot_hide_a_stalled_native_thread() {
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(81, "room").unwrap();
        let started = Instant::now();
        supervise_owned(options(), bridge, Ok(fixture("stalled_progress")));
        assert!(started.elapsed() < STALL_LIMIT + TREE_EXIT_LIMIT + Duration::from_secs(2));
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                outcome: Err(_),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        assert_eq!(report.worker.unwrap().failure, Some(WorkerFailure::Stalled));
    }

    #[test]
    fn a_blocked_pipe_writer_is_bounded_independently_of_native_progress() {
        let (handle, bridge) = HostHandle::channel();
        let long_profile = PathBuf::from(format!("C:\\{}", "x".repeat(16384)));
        let started = Instant::now();
        supervise_owned(
            HostOptions {
                profile_dir: long_profile,
            },
            bridge,
            Ok(fixture("never_reads")),
        );
        assert!(started.elapsed() < PIPE_WRITE_LIMIT + TREE_EXIT_LIMIT + Duration::from_secs(2));
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        assert_eq!(report.worker.unwrap().failure, Some(WorkerFailure::Stalled));
    }

    #[test]
    fn supervisor_retirement_budget_includes_all_exit_barriers_and_publication_margin() {
        let worst_case = SHUTDOWN_LIMIT + EXIT_GRACE_LIMIT + TREE_EXIT_LIMIT + PROCESS_EXIT_LIMIT;
        assert!(worst_case + POLL * 4 <= crate::host::MAX_WORKER_RETIREMENT);
    }

    #[test]
    fn ignored_shutdown_is_forced_within_the_shared_retirement_budget() {
        let (handle, bridge) = HostHandle::channel();
        handle.shutdown();
        let started = Instant::now();
        supervise_owned(options(), bridge, Ok(fixture("shutdown_ignored")));
        assert!(started.elapsed() < crate::host::MAX_WORKER_RETIREMENT);
        let (cleanup, report) = terminal(&handle);
        assert!(!cleanup);
        let worker = report.worker.unwrap();
        assert_eq!(worker.failure, Some(WorkerFailure::ShutdownTimeout));
        assert!(worker.containment_confirmed && worker.worker_tree_exited);
    }

    #[test]
    fn environment_is_an_os_allowlist_and_main_host_never_calls_native_service() {
        let command = worker_command().unwrap();
        let keys: Vec<_> = command
            .get_envs()
            .map(|(key, _)| key.to_string_lossy().to_ascii_lowercase())
            .collect();
        assert!(!keys
            .iter()
            .any(|key| key.contains("twitch") || key.contains("secret") || key.contains("token")));
        let host = include_str!("../host.rs");
        let spawn = host
            .split("pub fn spawn(options:")
            .nth(1)
            .unwrap()
            .split("pub(crate) fn channel")
            .next()
            .unwrap();
        assert!(!spawn.contains("native::"));
        assert!(spawn.contains("crate::worker::spawn"));
    }

    #[test]
    fn every_webview_override_is_refused_before_any_worker_or_profile_exists() {
        for name in WEBVIEW_ENVIRONMENT_OVERRIDES {
            assert!(has_environment_override(|candidate| candidate == name));
        }
        assert!(!has_environment_override(|_| false));
        let (handle, bridge) = HostHandle::channel();
        handle.try_sample(91, "room").unwrap();
        refuse_environment_override(&bridge);
        assert!(matches!(
            handle.try_event(),
            Ok(HostEvent::SampleCompleted {
                outcome: Err(SourceFailure {
                    code: "webview2_environment_override",
                    ..
                }),
                ..
            })
        ));
        let (cleanup, report) = terminal(&handle);
        assert!(cleanup);
        assert!(report.worker.is_none());
        assert!(
            report
                .cleanup_diagnostics
                .unwrap()
                .no_browser_cleanup_required
        );
        assert_eq!(
            report.primary_failure.unwrap().code,
            "webview2_environment_override"
        );
    }

    #[test]
    fn a_pulse_after_stopped_cannot_erase_the_final_fault() {
        let (handle, bridge) = HostHandle::channel();
        let mut progress = (0, Instant::now());
        let mut terminal = None;
        let mut worker = WorkerDiagnostics::default();
        let final_report = FaultReport {
            cleanup_diagnostics: Some(crate::host::CleanupDiagnostics {
                close_hresult: Some(-7),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(accept_frame(
            &bridge,
            WorkerFrame::Stopped {
                cleanup_confirmed: false,
                diagnostics: final_report
            },
            &mut progress,
            &mut terminal,
            &mut worker
        )
        .is_ok());
        assert!(accept_frame(
            &bridge,
            WorkerFrame::Pulse {
                native_progress: 1,
                diagnostics: FaultReport::default()
            },
            &mut progress,
            &mut terminal,
            &mut worker
        )
        .is_err());
        assert_eq!(
            handle
                .fault_report()
                .cleanup_diagnostics
                .unwrap()
                .close_hresult,
            Some(-7)
        );
    }
}
