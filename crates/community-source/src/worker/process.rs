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
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(10);
const PIPE_WRITE_LIMIT: Duration = Duration::from_secs(3);
const TREE_EXIT_LIMIT: Duration = Duration::from_secs(5);
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
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct OwnedWorker {
    child: ChildProcess,
    job: Job,
}
impl OwnedWorker {
    fn launch(command: &mut Command) -> io::Result<Self> {
        let job = Job::create()?;
        let mut child = ChildProcess::spawn(command)?;
        if let Err(error) = job.assign(&child) {
            // Still gated on stdin: this child has never touched COM or a profile.
            let _ = child.kill();
            let started = Instant::now();
            while matches!(child.try_wait(), Ok(None)) && started.elapsed() < Duration::from_secs(1)
            {
                std::thread::sleep(POLL);
            }
            return Err(error);
        }
        Ok(Self { child, job })
    }
    fn finish(&mut self, allow_graceful: bool) -> (Option<u32>, bool) {
        if allow_graceful {
            let started = Instant::now();
            while matches!(self.child.try_wait(), Ok(None))
                && started.elapsed() < Duration::from_millis(500)
            {
                std::thread::sleep(POLL);
            }
        }
        self.job.terminate();
        let empty = self.job.wait_empty();
        let code = self
            .child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code())
            .map(|n| n as u32);
        (code, empty)
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

enum Inbound {
    Frame(WorkerFrame),
    Eof,
    Invalid,
}

fn supervise(options: HostOptions, bridge: Bridge) {
    let launched = worker_command().and_then(|mut command| OwnedWorker::launch(&mut command));
    supervise_owned(options, bridge, launched);
}

fn supervise_owned(options: HostOptions, bridge: Bridge, launched: io::Result<OwnedWorker>) {
    let control = bridge.control.clone();
    let _guard = ServiceExitGuard(control.clone());
    let mut worker = match launched {
        Ok(worker) => worker,
        Err(_) => {
            fail_worker(
                &bridge,
                WorkerDiagnostics {
                    failure: Some(WorkerFailure::Start),
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
    if outbound
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
    while failure.is_none() {
        // FIFO pipe delivery is consumed before examining process exit. A full
        // valid receipt remains completed even if the worker then crashes.
        match inbound.recv_timeout(POLL) {
            Ok(Inbound::Frame(frame)) => {
                match accept_frame(&bridge, frame, &mut last_progress, &mut terminal) {
                    Ok(()) => {}
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
        if terminal.is_some() || shutdown {
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
    let (exit_code, contained) = worker.finish(failure.is_none() && terminal.is_some());
    worker_diagnostics.exit_code = exit_code;
    worker_diagnostics.worker_tree_exited = contained;
    worker_diagnostics.containment_confirmed = contained;
    if failure.is_none() && exit_code != Some(0) {
        failure = Some(WorkerFailure::UnexpectedExit);
    }
    worker_diagnostics.failure = failure;
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

fn accept_frame(
    bridge: &Bridge,
    frame: WorkerFrame,
    progress: &mut (u64, Instant),
    terminal: &mut Option<(bool, FaultReport)>,
) -> Result<(), ()> {
    match frame {
        WorkerFrame::Pulse {
            native_progress,
            diagnostics,
        } => {
            if native_progress < progress.0 {
                return Err(());
            }
            if native_progress > progress.0 {
                *progress = (native_progress, Instant::now());
            }
            bridge.control.record_fault(diagnostics);
        }
        WorkerFrame::Ready if terminal.is_none() => bridge.emit(Status::Ready),
        WorkerFrame::LoginShown if terminal.is_none() => bridge.emit(Status::LoginShown),
        WorkerFrame::LoginFailed if terminal.is_none() => bridge.emit(Status::LoginFailed),
        WorkerFrame::Paused { code } if terminal.is_none() => bridge.emit(Status::Paused { code }),
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
            if !bridge.control.complete(ticket, outcome) {
                return Err(());
            }
        }
        WorkerFrame::Stopped {
            cleanup_confirmed,
            diagnostics,
        } if terminal.is_none() => {
            bridge.control.stop_accepting();
            *terminal = Some((cleanup_confirmed, diagnostics));
        }
        _ => return Err(()),
    }
    Ok(())
}

fn fail_worker(bridge: &Bridge, diagnostics: WorkerDiagnostics) {
    let code = match diagnostics.failure {
        Some(WorkerFailure::Start) => "worker_start_failed",
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
    bridge.emit(Status::Paused { code });
    bridge.control.terminate(false, code);
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

    fn fixture(mode: &str) -> OwnedWorker {
        let binary = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples")
            .join("community-worker-fixture.exe");
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
        handle.show_login().unwrap();
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
    fn malformed_oversized_and_partial_child_frames_never_complete_a_sample_successfully() {
        for mode in ["malformed", "oversized", "partial"] {
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
}
