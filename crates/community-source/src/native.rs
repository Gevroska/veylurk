//! Native Windows proof host. No private HTTP client, request replay, browser
//! flags, cookie API, remote debugging port, or production collection.
use crate::host::{
    AcceptedJob, Bridge, BrowserLifecycle, FaultReport, HostOptions, HostTermination,
    HostTerminationReason, PrimaryFailure, QueuedCommand, ServiceExitGuard, SourceFailure,
    SourceSample, Status, WindowDismissal, RECYCLE_NAVIGATIONS,
};
use crate::{
    channel_login, is_channel_page, is_popout_page, parse_response,
    sampling::{self, BodyRequest, Config, RunEvidence, Schedule, Tracker},
    Evidence, Failure, MAX_SAMPLES,
};
use serde::Deserialize;
use std::sync::{atomic::Ordering, mpsc::TryRecvError};
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    core::{w, Interface, BOOL, PCWSTR, PWSTR},
    Win32::{
        Foundation::{E_UNEXPECTED, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::WindowsAndMessaging::*,
    },
};

const LOGIN_ID: usize = 101;
const OPEN_ID: usize = 102;
const SAMPLE_ID: usize = 103;
const RENDER_ID: usize = 104;
const ARM_TIME: Duration = Duration::from_secs(25);
const ROUND_GAP: Duration = Duration::from_secs(30);
const FLUSH_TIME: Duration = Duration::from_secs(5);
const TOOLBAR_HEIGHT: i32 = 92;
const IDLE_PAUSE_CHECK_LIMIT: Duration = Duration::from_secs(5);

// Checks the source location before any DOM read, including navigation races.
// The browser clock before the ordinary click fences delayed pre-arm requests.
const COMMUNITY_UI: &str = r#"(() => {
  const expected = __POPOUT__ ? '/popout/__CHANNEL__/chat' : '/__CHANNEL__';
  if (location.origin !== 'https://www.twitch.tv' || location.pathname.toLowerCase() !== expected)
    return {code:'wrong_page', signed_in:false, chat_ready:false, community_ready:false,
      video_present:false, video_ready:false, video_paused:null, play_control:false,
      pause_clicked:false, trigger_unix_ms:null, observed_unix_ms:null};
  const visible = e => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().height > 0;
  const buttons = [...document.querySelectorAll('button,[role="tab"]')].filter(visible);
  const label = e => (e.getAttribute('aria-label') || '').trim();
  const target = buttons.find(e => e.getAttribute('data-test-selector') === 'chat-viewer-list')
    || buttons.find(e => e.getAttribute('data-a-target') === 'chat-viewers')
    || buttons.find(e => label(e) === 'Community')
    || buttons.find(e => (e.textContent || '').trim() === 'Community');
  const player = buttons.find(e => e.getAttribute('data-a-target') === 'player-play-pause-button')
    || buttons.find(e => /^(Pause|Play) \(space\/k\)$/.test(label(e)));
  const signed_in = buttons.some(e => label(e) === 'User Avatar' || e.querySelector('img[alt="User Avatar"]'))
    && !buttons.some(e => e.getAttribute('data-a-target') === 'login-button');
  const chat_ready = !!document.querySelector('[data-a-target="chat-input"],.chat-scrollable-area__message-container');
  let pause_clicked = false;
  const snapshot = () => {
    const videos = [...document.querySelectorAll('video')];
    return {signed_in, chat_ready, community_ready:!!target && !target.disabled,
      video_present:videos.length > 0,
      video_ready:videos.length > 0 && videos.length <= 8 && videos.every(v => v.readyState >= 2 && !v.ended),
      video_paused:videos.length > 0 ? videos.every(v => v.paused) : null,
      play_control:!!player && !player.disabled && /^Play(?: \(space\/k\))?$/.test(label(player)), pause_clicked};
  };
  const result = code => ({code, ...snapshot(), trigger_unix_ms:null,
    observed_unix_ms:performance.timeOrigin + performance.now()});
  if (/access denied|verify you are human|unusual traffic/i.test(document.title)
      || document.querySelector('iframe[src*="captcha" i],iframe[src*="challenge" i],form[action*="challenge" i]'))
    return result('challenge_denied');
  if (__AUTOMATIC__) {
    const before = snapshot();
    if (__ALLOW_PAUSE__ && before.video_present && before.video_paused === false
        && player && !player.disabled && /^Pause(?: \(space\/k\))?$/.test(label(player))) {
      player.click();
      pause_clicked = true;
      return result('observed');
    }
    if (!__CLICK__) return result('observed');
    if (!before.signed_in || !before.chat_ready || !before.community_ready
        || !before.video_present || before.video_paused !== true)
      return result('pause_or_readiness_lost');
  }
  if (!target || target.disabled) return result('waiting_ui');
  if (!__CLICK__) return result('ready');
  const observed = snapshot();
  const trigger_unix_ms = performance.timeOrigin + performance.now();
  const deadline = __TRIGGER_DEADLINE_MS__;
  if (!Number.isFinite(deadline) || !Number.isFinite(trigger_unix_ms)
      || trigger_unix_ms <= 0 || trigger_unix_ms >= deadline)
    return {code:'community_trigger_deadline',...observed,trigger_unix_ms:null,observed_unix_ms:trigger_unix_ms};
  target.click();
  return {code:'native_community_clicked',...observed,trigger_unix_ms,observed_unix_ms:trigger_unix_ms};
})()"#;
thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

struct Engine {
    controller: ICoreWebView2Controller,
    core: ICoreWebView2,
    core2: ICoreWebView2_2,
    environment: ICoreWebView2Environment,
    environment5: ICoreWebView2Environment5,
    protocol_receivers: Vec<(ICoreWebView2DevToolsProtocolEventReceiver, i64)>,
    exit_token: i64,
    new_window_token: i64,
    navigation_token: i64,
    source_token: i64,
    navigation_completed_token: i64,
    document_loaded_token: i64,
    process_failed_token: i64,
}

#[derive(Default)]
struct State {
    selected_channel: Option<String>,
    armed_until: Option<Instant>,
    last_round: Option<Instant>,
    generation: u64,
    pending: bool,
    script_pending: bool,
    script_pending_since: Option<Instant>,
    denied: bool,
    closing_at: Option<Instant>,
    browser: BrowserLifecycle,
    last_player_observation: Option<sampling::PlayerObservation>,
    fault: FaultReport,
    evidence_write_failed: bool,
    tracker: Option<Tracker>,
    network_enabled: bool,
    network_pending: bool,
    navigation_ready: bool,
    navigation_gate: sampling::NavigationGate,
    frame_id: Option<String>,
    schedule: Option<Schedule>,
    auto_stage: Option<AutoStage>,
    player_gate: sampling::PlayerGate,
    pending_sample: Option<(crate::ParsedSample, BodyRequest, i64)>,
    trigger_pending: bool,
    trigger_deadline: Option<sampling::TriggerDeadline>,
    controller_epoch: u64,
    recycling_at: Option<Instant>,
    initializing_at: Option<Instant>,
}

type PendingCompletion = (u64, std::result::Result<SourceSample, SourceFailure>);

struct Service {
    bridge: Bridge,
    profile: PathBuf,
    active: RefCell<Option<AcceptedJob>>,
    pending_command: RefCell<Option<QueuedCommand>>,
    pending_result: RefCell<Option<PendingCompletion>>,
    last_cancel: RefCell<u64>,
    login_visible: RefCell<bool>,
}

impl Service {
    fn accept_login_publication(&self, publication: u64) -> bool {
        let Some(cancel_version) = self.bridge.control.login_cancel_version(publication) else {
            return false;
        };
        *self.last_cancel.borrow_mut() = cancel_version;
        true
    }
}

#[derive(Clone, Copy)]
enum AutoStage {
    Navigating(Instant),
    Preparing(Instant),
    Enabling(Instant),
    Ready(Instant),
    Sampling,
}

#[derive(Deserialize)]
struct UiResult {
    code: String,
    #[serde(flatten)]
    player: sampling::PlayerObservation,
    trigger_unix_ms: Option<f64>,
    observed_unix_ms: Option<f64>,
}

struct App {
    hwnd: HWND,
    input: HWND,
    status: HWND,
    engine: RefCell<Option<Engine>>,
    state: RefCell<State>,
    evidence: RefCell<Evidence>,
    evidence_path: PathBuf,
    started: Instant,
    service: Option<Service>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn unix_milliseconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

impl App {
    fn epoch(&self) -> u64 {
        self.state.borrow().controller_epoch
    }

    fn controller_current(&self, epoch: u64) -> bool {
        let state = self.state.borrow();
        state.controller_epoch == epoch
            && state.closing_at.is_none()
            && state.recycling_at.is_none()
    }

    fn callbacks_allowed(&self) -> bool {
        self.service.as_ref().is_none_or(|service| {
            !service.bridge.control.shutdown.load(Ordering::Acquire)
                && service
                    .bridge
                    .control
                    .cancel_version
                    .load(Ordering::Acquire)
                    == *service.last_cancel.borrow()
        })
    }

    fn callback_current(&self, epoch: u64, generation: u64) -> bool {
        self.controller_current(epoch)
            && self.state.borrow().generation == generation
            && self.callbacks_allowed()
    }

    fn service_failure(&self, code: &'static str, pauses_community: bool) {
        let Some(service) = &self.service else {
            return;
        };
        if self.state.borrow().closing_at.is_some() || self.state.borrow().recycling_at.is_some() {
            return;
        }
        if self.engine.borrow().is_none() {
            let reason = if self.state.borrow().initializing_at.is_some() {
                HostTerminationReason::StartupFailed
            } else {
                HostTerminationReason::SourceFailureWithoutController
            };
            self.observe_termination(reason);
        }
        if code != "operator_cancelled" {
            let mut state = self.state.borrow_mut();
            state.fault = FaultReport {
                primary_failure: Some(PrimaryFailure {
                    code,
                    stage: match state.auto_stage {
                        Some(AutoStage::Navigating(_)) => "navigating",
                        Some(AutoStage::Preparing(_)) => "preparing",
                        Some(AutoStage::Enabling(_)) => "enabling",
                        Some(AutoStage::Ready(_)) => "ready",
                        Some(AutoStage::Sampling) => "sampling",
                        None if state.initializing_at.is_some() => "initializing",
                        None => "idle",
                    },
                    observed_at_unix_ms: unix_milliseconds(),
                    controller_epoch: state.controller_epoch,
                    job_id: service
                        .active
                        .borrow()
                        .as_ref()
                        .map(|job| job.job_id)
                        .or_else(|| service.bridge.control.pending_job().map(|job| job.job_id)),
                    navigation_ready: state.navigation_ready,
                    last_player_observation: state.last_player_observation.clone(),
                    script_pending: state.script_pending,
                    network_pending: state.network_pending,
                    response_pending: state.pending,
                    trigger_pending: state.trigger_pending,
                }),
                host_termination: state.fault.host_termination.clone(),
                window_dismissal: state.fault.window_dismissal.clone(),
                ..Default::default()
            };
            drop(state);
            self.publish_fault();
        }
        let active = service.active.borrow_mut().take();
        let job = active.or_else(|| {
            service.bridge.control.pending_job().filter(|job| {
                code != "operator_cancelled"
                    || job.cancel_version
                        != service
                            .bridge
                            .control
                            .cancel_version
                            .load(Ordering::Acquire)
            })
        });
        if let Some(job) = job {
            *service.pending_result.borrow_mut() = Some((
                job.ticket,
                Err(SourceFailure {
                    code,
                    pauses_community,
                }),
            ));
        }
        self.state.borrow_mut().denied |= pauses_community;
        if pauses_community {
            service.bridge.emit(Status::Paused { code });
        }
        self.cancel_round();
        self.state.borrow_mut().auto_stage = None;
        if self.engine.borrow().is_none() {
            service.bridge.emit(Status::Paused { code });
            self.close();
        } else {
            self.begin_recycle();
        }
    }

    fn begin_recycle(&self) {
        if self.state.borrow().recycling_at.is_some() {
            return;
        }
        self.cancel_round();
        self.disable_network();
        {
            let mut state = self.state.borrow_mut();
            if state.browser.retire() {
                state.controller_epoch += 1;
            }
            if let Some(service) = &self.service {
                service
                    .bridge
                    .control
                    .set_controller_epoch(state.controller_epoch);
            }
            state.recycling_at = Some(Instant::now());
            state.auto_stage = None;
        }
        self.detach_engine();
    }

    fn detach_engine(&self) {
        let Some(engine) = self.engine.borrow_mut().take() else {
            return;
        };
        let record = |operation, result: windows::core::Result<()>| {
            let hresult = result.err().map_or(0, |error| error.code().0);
            self.state
                .borrow_mut()
                .browser
                .detach_result(operation, hresult);
        };
        unsafe {
            for (receiver, token) in &engine.protocol_receivers {
                record(
                    "remove_protocol_event",
                    receiver.remove_DevToolsProtocolEventReceived(*token),
                );
            }
            record(
                "remove_new_window",
                engine
                    .core
                    .remove_NewWindowRequested(engine.new_window_token),
            );
            record(
                "remove_navigation_starting",
                engine
                    .core
                    .remove_NavigationStarting(engine.navigation_token),
            );
            record(
                "remove_source_changed",
                engine.core.remove_SourceChanged(engine.source_token),
            );
            record(
                "remove_navigation_completed",
                engine
                    .core
                    .remove_NavigationCompleted(engine.navigation_completed_token),
            );
            record(
                "remove_document_loaded",
                engine
                    .core2
                    .remove_DOMContentLoaded(engine.document_loaded_token),
            );
            record(
                "remove_process_failed",
                engine
                    .core
                    .remove_ProcessFailed(engine.process_failed_token),
            );
            let result = engine
                .controller
                .Close()
                .err()
                .map_or(0, |error| error.code().0);
            self.state.borrow_mut().browser.diagnostics.close_hresult = Some(result);
        }
        EXIT_ENV.with(|slot| {
            *slot.borrow_mut() = Some((engine.environment, engine.environment5, engine.exit_token))
        });
    }

    fn release_exit_environment(&self) -> Option<i32> {
        EXIT_ENV.with(|slot| {
            slot.borrow_mut()
                .take()
                .map(|(_, environment5, token)| unsafe {
                    environment5
                        .remove_BrowserProcessExited(token)
                        .err()
                        .map_or(0, |error| error.code().0)
                })
        })
    }

    fn complete_cleanup(&self, started: Instant) -> bool {
        let unsubscribe = self.release_exit_environment();
        let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let diagnostics = self.state.borrow_mut().browser.finish(elapsed, unsubscribe);
        let confirmed = diagnostics.confirmed;
        self.state.borrow_mut().fault.cleanup_diagnostics = Some(diagnostics);
        self.publish_fault();
        confirmed
    }

    fn publish_fault(&self) {
        let Some(service) = &self.service else {
            return;
        };
        let report = self.state.borrow().fault.clone();
        // Publish before cancellation/Close without disk I/O or another worker.
        // The reserved terminal slot carries this bounded snapshot through shutdown.
        service.bridge.control.record_fault(report);
    }

    fn observe_termination(&self, reason: HostTerminationReason) {
        if self.state.borrow().fault.host_termination.is_some() {
            return;
        }
        let termination = if let Some(service) = &self.service {
            service.bridge.control.set_controller_epoch(self.epoch());
            service.bridge.control.observe_termination(reason)
        } else {
            HostTermination {
                reason,
                observed_at_unix_ms: unix_milliseconds(),
                controller_epoch: self.epoch(),
                job_id: None,
            }
        };
        self.state.borrow_mut().fault.host_termination = Some(termination);
        self.publish_fault();
    }

    fn window_close(&self) {
        if self.service.is_some() {
            // An auxiliary window dismissal changes presentation only. In
            // particular sign-in remains owner-paused until explicit Resume.
            let mut state = self.state.borrow_mut();
            let dismissal = state.fault.window_dismissal.get_or_insert(WindowDismissal {
                count: 0,
                last_observed_at_unix_ms: 0,
            });
            dismissal.count = dismissal.count.saturating_add(1);
            dismissal.last_observed_at_unix_ms = unix_milliseconds();
            drop(state);
            self.publish_fault();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            }
        } else {
            self.observe_termination(HostTerminationReason::ProofWindowClose);
            self.close();
        }
    }

    fn message_pump_exit(&self, result: i32) {
        self.observe_termination(if result == 0 {
            HostTerminationReason::MessagePumpQuit
        } else {
            HostTerminationReason::MessagePumpError
        });
    }

    fn finish_service(&self, cleanup_confirmed: bool) {
        if !cleanup_confirmed {
            self.observe_termination(HostTerminationReason::CleanupUnconfirmed);
        }
        if let Some(service) = &self.service {
            // Close admission under the same lock used by try_sample before
            // handling queued/native state. Termination completes the ledger's
            // accepted job, including one admitted before any dequeue.
            service.bridge.control.stop_accepting();
            if let Some((ticket, outcome)) = service.pending_result.borrow_mut().take() {
                service.bridge.control.complete(ticket, outcome);
            }
            service
                .bridge
                .control
                .terminate(cleanup_confirmed, "operator_cancelled");
            service.active.borrow_mut().take();
            service.pending_command.borrow_mut().take();
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    fn service_wait_failure(&self, now: Instant) -> Option<&'static str> {
        let service = self.service.as_ref()?;
        let state = self.state.borrow();
        if service.active.borrow().is_some()
            || state.closing_at.is_some()
            || state.recycling_at.is_some()
        {
            return None;
        }
        if state.script_pending
            && state
                .script_pending_since
                .is_some_and(|since| now.saturating_duration_since(since) >= IDLE_PAUSE_CHECK_LIMIT)
        {
            return Some("idle_pause_guard_deadline");
        }
        service
            .bridge
            .control
            .pending_job()
            .filter(|job| job.queue_deadline_expired(now))
            .map(|_| "sample_queue_deadline")
    }

    fn service_tick(self: &Rc<Self>) {
        let Some(service) = &self.service else {
            return;
        };
        if service.bridge.control.shutdown.load(Ordering::Acquire) {
            service.bridge.control.stop_accepting();
            self.observe_termination(HostTerminationReason::ShutdownRequested);
            self.close();
            return;
        }
        let cancel = service
            .bridge
            .control
            .cancel_version
            .load(Ordering::Acquire);
        let changed = *service.last_cancel.borrow() != cancel;
        *service.last_cancel.borrow_mut() = cancel;
        if changed {
            if service.active.borrow().is_some() || self.state.borrow().selected_channel.is_some() {
                self.service_failure("operator_cancelled", false);
            } else {
                self.cancel_round();
            }
        }
        let recycling = self.state.borrow().recycling_at;
        if let Some(started) = recycling {
            let exited = self.state.borrow().browser.diagnostics.normal_exit_observed;
            if exited || started.elapsed() >= FLUSH_TIME {
                let confirmed = self.complete_cleanup(started);
                if !confirmed {
                    service.bridge.emit(Status::Paused {
                        code: "controller_cleanup_unconfirmed",
                    });
                    self.finish_service(false);
                    return;
                }
                {
                    let mut state = self.state.borrow_mut();
                    state.recycling_at = None;
                    state.browser = BrowserLifecycle::default();
                    state.navigation_gate = sampling::NavigationGate::default();
                    state.player_gate = sampling::PlayerGate::default();
                    state.selected_channel = None;
                    state.initializing_at = Some(Instant::now());
                }
                if let Some((ticket, outcome)) = service.pending_result.borrow_mut().take() {
                    service.bridge.control.complete(ticket, outcome);
                }
                if unsafe { initialize(self, service.profile.clone()) }.is_err() {
                    self.fail(
                        "runtime_start_failed",
                        "The existing browser runtime could not restart.",
                    );
                }
            }
            return;
        }
        if let Some(code) = self.service_wait_failure(Instant::now()) {
            self.service_failure(code, false);
            return;
        }
        if self.engine.borrow().is_none() {
            if self
                .state
                .borrow()
                .initializing_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(30))
            {
                self.fail(
                    "runtime_start_deadline",
                    "The existing browser runtime did not initialize within its deadline.",
                );
            }
            return;
        }
        if service.active.borrow().is_none() {
            let command = service.pending_command.borrow_mut().take().or_else(|| {
                match service.bridge.commands.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        self.observe_termination(HostTerminationReason::CommandChannelClosed);
                        self.close();
                        None
                    }
                }
            });
            if let Some(command) = command {
                if self.state.borrow().script_pending
                    || self.state.borrow().pending
                    || self.state.borrow().network_pending
                {
                    *service.pending_command.borrow_mut() = Some(command);
                    return;
                }
                if self.state.borrow().navigation_gate.retained_ids() >= RECYCLE_NAVIGATIONS {
                    *service.pending_command.borrow_mut() = Some(command);
                    self.begin_recycle();
                    return;
                }
                match command {
                    QueuedCommand::ShowLogin { publication } => {
                        // The timer may have read the previous version before
                        // this command was published. Adopt the command's
                        // committed cancellation version before navigation.
                        if !service.accept_login_publication(publication) {
                            return;
                        }
                        self.cancel_round();
                        {
                            let mut state = self.state.borrow_mut();
                            state.denied = false;
                            state.selected_channel = None;
                            state.auto_stage = None;
                            state.player_gate = sampling::PlayerGate::default();
                        }
                        *service.login_visible.borrow_mut() = true;
                        unsafe {
                            let _ = ShowWindow(self.hwnd, SW_RESTORE);
                        }
                        self.navigate("https://www.twitch.tv/login".into());
                        service.bridge.emit(Status::LoginShown);
                    }
                    QueuedCommand::Sample { job } => {
                        if !service.bridge.control.is_pending(job.ticket) {
                            return;
                        }
                        let current = service
                            .bridge
                            .control
                            .cancel_version
                            .load(Ordering::Acquire);
                        if job.cancel_version != current || self.state.borrow().denied {
                            service.bridge.control.complete(
                                job.ticket,
                                Err(SourceFailure {
                                    code: if job.cancel_version != current {
                                        "operator_cancelled"
                                    } else {
                                        "community_paused"
                                    },
                                    pauses_community: self.state.borrow().denied,
                                }),
                            );
                            return;
                        }
                        *service.last_cancel.borrow_mut() = current;
                        let channel = job.channel.clone();
                        self.cancel_round();
                        *service.login_visible.borrow_mut() = false;
                        *service.active.borrow_mut() = Some(job);
                        {
                            let mut state = self.state.borrow_mut();
                            state.selected_channel = Some(channel.clone());
                            state.player_gate = sampling::PlayerGate::default();
                            state.auto_stage = Some(AutoStage::Navigating(
                                Instant::now() + Duration::from_millis(sampling::NAVIGATION_UI_MS),
                            ));
                        }
                        let minimized = unsafe {
                            let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
                            IsIconic(self.hwnd).as_bool()
                        };
                        if !minimized {
                            self.service_failure("native_minimize_unverified", false);
                            return;
                        }
                        self.navigate(format!("https://www.twitch.tv/{channel}"));
                    }
                }
                return;
            }
        }
        let stage = self.state.borrow().auto_stage;
        match stage {
            Some(
                AutoStage::Navigating(deadline)
                | AutoStage::Preparing(deadline)
                | AutoStage::Enabling(deadline)
                | AutoStage::Ready(deadline),
            ) if Instant::now() >= deadline => {
                self.service_failure("navigation_ui_deadline", false)
            }
            Some(AutoStage::Navigating(deadline)) if self.state.borrow().navigation_ready => {
                self.state.borrow_mut().auto_stage = Some(AutoStage::Preparing(deadline));
                self.ui_step(false);
            }
            Some(AutoStage::Preparing(_) | AutoStage::Enabling(_) | AutoStage::Ready(_)) => {
                self.ui_step(false)
            }
            None if self.state.borrow().player_gate.pause_verified()
                && !*service.login_visible.borrow() =>
            {
                self.ui_step(false)
            }
            _ => (),
        }
    }

    fn status(&self, message: &str) {
        let text = wide(message);
        unsafe {
            let _ = SetWindowTextW(self.status, PCWSTR(text.as_ptr()));
        }
    }

    fn save(&self) -> bool {
        if self.service.is_some() {
            return true;
        }
        let temporary = self.evidence_path.with_extension("tmp");
        let result = serde_json::to_vec_pretty(&*self.evidence.borrow())
            .ok()
            .and_then(|bytes| fs::write(&temporary, bytes).ok())
            .and_then(|_| fs::rename(&temporary, &self.evidence_path).ok());
        if result.is_none() {
            self.state.borrow_mut().evidence_write_failed = true;
            self.status(
                "Cannot save aggregate evidence next to this executable. Sampling is disabled.",
            );
            false
        } else {
            true
        }
    }

    fn fail(&self, code: &'static str, message: &str) {
        if self.service.is_some() {
            self.service_failure(code, matches!(code, "signed_in_ui_lost"));
            self.status(message);
            return;
        }
        let timing = self
            .state
            .borrow()
            .tracker
            .as_ref()
            .and_then(|tracker| tracker.timing_failure(code));
        if let Some(timing) = timing {
            let mut evidence = self.evidence.borrow_mut();
            if evidence.native_timing_failure.is_none() {
                evidence.native_timing_failure = Some(timing);
            }
        }
        self.cancel_round();
        self.disable_network();
        if self.automatic() {
            self.end_auto_attempt(code, true);
        }
        {
            let mut evidence = self.evidence.borrow_mut();
            evidence.phase = "host_failed";
            evidence.status = code;
        }
        if self.save() {
            self.status(message);
        }
    }

    fn sample_failure(&self, failure: Failure) {
        if self.service.is_some() {
            self.service_failure(failure.code(), failure.is_denial());
            return;
        }
        self.cancel_round();
        self.disable_network();
        self.state.borrow_mut().denied |= failure.is_denial();
        if self.automatic() {
            self.end_auto_attempt(failure.code(), failure.is_denial());
        }
        self.evidence.borrow_mut().failure(failure);
        if self.save() {
            self.status(if failure.is_denial() {
                "Source denied. Sampling is stopped for this run; no automatic retry. Close the proof."
            } else { "Sample failed. Aggregate evidence contains a safe reason code; no automatic retry." });
        }
    }

    fn current_uri(&self) -> windows::core::Result<String> {
        let core = self
            .engine
            .borrow()
            .as_ref()
            .map(|engine| engine.core.clone())
            .ok_or_else(|| windows::core::Error::from_hresult(E_UNEXPECTED))?;
        unsafe {
            let mut uri = PWSTR::null();
            core.Source(&mut uri)?;
            Ok(take_pwstr(uri))
        }
    }

    fn open_login(self: &Rc<Self>) {
        if self.state.borrow().closing_at.is_some() || self.automatic() {
            return;
        }
        self.cancel_round();
        self.record_action("twitch_login");
        self.navigate("https://www.twitch.tv/login".into());
        self.status("Sign in manually in this app. Then enter a channel and choose Open channel.");
    }

    fn cancel_round(&self) {
        let mut state = self.state.borrow_mut();
        state.generation += 1;
        state.armed_until = None;
        state.pending = false;
        state.script_pending = false;
        state.script_pending_since = None;
        state.network_pending = false;
        state.tracker = None;
        state.pending_sample = None;
        state.trigger_pending = false;
        state.trigger_deadline = None;
    }

    fn record_action(&self, target: &'static str) {
        {
            let mut evidence = self.evidence.borrow_mut();
            evidence.browser_diagnostics.requested_target = target;
            evidence.browser_diagnostics.native_action_count += 1;
        }
        self.save();
    }

    fn check_rendering(&self) {
        if self.state.borrow().closing_at.is_some() || self.automatic() {
            return;
        }
        self.cancel_round();
        self.disable_network();
        self.record_action("local_render_check");
        {
            let mut state = self.state.borrow_mut();
            let intent = state.navigation_gate.begin();
            state.navigation_gate.issue(intent);
            state.navigation_ready = false;
        }
        let core = self
            .engine
            .borrow()
            .as_ref()
            .map(|engine| engine.core.clone());
        if let Some(core) = core {
            let html = w!("<!doctype html><html lang=en><meta charset=utf-8><title>Local rendering check</title><body style='background:white;color:#173c28;font:24px system-ui;padding:50px'><h1>Local browser rendering works</h1><p>This page needs no Internet connection.</p><p>Use Open login to return to Twitch.</p></body></html>");
            if unsafe { core.NavigateToString(html) }.is_err() {
                self.fail(
                    "local_render_navigation_failed",
                    "Could not open the local rendering check.",
                );
            } else {
                self.status("Local rendering check requested. Its text must appear below; then use Open login.");
            }
        }
    }

    fn navigation_changed(&self) {
        let active = {
            let state = self.state.borrow();
            state.closing_at.is_none()
                && (state.armed_until.is_some() || state.pending || state.script_pending)
        };
        self.cancel_round();
        self.disable_network();
        self.state.borrow_mut().navigation_ready = false;
        if !active {
            return;
        }
        if self.automatic() {
            self.end_auto_attempt("navigation_changed", true);
        }
        {
            let mut evidence = self.evidence.borrow_mut();
            evidence.phase = "sample_cancelled";
            evidence.status = "navigation_changed";
        }
        if self.save() {
            self.status("Navigation cancelled the sample. Open the intended channel before another manual attempt.");
        }
    }

    fn open_channel(self: &Rc<Self>) {
        if self.state.borrow().closing_at.is_some() || self.automatic() {
            return;
        }
        let mut buffer = [0u16; 64];
        let length = unsafe { GetWindowTextW(self.input, &mut buffer) };
        let Some(channel) =
            channel_login(&String::from_utf16_lossy(&buffer[..length.max(0) as usize]))
        else {
            self.status("Enter a channel login: 1-25 ASCII letters, digits, or underscores.");
            return;
        };
        // A proof run belongs to one channel, so union/overlap cannot combine channels.
        if self.evidence.borrow().sample_count > 0
            && self.evidence.borrow().channel.as_ref() != Some(&channel)
        {
            self.status("This proof run already has samples for another channel. Restart for a different channel.");
            return;
        }
        self.cancel_round();
        self.record_action("twitch_channel");
        self.state.borrow_mut().selected_channel = Some(channel.clone());
        self.evidence.borrow_mut().channel = Some(channel.clone());
        self.navigate(format!("https://www.twitch.tv/{channel}"));
        if self.save() {
            self.status("Wait for chat to load. Close Community if already open, then choose Sample Community.");
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    fn core(&self) -> Option<ICoreWebView2> {
        self.engine.borrow().as_ref().map(|e| e.core.clone())
    }

    fn automatic(&self) -> bool {
        self.service.is_some() || self.state.borrow().schedule.is_some()
    }

    fn disable_network(&self) {
        {
            let mut state = self.state.borrow_mut();
            state.network_enabled = false;
            state.network_pending = false;
            state.tracker = None;
        }
        if let Some(core) = self.core() {
            let done = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
            unsafe {
                let _ = core.CallDevToolsProtocolMethod(w!("Network.disable"), w!("{}"), &done);
            }
        }
    }

    // Disable finishes before navigation; no protocol observation is active on
    // login or another origin, including queued old callbacks.
    fn navigate(self: &Rc<Self>, url: String) {
        self.disable_network();
        let intent = {
            let mut state = self.state.borrow_mut();
            state.navigation_ready = false;
            state.last_player_observation = None;
            state.navigation_gate.begin()
        };
        let generation = self.state.borrow().generation;
        let epoch = self.epoch();
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _| {
                if let Some(app) = weak.upgrade() {
                    if !app.callback_current(epoch, generation) {
                        return Ok(());
                    }
                    if result.is_err() {
                        app.fail(
                            "network_disable_failed",
                            "Native observation could not stop safely.",
                        );
                        return Ok(());
                    }
                    if let Some(core) = app.core() {
                        if !app.state.borrow_mut().navigation_gate.issue(intent) {
                            return Ok(());
                        }
                        let url = wide(&url);
                        if unsafe { core.Navigate(PCWSTR(url.as_ptr())) }.is_err() {
                            app.fail(
                                "navigation_failed",
                                "Could not open the requested native page.",
                            );
                        }
                    }
                }
                Ok(())
            }));
        if let Some(core) = self.core() {
            if unsafe { core.CallDevToolsProtocolMethod(w!("Network.disable"), w!("{}"), &done) }
                .is_err()
            {
                self.fail(
                    "network_disable_failed",
                    "Native observation could not stop safely.",
                );
            }
        }
    }

    fn enable_network(self: &Rc<Self>, manual_arm: bool) {
        if self.state.borrow().network_pending {
            return;
        }
        let channel = self
            .state
            .borrow()
            .selected_channel
            .clone()
            .unwrap_or_default();
        if !self
            .current_uri()
            .is_ok_and(|uri| is_channel_page(&uri, &channel))
        {
            self.sample_failure(Failure::AuthenticationDenied);
            return;
        }
        self.state.borrow_mut().network_pending = true;
        let generation = self.state.borrow().generation;
        let epoch = self.epoch();
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _| {
                if let Some(app) = weak.upgrade() {
                    if !app.callback_current(epoch, generation) {
                        return Ok(());
                    }
                    if result.is_err() {
                        app.fail(
                            "network_observer_enable_failed",
                            "Native source observation could not start.",
                        );
                        return Ok(());
                    }
                    if !app
                        .current_uri()
                        .is_ok_and(|uri| is_channel_page(&uri, &channel))
                    {
                        app.sample_failure(Failure::AuthenticationDenied);
                        return Ok(());
                    }
                    app.verify_frame(generation, manual_arm);
                }
                Ok(())
            }));
        if let Some(core) = self.core() {
            if unsafe { core.CallDevToolsProtocolMethod(w!("Network.enable"), w!("{\"maxTotalBufferSize\":4194304,\"maxResourceBufferSize\":1048577,\"maxPostDataSize\":65537}"), &done) }.is_err() {
                self.state.borrow_mut().network_pending = false;
                self.fail("network_observer_enable_failed", "Native source observation could not start.");
            }
        }
    }

    fn verify_frame(self: &Rc<Self>, generation: u64, manual_arm: bool) {
        let channel = self
            .state
            .borrow()
            .selected_channel
            .clone()
            .unwrap_or_default();
        let epoch = self.epoch();
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                let Some(app) = weak.upgrade() else {
                    return Ok(());
                };
                if !app.callback_current(epoch, generation) {
                    return Ok(());
                }
                app.state.borrow_mut().network_pending = false;
                if result.is_err() {
                    app.fail(
                        "native_frame_unavailable",
                        "Cannot identify the current source frame.",
                    );
                    return Ok(());
                }
                let frame = match sampling::root_frame(&json, &channel) {
                    Ok(frame) => frame,
                    Err(code) => {
                        app.fail(code, "Cannot identify the current source frame.");
                        return Ok(());
                    }
                };
                if !app
                    .current_uri()
                    .is_ok_and(|uri| is_channel_page(&uri, &channel))
                {
                    app.sample_failure(Failure::AuthenticationDenied);
                    return Ok(());
                }
                {
                    let mut state = app.state.borrow_mut();
                    state.frame_id = Some(frame);
                    state.network_enabled = true;
                }
                if manual_arm {
                    app.ui_step(false);
                } else {
                    let stage = app.state.borrow().auto_stage;
                    if let Some(AutoStage::Enabling(deadline)) = stage {
                        app.state.borrow_mut().auto_stage = Some(AutoStage::Ready(deadline));
                    }
                }
                Ok(())
            }));
        if let Some(core) = self.core() {
            if unsafe { core.CallDevToolsProtocolMethod(w!("Page.getFrameTree"), w!("{}"), &done) }
                .is_err()
            {
                self.fail(
                    "native_frame_unavailable",
                    "Cannot identify the current source frame.",
                );
            }
        }
    }

    fn arm(self: &Rc<Self>) {
        if self.automatic() {
            self.status("The finite autonomous run owns the browser. Close the window to stop.");
            return;
        }
        let channel = {
            let state = self.state.borrow();
            if state.closing_at.is_some() || state.denied || state.evidence_write_failed {
                return;
            }
            if state.pending
                || state.script_pending
                || state.network_pending
                || state.armed_until.is_some()
            {
                return;
            }
            if state
                .last_round
                .is_some_and(|time| time.elapsed() < ROUND_GAP)
            {
                self.status("Wait at least 30 seconds between attempts.");
                return;
            }
            state.selected_channel.clone().unwrap_or_default()
        };
        if channel.is_empty()
            || !self
                .current_uri()
                .is_ok_and(|uri| is_channel_page(&uri, &channel))
        {
            self.status("Open the selected Twitch channel before sampling.");
            return;
        }
        if self.evidence.borrow().manual_attempt_count >= MAX_SAMPLES {
            self.status("Three manual attempts are complete. Close this bounded proof.");
            return;
        }
        self.state.borrow_mut().last_round = Some(Instant::now());
        self.evidence.borrow_mut().manual_attempt_count += 1;
        // Include observer setup in a bounded manual attempt.
        self.state.borrow_mut().armed_until = Some(Instant::now() + ARM_TIME);
        self.enable_network(true);
    }

    fn begin_sample(self: &Rc<Self>, browser_observed_ms: Option<f64>) {
        if self.automatic() && !self.state.borrow().player_gate.ready() {
            self.fail(
                "player_pause_not_verified",
                "The normal player pause must be verified before sampling.",
            );
            return;
        }
        let generation;
        let now = self.elapsed_ms();
        {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            generation = state.generation;
            let budget = state
                .schedule
                .as_ref()
                .map_or(sampling::RESPONSE_MS, |schedule| {
                    (schedule.config.max_seconds * 1000)
                        .saturating_sub(now)
                        .min(sampling::RESPONSE_MS)
                });
            state.trigger_deadline = browser_observed_ms
                .and_then(|clock| sampling::TriggerDeadline::new(generation, now, clock, budget));
            if state.trigger_deadline.is_none() {
                drop(state);
                self.fail(
                    "invalid_trigger_deadline",
                    "A bounded browser-clock trigger deadline could not be established.",
                );
                return;
            }
            state.armed_until = Some(Instant::now() + Duration::from_millis(budget));
            state.last_round = Some(Instant::now());
            if let Some(schedule) = state.schedule.as_mut() {
                schedule.note_trigger_started(now);
            }
            state.tracker = Some(Tracker::new(
                generation,
                state.selected_channel.clone().unwrap_or_default(),
                state.frame_id.clone().unwrap_or_default(),
            ));
            if state.schedule.is_some() || self.service.is_some() {
                state.auto_stage = Some(AutoStage::Sampling);
                state.trigger_pending = true;
            }
        }
        {
            let mut evidence = self.evidence.borrow_mut();
            evidence.phase = "armed";
            evidence.status = "awaiting_native_community";
        }
        if !self.save() {
            self.cancel_round();
            return;
        }
        self.status("Observing one ordinary Community click; waiting up to 25 seconds for its native response.");
        self.ui_step(true);
    }

    fn trigger_dispatch_allowed(&self, generation: u64) -> bool {
        self.is_active(generation)
            && self
                .state
                .borrow()
                .trigger_deadline
                .as_ref()
                .is_some_and(|deadline| deadline.allows_dispatch(generation, self.elapsed_ms()))
    }

    fn expire_trigger(&self) {
        if self.automatic() {
            self.end_auto_attempt("community_trigger_deadline", false);
        } else {
            self.fail(
                "community_trigger_deadline",
                "Trigger not accepted within the deadline. No automatic retry.",
            );
        }
    }

    fn ui_step(self: &Rc<Self>, click: bool) {
        let (channel, generation, automatic, allow_pause, require_readiness, checks_pending_sample) = {
            let state = self.state.borrow();
            if state.script_pending || state.closing_at.is_some() {
                return;
            }
            (
                state.selected_channel.clone().unwrap_or_default(),
                state.generation,
                self.automatic(),
                matches!(state.auto_stage, Some(AutoStage::Preparing(_)))
                    && state.player_gate.may_click_pause(),
                click
                    || state.trigger_pending
                    || matches!(
                        state.auto_stage,
                        Some(AutoStage::Preparing(_) | AutoStage::Ready(_))
                    ),
                state.pending_sample.is_some(),
            )
        };
        if click && !self.trigger_dispatch_allowed(generation) {
            self.expire_trigger();
            return;
        }
        let Ok(uri) = self.current_uri() else {
            self.fail(
                "source_uri_unavailable",
                "Cannot verify the current source page.",
            );
            return;
        };
        if !is_channel_page(&uri, &channel) || automatic && is_popout_page(&uri, &channel) {
            self.sample_failure(Failure::AuthenticationDenied);
            return;
        }
        let popout = is_popout_page(&uri, &channel);
        let deadline_json = self
            .state
            .borrow()
            .trigger_deadline
            .as_ref()
            .map(|deadline| deadline.browser_expires_unix_ms().to_string())
            .unwrap_or("null".into());
        let script = COMMUNITY_UI
            .replace("__CHANNEL__", &channel)
            .replace("__POPOUT__", if popout { "true" } else { "false" })
            .replace("__AUTOMATIC__", if automatic { "true" } else { "false" })
            .replace(
                "__ALLOW_PAUSE__",
                if allow_pause { "true" } else { "false" },
            )
            .replace("__TRIGGER_DEADLINE_MS__", &deadline_json)
            .replace("__CLICK__", if click { "true" } else { "false" });
        self.state.borrow_mut().script_pending = true;
        self.state.borrow_mut().script_pending_since = Some(Instant::now());
        let epoch = self.epoch();
        let weak = Rc::downgrade(self);
        let done = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            let Some(app) = weak.upgrade() else {
                return Ok(());
            };
            if !app.callback_current(epoch, generation) {
                return Ok(());
            }
            app.state.borrow_mut().script_pending = false;
            app.state.borrow_mut().script_pending_since = None;
            if automatic {
                let state = app.state.borrow();
                let expired = state
                    .schedule
                    .as_ref()
                    .is_some_and(|schedule| schedule.expired(app.elapsed_ms()));
                let deadline = match state.auto_stage {
                    Some(
                        AutoStage::Navigating(deadline)
                        | AutoStage::Preparing(deadline)
                        | AutoStage::Enabling(deadline)
                        | AutoStage::Ready(deadline),
                    ) => Some(deadline),
                    _ => None,
                };
                drop(state);
                if expired || deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    app.end_auto_attempt(
                        if expired {
                            "run_deadline"
                        } else {
                            "navigation_ui_deadline"
                        },
                        true,
                    );
                    return Ok(());
                }
            }
            if app.state.borrow().trigger_pending && !app.trigger_dispatch_allowed(generation) {
                app.expire_trigger();
                return Ok(());
            }
            if result.is_err() || json.len() > 1024 {
                app.fail(
                    "community_ui_action_failed",
                    "Community UI observation failed.",
                );
                return Ok(());
            }
            let Ok(ui) = serde_json::from_str::<UiResult>(&json) else {
                app.fail(
                    "community_ui_result_invalid",
                    "Community UI returned no valid structural result.",
                );
                return Ok(());
            };
            app.state.borrow_mut().last_player_observation = Some(ui.player.clone());
            if let Some(run) = app.evidence.borrow_mut().autonomous.as_mut() {
                if let Some(attempt) = run.attempts.last_mut() {
                    attempt.video_element_present = Some(ui.player.video_present);
                    attempt.video_paused = ui.player.video_paused;
                    attempt.player_pause_last_checked_ms = Some(app.elapsed_ms());
                    attempt.normal_pause_clicks += usize::from(ui.player.pause_clicked);
                    attempt.signed_in_ui_verified |= ui.player.signed_in;
                    attempt.last_player_observation = Some(ui.player.clone());
                }
            }
            let player_ready = if automatic
                && !matches!(ui.code.as_str(), "wrong_page" | "challenge_denied")
            {
                let outcome = app.state.borrow_mut().player_gate.observe(
                    app.elapsed_ms(),
                    &ui.player,
                    require_readiness,
                );
                match outcome {
                    Ok(ready) => {
                        if let Some(run) = app.evidence.borrow_mut().autonomous.as_mut() {
                            if let Some(attempt) = run.attempts.last_mut() {
                                attempt.player_pause_verified |=
                                    app.state.borrow().player_gate.pause_verified();
                            }
                        }
                        ready
                    }
                    Err(code) => {
                        app.fail(code, "The verified normal player pause or signed-in state was lost. The proof is stopped.");
                        return Ok(());
                    }
                }
            } else {
                false
            };
            match ui.code.as_str() {
                "challenge_denied" => app.sample_failure(Failure::ChallengeDenied),
                "wrong_page" => app.sample_failure(Failure::AuthenticationDenied),
                // The script made no Community click. Actual playback loss has
                // already failed the gate; missing controls wait within the
                // original response deadline without rearming or a source retry.
                "pause_or_readiness_lost" if automatic && click => (),
                "observed" if automatic && !click => {
                    let (stage, trigger_pending) = {
                        let state = app.state.borrow();
                        (state.auto_stage, state.trigger_pending)
                    };
                    if player_ready {
                        match stage {
                            Some(AutoStage::Preparing(deadline)) => {
                                app.state.borrow_mut().auto_stage =
                                    Some(AutoStage::Enabling(deadline));
                                app.enable_network(false);
                            }
                            Some(AutoStage::Ready(_)) if require_readiness => {
                                app.begin_sample(ui.observed_unix_ms)
                            }
                            Some(AutoStage::Sampling) if trigger_pending => app.ui_step(true),
                            Some(AutoStage::Sampling) if checks_pending_sample => {
                                app.finish_pending_sample()
                            }
                            _ => (),
                        }
                    }
                }
                "waiting_ui" if !click => (),
                "waiting_ui" => app.fail(
                    "community_control_unavailable",
                    "The ordinary Community control is unavailable. No repeated click.",
                ),
                "ready" if !click && !automatic => {
                    let preparing = app.state.borrow().tracker.is_none();
                    if preparing {
                        if app.is_active(generation) {
                            app.begin_sample(ui.observed_unix_ms);
                        } else {
                            app.expire_trigger();
                        }
                    }
                }
                "community_trigger_deadline" if click => app.expire_trigger(),
                "native_community_clicked" if click => {
                    if !app.trigger_dispatch_allowed(generation)
                        || !ui.trigger_unix_ms.is_some_and(|clock| {
                            app.state
                                .borrow()
                                .trigger_deadline
                                .as_ref()
                                .is_some_and(|deadline| deadline.allows_browser_click(clock))
                        })
                    {
                        app.expire_trigger();
                        return Ok(());
                    }
                    if automatic && !player_ready {
                        app.fail(
                            "player_pause_not_verified",
                            "Player pause was not verified at the Community trigger.",
                        );
                        return Ok(());
                    }
                    if let Some(run) = app.evidence.borrow_mut().autonomous.as_mut() {
                        if let Some(attempt) = run.attempts.last_mut() {
                            attempt.pause_verified_before_trigger = player_ready;
                        }
                    }
                    let outcome = app
                        .state
                        .borrow_mut()
                        .tracker
                        .as_mut()
                        .ok_or("missing_request_tracker")
                        .and_then(|tracker| {
                            tracker.set_trigger(ui.trigger_unix_ms.ok_or("missing_trigger_time")?)
                        });
                    match outcome {
                        Ok(()) => {
                            app.state.borrow_mut().trigger_pending = false;
                            app.try_body();
                        }
                        Err(code) => {
                            app.fail(code, "Native trigger identity could not be verified.")
                        }
                    }
                }
                _ => app.fail(
                    "community_ui_result_invalid",
                    "Community UI returned an unexpected structural result.",
                ),
            }
            app.save();
            if automatic && !checks_pending_sample && app.state.borrow().pending_sample.is_some() {
                // A body received during an older UI call needs a new pause check.
                app.ui_step(false);
            }
            Ok(())
        }));
        if let Some(core) = self.core() {
            let script = wide(&script);
            // Recheck immediately before dispatch, including delayed callbacks.
            if click && !self.trigger_dispatch_allowed(generation) {
                self.state.borrow_mut().script_pending = false;
                self.state.borrow_mut().script_pending_since = None;
                self.expire_trigger();
                return;
            }
            if unsafe { core.ExecuteScript(PCWSTR(script.as_ptr()), &done) }.is_err() {
                self.state.borrow_mut().script_pending = false;
                self.state.borrow_mut().script_pending_since = None;
                self.fail(
                    "community_ui_action_failed",
                    "Community UI observation failed.",
                );
            }
        }
    }

    fn is_active(&self, generation: u64) -> bool {
        let state = self.state.borrow();
        state.generation == generation
            && self.callbacks_allowed()
            && state.closing_at.is_none()
            && !state.denied
            && !state.evidence_write_failed
            && !state
                .schedule
                .as_ref()
                .is_some_and(|s| s.expired(self.elapsed_ms()))
            && state.armed_until.is_some_and(|time| Instant::now() < time)
    }

    fn protocol_event(
        self: &Rc<Self>,
        kind: &'static str,
        args: ICoreWebView2DevToolsProtocolEventReceivedEventArgs,
    ) -> windows::core::Result<()> {
        let (generation, channel) = {
            let state = self.state.borrow();
            if !state.network_enabled
                || state.tracker.is_none()
                || state.closing_at.is_some()
                || state.denied
                || state.evidence_write_failed
                || !state.armed_until.is_some_and(|time| Instant::now() < time)
            {
                return Ok(());
            }
            (
                state.generation,
                state.selected_channel.clone().unwrap_or_default(),
            )
        };
        if !self
            .current_uri()
            .is_ok_and(|uri| is_channel_page(&uri, &channel))
        {
            self.sample_failure(Failure::AuthenticationDenied);
            return Ok(());
        }
        let mut value = PWSTR::null();
        unsafe {
            args.ParameterObjectAsJson(&mut value)?;
        }
        let json = match unsafe { bounded_pwstr(value, sampling::EVENT_CAP) } {
            Ok(v) => v,
            Err(code) => {
                self.fail(code, "Native event exceeded its finite input bound.");
                return Ok(());
            }
        };
        let result = (|| {
            let mut state = self.state.borrow_mut();
            let tracker = state.tracker.as_mut().ok_or("missing_request_tracker")?;
            tracker.count_event()?;
            match kind {
                "Network.requestWillBeSent" => tracker.request(generation, sampling::event(&json)?),
                "Network.responseReceived" => tracker.response(generation, sampling::event(&json)?),
                "Network.loadingFinished" => tracker.finished(generation, sampling::event(&json)?),
                "Network.requestServedFromCache" => {
                    let e: sampling::IdEvent = sampling::event(&json)?;
                    tracker.cached(&e.request_id);
                    Ok(())
                }
                "Network.loadingFailed" => {
                    let e: sampling::IdEvent = sampling::event(&json)?;
                    tracker.failed(&e.request_id);
                    Ok(())
                }
                _ => Err("unsupported_native_event"),
            }
        })();
        drop(json);
        if let Err(code) = result {
            self.fail(
                code,
                "Native request correlation failed. No automatic retry.",
            );
        } else {
            self.try_body();
        }
        Ok(())
    }

    fn try_body(self: &Rc<Self>) {
        let ready = {
            let mut state = self.state.borrow_mut();
            if state.pending {
                return;
            }
            state.tracker.as_mut().map(Tracker::take_ready)
        };
        let body = match ready {
            Some(Ok(Some(body))) => body,
            Some(Err("http_denied")) => {
                self.sample_failure(Failure::HttpDenied);
                return;
            }
            Some(Err(code)) => {
                self.fail(code, "Native source response could not be accepted.");
                return;
            }
            _ => return,
        };
        let generation = self.state.borrow().generation;
        if !self.is_active(generation) {
            return;
        }
        self.state.borrow_mut().pending = true;
        let channel = self
            .state
            .borrow()
            .selected_channel
            .clone()
            .unwrap_or_default();
        let parameters = wide(&serde_json::json!({"requestId":body.id}).to_string());
        let epoch = self.epoch();
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                let Some(app) = weak.upgrade() else {
                    return Ok(());
                };
                if !app.callback_current(epoch, generation) || !app.is_active(generation) {
                    return Ok(());
                }
                app.state.borrow_mut().pending = false;
                if !app
                    .current_uri()
                    .is_ok_and(|uri| is_channel_page(&uri, &channel))
                {
                    app.sample_failure(Failure::AuthenticationDenied);
                    return Ok(());
                }
                if result.is_err() {
                    app.fail(
                        "native_body_unavailable",
                        "The matching native response body is unavailable.",
                    );
                    return Ok(());
                }
                let verified = {
                    let state = app.state.borrow();
                    state
                        .tracker
                        .as_ref()
                        .ok_or("missing_request_tracker")
                        .and_then(|tracker| tracker.complete_body(generation, &body.id))
                };
                let verified = match verified {
                    Ok(verified) => verified,
                    Err("http_denied") => {
                        app.sample_failure(Failure::HttpDenied);
                        return Ok(());
                    }
                    Err(code) => {
                        app.fail(
                            code,
                            "Native response identity changed before body acceptance.",
                        );
                        return Ok(());
                    }
                };
                let bytes = match sampling::response_body(&json) {
                    Ok(bytes) => bytes,
                    Err(code) => {
                        app.fail(
                            code,
                            "Native response exceeded its bound or used unsupported encoding.",
                        );
                        return Ok(());
                    }
                };
                let received_at_unix_ms = unix_milliseconds();
                let parsed = parse_response(&bytes, &verified.selection, verified.status);
                drop(bytes);
                match parsed {
                    Ok(parsed) if app.automatic() => {
                        app.state.borrow_mut().pending_sample =
                            Some((parsed, verified, received_at_unix_ms));
                        app.ui_step(false);
                    }
                    Ok(parsed) => app.accept_sample(parsed, &verified, received_at_unix_ms),
                    Err(failure) => app.sample_failure(failure),
                }
                Ok(())
            }));
        if let Some(core) = self.core() {
            if unsafe {
                core.CallDevToolsProtocolMethod(
                    w!("Network.getResponseBody"),
                    PCWSTR(parameters.as_ptr()),
                    &done,
                )
            }
            .is_err()
            {
                self.state.borrow_mut().pending = false;
                self.fail(
                    "native_body_unavailable",
                    "The matching native response body is unavailable.",
                );
            }
        }
    }

    fn finish_pending_sample(&self) {
        let generation = self.state.borrow().generation;
        if !self.is_active(generation) || !self.state.borrow().player_gate.ready() {
            return;
        }
        let pending = self.state.borrow_mut().pending_sample.take();
        let Some((parsed, body, received_at_unix_ms)) = pending else {
            return;
        };
        let verified = self
            .state
            .borrow()
            .tracker
            .as_ref()
            .ok_or("missing_request_tracker")
            .and_then(|tracker| tracker.complete_body(generation, &body.id));
        match verified {
            Ok(body) => self.accept_sample(parsed, &body, received_at_unix_ms),
            Err("http_denied") => self.sample_failure(Failure::HttpDenied),
            Err(code) => self.fail(
                code,
                "Native source identity changed during the final pause check.",
            ),
        }
    }

    fn accept_sample(
        &self,
        parsed: crate::ParsedSample,
        body: &BodyRequest,
        received_at_unix_ms: i64,
    ) {
        if let Some(service) = &self.service {
            let active = service.active.borrow_mut().take();
            self.cancel_round();
            self.disable_network();
            self.state.borrow_mut().auto_stage = None;
            if let Some(job) = active {
                service.bridge.control.complete(
                    job.ticket,
                    Ok(SourceSample::from_parsed(parsed, body, received_at_unix_ms)),
                );
            }
            return;
        }
        if self.automatic() {
            if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
                run.sample(parsed, body);
            }
            self.evidence.borrow_mut().source_proven_in_this_run = true;
            self.end_auto_attempt("sample_received", false);
            self.minimize_after_first_sample();
        } else {
            self.cancel_round();
            self.disable_network();
            let mut evidence = self.evidence.borrow_mut();
            evidence.add(
                parsed,
                &body.selection,
                (received_at_unix_ms.max(0) / 1000) as u64,
            );
            if let Some(sample) = evidence.samples.last_mut() {
                sample.provenance = Some(body.provenance.clone());
            }
            drop(evidence);
            self.save();
            self.status("Native sample saved. Fresh server state remains unproved; wait 30 seconds for another manual attempt.");
        }
    }

    fn minimize_after_first_sample(&self) {
        if self.state.borrow().evidence_write_failed {
            return;
        }
        let first_persisted = self
            .evidence
            .borrow()
            .autonomous
            .as_ref()
            .is_some_and(|run| {
                run.source_samples_received == 1 && run.minimize_milestone_ms.is_none()
            });
        if !first_persisted {
            return;
        }
        let milestone = self.elapsed_ms();
        // end_auto_attempt saved the accepted sample. No RefCell guard crosses
        // ShowWindow, which can synchronously deliver native window messages.
        let minimized = unsafe {
            let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            IsIconic(self.hwnd).as_bool()
        };
        if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
            run.minimize_milestone_ms = Some(milestone);
            run.minimized_verified = Some(minimized);
        }
        self.save();
        if !minimized {
            self.fail(
                "native_minimize_unverified",
                "Native minimization was not verified. The finite proof is stopped.",
            );
        }
    }

    fn end_auto_attempt(&self, code: &'static str, stop: bool) {
        if self.service.is_some() {
            self.service_failure(code, false);
            return;
        }
        let now = self.elapsed_ms();
        self.cancel_round();
        self.disable_network();
        {
            let mut state = self.state.borrow_mut();
            state.auto_stage = None;
            if let Some(schedule) = state.schedule.as_mut() {
                if code == "sample_received" {
                    schedule.finish();
                } else {
                    schedule.finish_failed();
                }
                schedule.stopped |= stop;
            }
        }
        if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
            run.finish(now, code);
            if stop {
                run.status = code;
            }
        }
        self.save();
        self.status(if stop {
            "Autonomous proof stopped; no retry after denial."
        } else {
            "Attempt recorded. Waiting for the next channel's scheduled start."
        });
    }

    fn auto_tick(self: &Rc<Self>) {
        let now = self.elapsed_ms();
        let (stop, expired, complete) = {
            let state = self.state.borrow();
            let Some(s) = state.schedule.as_ref() else {
                return;
            };
            (s.stopped, s.expired(now), s.complete())
        };
        if self.state.borrow().evidence_write_failed {
            self.end_auto_attempt("aggregate_write_failed", true);
            self.close();
            return;
        }
        if stop || expired || complete {
            if !stop
                && expired
                && self
                    .state
                    .borrow()
                    .schedule
                    .as_ref()
                    .is_some_and(|s| s.active.is_some())
            {
                self.end_auto_attempt("run_deadline", true);
            }
            if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
                if !stop && expired {
                    run.finalize(now, "run_deadline");
                } else if !stop && complete {
                    let reason = if run.source_samples_received == run.attempts.len() {
                        "circuits_completed_with_samples"
                    } else {
                        "circuits_completed_with_gaps"
                    };
                    run.finalize(now, reason);
                }
            }
            self.close();
            return;
        }
        if self.engine.borrow().is_none() {
            return;
        }
        let attempt = self
            .state
            .borrow_mut()
            .schedule
            .as_mut()
            .and_then(|s| s.start_due(now));
        if let Some(attempt) = attempt {
            self.cancel_round();
            {
                let mut state = self.state.borrow_mut();
                state.selected_channel = Some(attempt.channel.clone());
                state.player_gate = sampling::PlayerGate::default();
                state.auto_stage = Some(AutoStage::Navigating(
                    Instant::now() + Duration::from_millis(sampling::NAVIGATION_UI_MS),
                ));
            }
            if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
                run.start(attempt.clone());
                run.status = "running";
            }
            self.record_action("twitch_full_channel_paused");
            self.navigate(format!("https://www.twitch.tv/{}", attempt.channel));
            return;
        }
        let stage = self.state.borrow().auto_stage;
        match stage {
            Some(
                AutoStage::Navigating(deadline)
                | AutoStage::Preparing(deadline)
                | AutoStage::Enabling(deadline)
                | AutoStage::Ready(deadline),
            ) if Instant::now() >= deadline => {
                // A page that never proves pause must not play through the gap.
                self.end_auto_attempt("navigation_ui_deadline", true)
            }
            Some(AutoStage::Navigating(deadline)) => {
                if self.state.borrow().navigation_ready {
                    self.state.borrow_mut().auto_stage = Some(AutoStage::Preparing(deadline));
                    self.ui_step(false);
                }
            }
            Some(AutoStage::Preparing(_) | AutoStage::Enabling(_) | AutoStage::Ready(_)) => {
                self.ui_step(false)
            }
            None if self.state.borrow().player_gate.pause_verified() => self.ui_step(false),
            _ => (),
        }
    }
    fn resize(&self) {
        let engine = self.engine.borrow();
        if let Some(engine) = engine.as_ref() {
            unsafe {
                let mut bounds = RECT::default();
                if GetClientRect(self.hwnd, &mut bounds).is_ok() {
                    if self.service.is_some() {
                        bounds.right = 1280;
                        bounds.bottom = 900;
                    }
                    bounds.top = TOOLBAR_HEIGHT;
                    bounds.bottom = bounds.bottom.max(bounds.top);
                    let _ = engine.controller.SetBounds(bounds);
                    let _ = MoveWindow(self.status, 12, 48, (bounds.right - 24).max(1), 40, true);
                }
            }
        }
    }

    fn close(&self) {
        if let Some(service) = &self.service {
            service.bridge.control.stop_accepting();
        }
        if self.state.borrow().closing_at.is_some() {
            return;
        }
        if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
            run.finalize(self.elapsed_ms(), "operator_cancelled");
        }
        self.cancel_round();
        {
            let mut state = self.state.borrow_mut();
            state.closing_at = Some(state.recycling_at.take().unwrap_or_else(Instant::now));
        }
        self.evidence.borrow_mut().phase = "closing";
        self.evidence.borrow_mut().browser_cleanup = "waiting_for_browser_exit";
        self.save();
        self.status(
            "Closing the embedded browser and flushing aggregate evidence (up to five seconds)...",
        );
        {
            let mut state = self.state.borrow_mut();
            if state.browser.retire() {
                state.controller_epoch += 1;
            }
            if let Some(service) = &self.service {
                service
                    .bridge
                    .control
                    .set_controller_epoch(state.controller_epoch);
            }
        }
        self.detach_engine();
    }

    fn tick(self: &Rc<Self>) {
        let closing = self.state.borrow().closing_at;
        if let Some(started) = closing {
            let exited = self.state.borrow().browser.diagnostics.normal_exit_observed;
            if exited || started.elapsed() >= FLUSH_TIME {
                {
                    let mut evidence = self.evidence.borrow_mut();
                    evidence.phase = "closed";
                    evidence.browser_cleanup = if exited {
                        "browser_process_exited"
                    } else {
                        "exit_not_confirmed_within_5_seconds"
                    };
                }
                self.save();
                let confirmed = self.complete_cleanup(started);
                if self.service.is_some() {
                    self.finish_service(confirmed);
                } else {
                    unsafe {
                        let _ = DestroyWindow(self.hwnd);
                    }
                }
            }
            return;
        }
        if self
            .state
            .borrow()
            .armed_until
            .is_some_and(|time| Instant::now() >= time)
        {
            if self.automatic() {
                let reason = {
                    let state = self.state.borrow();
                    if state.trigger_pending {
                        "community_trigger_deadline"
                    } else if state.pending_sample.is_some() {
                        "player_reverification_deadline"
                    } else {
                        "native_sample_timeout"
                    }
                };
                self.end_auto_attempt(reason, false);
            } else {
                self.fail(
                    "native_sample_timeout",
                    "No matching native Community response within 25 seconds. No automatic retry.",
                );
            }
        }
        if self.service.is_some() {
            self.service_tick();
        } else if self.automatic() {
            self.auto_tick();
        }
        if self.state.borrow().tracker.is_some() && self.is_active(self.state.borrow().generation) {
            self.ui_step(false);
        }
    }
}

thread_local! {
    static EXIT_ENV: RefCell<Option<(ICoreWebView2Environment, ICoreWebView2Environment5, i64)>> = const { RefCell::new(None) };
}

/// Inspect at most the configured UTF-16 length before making an owned copy.
/// Free the COM allocation on every path; do not retain raw event JSON.
unsafe fn bounded_pwstr(value: PWSTR, cap: usize) -> std::result::Result<String, &'static str> {
    if value.is_null() {
        return Err("native_event_unavailable");
    }
    let mut length = 0;
    while length <= cap && *value.0.add(length) != 0 {
        length += 1;
    }
    let result = if length > cap {
        Err("native_event_size_limit")
    } else {
        let result = String::from_utf16_lossy(std::slice::from_raw_parts(value.0, length));
        if result.len() > cap {
            Err("native_event_size_limit")
        } else {
            Ok(result)
        }
    };
    CoTaskMemFree(Some(value.0.cast()));
    result
}
unsafe fn initialize(app: &Rc<App>, profile: PathBuf) -> windows::core::Result<()> {
    let epoch = app.epoch();
    let weak = Rc::downgrade(app);
    let environment_handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |result, environment| {
            let Some(app) = weak.upgrade() else {
                return Ok(());
            };
            if !app.controller_current(epoch) {
                return Ok(());
            }
            let Some(environment) = environment.filter(|_| result.is_ok()) else {
                app.fail(
                    "environment_creation_failed",
                    "The existing WebView2 runtime could not start. No software was installed.",
                );
                return Ok(());
            };
            let weak = Rc::downgrade(&app);
            let environment_for_controller = environment.clone();
            let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                move |result, controller| {
                    let Some(app) = weak.upgrade() else {
                        return Ok(());
                    };
                    if !app.controller_current(epoch) {
                        if let Some(controller) = controller {
                            let _ = controller.Close();
                        }
                        return Ok(());
                    }
                    let Some(controller) = controller.filter(|_| result.is_ok()) else {
                        app.fail(
                            "controller_creation_failed",
                            "The embedded browser window could not be created.",
                        );
                        return Ok(());
                    };
                    match attach(&app, environment_for_controller, controller) {
                Ok(()) => (),
                Err(_) => app.fail("observer_setup_failed", "Native browser observation could not be initialized. Sampling is unavailable."),
            }
                    Ok(())
                },
            ));
            if environment
                .CreateCoreWebView2Controller(app.hwnd, &handler)
                .is_err()
            {
                app.fail(
                    "controller_creation_failed",
                    "The embedded browser window could not be created.",
                );
            }
            Ok(())
        },
    ));
    let profile = wide(&profile.to_string_lossy());
    CreateCoreWebView2EnvironmentWithOptions(
        PCWSTR::null(),
        PCWSTR(profile.as_ptr()),
        None,
        &environment_handler,
    )
}

unsafe fn attach(
    app: &Rc<App>,
    environment: ICoreWebView2Environment,
    controller: ICoreWebView2Controller,
) -> windows::core::Result<()> {
    let epoch = app.epoch();
    let mut close_on_failure = ControllerGuard(Some(controller.clone()));
    let core = controller.CoreWebView2()?;
    let core2: ICoreWebView2_2 = core.cast()?;
    let environment5: ICoreWebView2Environment5 = environment.cast()?;
    let mut browser_pid = 0;
    core.BrowserProcessId(&mut browser_pid)?;
    app.state.borrow_mut().browser.bind(epoch, browser_pid);
    if let Some(service) = &app.service {
        service.bridge.control.set_controller_epoch(epoch);
    }
    let mut protocol_receivers = Vec::new();
    for kind in [
        "Network.requestWillBeSent",
        "Network.responseReceived",
        "Network.loadingFinished",
        "Network.requestServedFromCache",
        "Network.loadingFailed",
    ] {
        let name = wide(kind);
        let receiver = core.GetDevToolsProtocolEventReceiver(PCWSTR(name.as_ptr()))?;
        let weak = Rc::downgrade(app);
        let handler =
            DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                if let (Some(app), Some(args)) = (weak.upgrade(), args) {
                    if !app.controller_current(epoch) || !app.callbacks_allowed() {
                        return Ok(());
                    }
                    if app.protocol_event(kind, args).is_err() {
                        app.fail(
                            "native_observer_failed",
                            "Native source observation failed. No retry.",
                        );
                    }
                }
                Ok(())
            }));
        let mut token = 0;
        receiver.add_DevToolsProtocolEventReceived(&handler, &mut token)?;
        protocol_receivers.push((receiver, token));
    }
    let weak = Rc::downgrade(app);
    let exit_handler = BrowserProcessExitedEventHandler::create(Box::new(move |_, args| {
        if let (Some(app), Some(args)) = (weak.upgrade(), args) {
            let mut pid = 0;
            let mut kind = COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND(0);
            args.BrowserProcessId(&mut pid)?;
            args.BrowserProcessExitKind(&mut kind)?;
            app.state.borrow_mut().browser.observe_exit(
                epoch,
                pid,
                kind.0,
                kind == COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_NORMAL,
            );
        }
        Ok(())
    }));
    let mut exit_token = 0;
    environment5.add_BrowserProcessExited(&exit_handler, &mut exit_token)?;
    // Never open the Edge browser or another host from popups.
    let popup_handler = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            args.SetHandled(true)?;
        }
        Ok(())
    }));
    let mut new_window_token = 0;
    core.add_NewWindowRequested(&popup_handler, &mut new_window_token)?;
    let weak = Rc::downgrade(app);
    let navigation_handler = NavigationStartingEventHandler::create(Box::new(move |_, args| {
        if let Some(app) = weak.upgrade() {
            if !app.controller_current(epoch) || !app.callbacks_allowed() {
                return Ok(());
            }
            let Some(args) = args else {
                return Ok(());
            };
            let mut id = 0;
            args.NavigationId(&mut id)?;
            if !app.state.borrow_mut().navigation_gate.started(id) {
                return Ok(());
            }
            app.navigation_changed();
            {
                let mut evidence = app.evidence.borrow_mut();
                evidence.browser_diagnostics.navigation_started_count += 1;
                evidence.browser_diagnostics.last_navigation_id = Some(id);
            }
            app.save();
        }
        Ok(())
    }));
    let mut navigation_token = 0;
    core.add_NavigationStarting(&navigation_handler, &mut navigation_token)?;
    let weak = Rc::downgrade(app);
    let source_handler = SourceChangedEventHandler::create(Box::new(move |_, _| {
        if let Some(app) = weak.upgrade() {
            if !app.controller_current(epoch) || !app.callbacks_allowed() {
                return Ok(());
            }
            if app.state.borrow().navigation_gate.has_started() {
                app.navigation_changed();
            }
        }
        Ok(())
    }));
    let mut source_token = 0;
    core.add_SourceChanged(&source_handler, &mut source_token)?;
    let weak = Rc::downgrade(app);
    let navigation_completed_handler = NavigationCompletedEventHandler::create(Box::new(
        move |_, args| {
            if let (Some(app), Some(args)) = (weak.upgrade(), args) {
                if !app.controller_current(epoch) || !app.callbacks_allowed() {
                    return Ok(());
                }
                if app.state.borrow().closing_at.is_some() {
                    return Ok(());
                }
                let mut success = BOOL(0);
                let mut error = COREWEBVIEW2_WEB_ERROR_STATUS(0);
                let mut id = 0;
                args.IsSuccess(&mut success)?;
                args.WebErrorStatus(&mut error)?;
                args.NavigationId(&mut id)?;
                let current = app.state.borrow().navigation_gate.completed(id);
                if current {
                    app.state.borrow_mut().navigation_ready = success.as_bool();
                }
                {
                    let mut evidence = app.evidence.borrow_mut();
                    evidence.browser_diagnostics.navigation_completed_count += 1;
                    if current {
                        evidence.browser_diagnostics.last_navigation_id = Some(id);
                        evidence.browser_diagnostics.last_navigation_success =
                            Some(success.as_bool());
                        evidence.browser_diagnostics.last_web_error_status = if success.as_bool() {
                            None
                        } else {
                            Some(error.0)
                        };
                    }
                }
                if current && app.save() {
                    if success.as_bool() {
                        app.status("Native page loaded. Login and Community access still require their own evidence.");
                    } else if app.automatic() {
                        app.end_auto_attempt("navigation_failed", true);
                    } else {
                        app.status(&format!("Browser navigation failed (WebView2 error {}). No login or Community sample is verified.", error.0));
                    }
                }
            }
            Ok(())
        },
    ));
    let mut navigation_completed_token = 0;
    core.add_NavigationCompleted(
        &navigation_completed_handler,
        &mut navigation_completed_token,
    )?;
    let weak = Rc::downgrade(app);
    let document_loaded_handler = DOMContentLoadedEventHandler::create(Box::new(move |_, _| {
        if let Some(app) = weak.upgrade() {
            if !app.controller_current(epoch) || !app.callbacks_allowed() {
                return Ok(());
            }
            if app.state.borrow().closing_at.is_none() {
                app.evidence
                    .borrow_mut()
                    .browser_diagnostics
                    .document_loaded_count += 1;
                app.save();
            }
        }
        Ok(())
    }));
    let mut document_loaded_token = 0;
    core2.add_DOMContentLoaded(&document_loaded_handler, &mut document_loaded_token)?;
    let weak = Rc::downgrade(app);
    let process_failed_handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
        if let (Some(app), Some(args)) = (weak.upgrade(), args) {
            if !app.controller_current(epoch) || !app.callbacks_allowed() {
                return Ok(());
            }
            if app.state.borrow().closing_at.is_some() {
                return Ok(());
            }
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND(0);
            args.ProcessFailedKind(&mut kind)?;
            app.evidence
                .borrow_mut()
                .browser_diagnostics
                .process_failure_kind = Some(kind.0);
            app.fail(
                "browser_process_failed",
                "An embedded browser process failed. No automatic retry.",
            );
        }
        Ok(())
    }));
    let mut process_failed_token = 0;
    core.add_ProcessFailed(&process_failed_handler, &mut process_failed_token)?;
    controller.SetIsVisible(true)?;
    *app.engine.borrow_mut() = Some(Engine {
        controller,
        core,
        core2,
        environment,
        environment5,
        protocol_receivers,
        exit_token,
        new_window_token,
        navigation_token,
        source_token,
        navigation_completed_token,
        document_loaded_token,
        process_failed_token,
    });
    close_on_failure.0 = None;
    app.resize();
    app.evidence.borrow_mut().phase = "ready";
    app.evidence.borrow_mut().status = if app.automatic() {
        "autonomous_ready"
    } else {
        "manual_login_required"
    };
    app.evidence.borrow_mut().browser_cleanup = "browser_running";
    app.save();
    app.state.borrow_mut().initializing_at = None;
    if let Some(service) = &app.service {
        service.bridge.emit(Status::Ready);
    } else if !app.automatic() {
        app.open_login();
    }
    Ok(())
}

struct ControllerGuard(Option<ICoreWebView2Controller>);

impl Drop for ControllerGuard {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            unsafe {
                let _ = controller.Close();
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app = APP.with(|slot| slot.borrow().clone());
    match message {
        WM_COMMAND => {
            if let Some(app) = app {
                match wparam.0 & 0xffff {
                    LOGIN_ID => app.open_login(),
                    OPEN_ID => app.open_channel(),
                    SAMPLE_ID => app.arm(),
                    RENDER_ID => app.check_rendering(),
                    _ => (),
                }
            }
            LRESULT(0)
        }
        WM_SIZE => {
            // Keep ordinary page layout when minimized. Native occlusion and
            // background throttling remain unchanged; restoration resizes again.
            if wparam.0 != SIZE_MINIMIZED as usize {
                if let Some(app) = app {
                    app.resize();
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if let Some(app) = app {
                app.tick();
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(app) = app {
                app.window_close();
            } else {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn make_window(
    directory: PathBuf,
    config: Option<Config>,
    service_options: Option<(HostOptions, Bridge)>,
) -> windows::core::Result<Rc<App>> {
    let instance = HINSTANCE(GetModuleHandleW(None)?.0);
    let class = w!("VeylurkCommunityWebView2Proof");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        ..Default::default()
    };
    // The same fixed class can be reused after a host has closed normally.
    let _ = RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        Default::default(),
        class,
        if service_options.is_some() {
            w!("Veylurk - Community source")
        } else {
            w!("Veylurk - standalone Community source proof")
        },
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        1280,
        900,
        None,
        None,
        Some(instance),
        None,
    )?;
    let button = |title: PCWSTR, x: i32, width: i32, id: usize| {
        CreateWindowExW(
            Default::default(),
            w!("BUTTON"),
            title,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            x,
            12,
            width,
            28,
            Some(hwnd),
            Some(HMENU(id as *mut _)),
            Some(instance),
            None,
        )
    };
    button(w!("Open login"), 12, 100, LOGIN_ID)?;
    let input = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w!("EDIT"),
        w!("kamet0"),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        124,
        12,
        200,
        28,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )?;
    button(w!("Open channel"), 336, 120, OPEN_ID)?;
    button(w!("Sample Community"), 468, 170, SAMPLE_ID)?;
    button(w!("Check rendering"), 650, 150, RENDER_ID)?;
    let status = CreateWindowExW(
        Default::default(),
        w!("STATIC"),
        w!("Starting existing WebView2 runtime..."),
        WS_CHILD | WS_VISIBLE,
        12,
        48,
        1200,
        40,
        Some(hwnd),
        None,
        Some(instance),
        None,
    )?;
    let evidence = Evidence {
        schema_version: 2,
        autonomous: config.as_ref().map(RunEvidence::new),
        ..Evidence::default()
    };
    let service = service_options.map(|(options, bridge)| Service {
        last_cancel: RefCell::new(bridge.control.cancel_version.load(Ordering::Acquire)),
        bridge,
        profile: options.profile_dir,
        active: RefCell::new(None),
        pending_command: RefCell::new(None),
        pending_result: RefCell::new(None),
        login_visible: RefCell::new(false),
    });
    let app = Rc::new(App {
        hwnd,
        input,
        status,
        engine: RefCell::new(None),
        state: RefCell::new(State {
            schedule: config.map(Schedule::new),
            initializing_at: Some(Instant::now()),
            ..State::default()
        }),
        evidence: RefCell::new(evidence),
        evidence_path: directory.join("aggregate-evidence.json"),
        started: Instant::now(),
        service,
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    if SetTimer(Some(hwnd), 1, 250, None) == 0 {
        APP.with(|slot| slot.borrow_mut().take());
        let _ = DestroyWindow(hwnd);
        return Err(windows::core::Error::from_thread());
    }
    let _ = ShowWindow(
        hwnd,
        if app.service.is_some() {
            SW_MINIMIZE
        } else {
            SW_SHOW
        },
    );
    app.save();
    Ok(app)
}

fn startup(
    config: Option<Config>,
    service_options: Option<(HostOptions, Bridge)>,
) -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
            .ok_or_else(|| windows::core::Error::from_hresult(E_UNEXPECTED))?;
        let app = make_window(directory.clone(), config, service_options)?;
        // Do not inherit profile/browser/argument overrides that could select
        // another browser or alter default security. Never rewrite them.
        let overrides = [
            "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
            "WEBVIEW2_USER_DATA_FOLDER",
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "WEBVIEW2_RELEASE_CHANNEL_PREFERENCE",
            "WEBVIEW2_CHANNEL_SEARCH_KIND",
            "WEBVIEW2_RELEASE_CHANNELS",
        ];
        if overrides
            .iter()
            .any(|name| std::env::var_os(name).is_some())
        {
            app.fail("webview2_environment_override", "WebView2 environment overrides are present. Close the proof; default settings are required.");
        } else {
            let mut version = PWSTR::null();
            let available =
                GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version).is_ok();
            if !version.is_null() {
                let _ = take_pwstr(version);
            }
            let profile = app.service.as_ref().map_or_else(
                || directory.join("data").join("browser-profile"),
                |service| service.profile.clone(),
            );
            if !available {
                app.fail("runtime_missing", "No usable existing WebView2 runtime was found. Nothing will be downloaded or installed.");
            } else if app.service.is_none() && app.automatic() && !profile.is_dir() {
                app.fail(
                    "existing_profile_required",
                    "First use manual mode to create the application profile and sign in normally.",
                );
            } else if (app.service.is_some() || !app.automatic())
                && fs::create_dir_all(&profile).is_err()
            {
                app.fail(
                    "profile_directory_unavailable",
                    "Cannot create the app's browser profile next to this executable.",
                );
            } else if initialize(&app, profile).is_err() {
                app.fail(
                    "runtime_start_failed",
                    "The existing WebView2 runtime could not start. Nothing was installed.",
                );
            }
        }
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                app.message_pump_exit(result);
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = KillTimer(Some(app.hwnd), 1);
        APP.with(|slot| slot.borrow_mut().take());
        drop(app);
        CoUninitialize();
    }
    Ok(())
}

pub fn run(config: Option<Config>) {
    if startup(config, None).is_err() {
        unsafe {
            MessageBoxW(None, w!("The standalone proof could not create its native window or initialize COM. No software was installed."),
                w!("Community WebView2 proof"), MB_OK | MB_ICONERROR);
        }
    }
}

pub(crate) fn run_service(options: HostOptions, bridge: Bridge) {
    let control = bridge.control.clone();
    let events = bridge.events.clone();
    let _exit_guard = ServiceExitGuard(control.clone());
    if startup(None, Some((options, bridge))).is_err() {
        control.observe_termination(HostTerminationReason::StartupFailed);
        let _ = events.try_send(crate::host::HostEvent::Paused {
            code: "native_host_start_failed",
        });
        control.terminate(false, "native_host_start_failed");
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::host::HostHandle;

    fn fixture() -> (HostHandle, Rc<App>) {
        let (handle, bridge) = HostHandle::channel();
        bridge.control.set_controller_epoch(3);
        let app = Rc::new(App {
            hwnd: HWND::default(),
            input: HWND::default(),
            status: HWND::default(),
            engine: RefCell::new(None),
            state: RefCell::new(State {
                generation: 7,
                controller_epoch: 3,
                ..State::default()
            }),
            evidence: RefCell::new(Evidence::default()),
            evidence_path: PathBuf::new(),
            started: Instant::now(),
            service: Some(Service {
                bridge,
                profile: PathBuf::new(),
                active: RefCell::new(None),
                pending_command: RefCell::new(None),
                pending_result: RefCell::new(None),
                last_cancel: RefCell::new(0),
                login_visible: RefCell::new(false),
            }),
        });
        (handle, app)
    }

    #[test]
    fn pump_startup_and_cleanup_stop_paths_keep_their_first_reason() {
        for (result, reason) in [
            (0, HostTerminationReason::MessagePumpQuit),
            (-1, HostTerminationReason::MessagePumpError),
        ] {
            let (handle, app) = fixture();
            app.message_pump_exit(result);
            app.finish_service(false);
            assert_eq!(
                handle.fault_report().host_termination.unwrap().reason,
                reason
            );
        }
        for initializing in [false, true] {
            let (handle, app) = fixture();
            app.state.borrow_mut().initializing_at = initializing.then(Instant::now);
            app.service_failure("runtime_start_failed", false);
            app.finish_service(false);
            assert_eq!(
                handle.fault_report().host_termination.unwrap().reason,
                if initializing {
                    HostTerminationReason::StartupFailed
                } else {
                    HostTerminationReason::SourceFailureWithoutController
                }
            );
        }
        let (handle, app) = fixture();
        app.finish_service(false);
        assert_eq!(
            handle.fault_report().host_termination.unwrap().reason,
            HostTerminationReason::CleanupUnconfirmed
        );
    }

    #[test]
    fn service_window_close_preserves_active_work_admission_epoch_and_denial() {
        let (handle, app) = fixture();
        handle.try_sample(42, "room").unwrap();
        let service = app.service.as_ref().unwrap();
        let job = service.bridge.control.pending_job().unwrap();
        *service.active.borrow_mut() = Some(job.clone());
        {
            let mut state = app.state.borrow_mut();
            state.denied = true;
            state.script_pending = true;
            state.auto_stage = Some(AutoStage::Sampling);
        }
        let cancel_version = service
            .bridge
            .control
            .cancel_version
            .load(Ordering::Acquire);
        app.window_close();
        app.window_close();
        assert_eq!(app.epoch(), 3);
        assert_eq!(app.state.borrow().generation, 7);
        assert!(app.state.borrow().denied && app.state.borrow().script_pending);
        assert!(matches!(
            app.state.borrow().auto_stage,
            Some(AutoStage::Sampling)
        ));
        assert!(app.state.borrow().closing_at.is_none());
        assert!(app.state.borrow().recycling_at.is_none());
        assert_eq!(service.active.borrow().as_ref().unwrap().ticket, job.ticket);
        assert!(service.bridge.control.is_pending(job.ticket));
        assert_eq!(
            service
                .bridge
                .control
                .cancel_version
                .load(Ordering::Acquire),
            cancel_version
        );
        assert!(!service.bridge.control.shutdown.load(Ordering::Acquire));
        assert!(handle.try_event().is_err());
        assert_eq!(
            handle.try_sample(43, "room"),
            Err(crate::host::CommandError::Busy)
        );
        assert!(handle.fault_report().host_termination.is_none());
        assert_eq!(handle.fault_report().window_dismissal.unwrap().count, 2);
        service.bridge.control.complete(
            job.ticket,
            Err(SourceFailure {
                code: "community_paused",
                pauses_community: true,
            }),
        );
        assert!(matches!(
            handle.try_event(),
            Ok(crate::host::HostEvent::SampleCompleted { .. })
        ));
        assert_eq!(handle.try_sample(43, "room"), Ok(()));
    }

    #[test]
    fn login_window_close_keeps_login_visible_state_and_accepts_reopen() {
        let (handle, app) = fixture();
        let service = app.service.as_ref().unwrap();
        *service.login_visible.borrow_mut() = true;
        app.window_close();
        assert!(*service.login_visible.borrow());
        assert!(app.state.borrow().closing_at.is_none());
        assert!(handle.try_event().is_err());
        handle.show_login().unwrap();
        let QueuedCommand::ShowLogin { publication } = service.bridge.commands.try_recv().unwrap()
        else {
            panic!("expected reopened login publication");
        };
        assert!(service.accept_login_publication(publication));
        assert!(app.callbacks_allowed());
    }

    #[test]
    fn service_dismissal_cannot_replace_an_explicit_shutdown_or_its_cleanup() {
        let (handle, app) = fixture();
        handle.try_sample(42, "room").unwrap();
        handle.shutdown();
        let requested = handle.fault_report().host_termination.unwrap();
        assert_eq!(requested.reason, HostTerminationReason::ShutdownRequested);
        assert_eq!(
            (requested.controller_epoch, requested.job_id),
            (3, Some(42))
        );
        app.window_close();
        assert!(app.state.borrow().closing_at.is_none());
        app.service_tick();
        assert!(app.state.borrow().closing_at.is_some());
        app.finish_service(false);
        assert_eq!(handle.fault_report().host_termination.unwrap(), requested);
    }

    #[test]
    fn standalone_proof_window_close_still_closes_and_records_its_trigger() {
        let (_handle, mut app) = fixture();
        let path = std::env::temp_dir().join(format!(
            "veylurk-proof-close-{}-{}.json",
            std::process::id(),
            unix_milliseconds()
        ));
        let unique = Rc::get_mut(&mut app).unwrap();
        unique.service = None;
        unique.evidence_path = path.clone();
        app.window_close();
        assert!(app.state.borrow().closing_at.is_some());
        assert_eq!(
            app.state
                .borrow()
                .fault
                .host_termination
                .as_ref()
                .unwrap()
                .reason,
            HostTerminationReason::ProofWindowClose
        );
        assert!(app.state.borrow().fault.window_dismissal.is_none());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn closing_during_recycle_preserves_exit_identity_and_original_deadline() {
        let (_handle, app) = fixture();
        app.state.borrow_mut().browser.bind(3, 100);
        app.state.borrow_mut().browser.observe_exit(3, 100, 0, true);
        app.begin_recycle();
        let started = app.state.borrow().recycling_at.unwrap();
        assert_eq!(app.epoch(), 4);
        app.close();
        assert_eq!(app.epoch(), 4);
        assert_eq!(app.state.borrow().closing_at, Some(started));
        assert!(app.state.borrow().recycling_at.is_none());
        assert!(app.state.borrow().browser.diagnostics.normal_exit_observed);
        assert_eq!(app.state.borrow().browser.diagnostics.controller_epoch, 3);
    }

    #[test]
    fn primary_failure_snapshot_precedes_cancellation_and_survives_cleanup_failure() {
        let (handle, app) = fixture();
        handle.try_sample(44, "room").unwrap();
        {
            let mut state = app.state.borrow_mut();
            state.auto_stage = Some(AutoStage::Preparing(Instant::now()));
            state.navigation_ready = true;
            state.script_pending = true;
            state.network_pending = true;
            state.pending = true;
            state.last_player_observation = Some(sampling::PlayerObservation {
                video_paused: Some(true),
                ..Default::default()
            });
        }
        app.service_failure("navigation_ui_deadline", false);
        assert!(!app.state.borrow().script_pending);
        let report = handle.fault_report();
        let primary = report.primary_failure.unwrap();
        assert_eq!(
            (primary.code, primary.stage, primary.job_id),
            ("navigation_ui_deadline", "preparing", Some(44))
        );
        assert!(primary.script_pending && primary.network_pending && primary.response_pending);
        assert_eq!(
            primary.last_player_observation.unwrap().video_paused,
            Some(true)
        );
        assert!(!app.complete_cleanup(Instant::now()));
        assert_eq!(
            handle.fault_report().primary_failure.unwrap().code,
            "navigation_ui_deadline"
        );
        assert!(!handle.fault_report().cleanup_diagnostics.unwrap().confirmed);
    }

    #[test]
    fn login_publication_after_an_older_timer_snapshot_does_not_fence_its_own_callback() {
        let (handle, app) = fixture();
        let service = app.service.as_ref().unwrap();
        assert_eq!(*service.last_cancel.borrow(), 0);
        handle.show_login().unwrap();
        let QueuedCommand::ShowLogin { publication } = service
            .bridge
            .commands
            .try_recv()
            .unwrap_or_else(|_| panic!("missing login"))
        else {
            panic!("expected login");
        };
        assert!(!app.callbacks_allowed());
        assert!(service.accept_login_publication(publication));
        assert!(app.callback_current(3, 7));
        handle.cancel();
        assert!(!service.accept_login_publication(publication));
        assert!(!app.callback_current(3, 7));
    }

    #[test]
    fn missing_idle_pause_callback_bounds_a_queued_job_and_late_callback_cannot_clear_the_new_state(
    ) {
        let (handle, app) = fixture();
        handle.try_sample(23, "room").unwrap();
        let service = app.service.as_ref().unwrap();
        let job = service.bridge.control.pending_job().unwrap();
        {
            let mut state = app.state.borrow_mut();
            state.script_pending = true;
            state.script_pending_since = Some(job.queued_at);
        }
        assert_eq!(
            app.service_wait_failure(
                job.queued_at + IDLE_PAUSE_CHECK_LIMIT - Duration::from_millis(1)
            ),
            None
        );
        assert_eq!(
            app.service_wait_failure(job.queued_at + IDLE_PAUSE_CHECK_LIMIT),
            Some("idle_pause_guard_deadline")
        );
        app.state.borrow_mut().script_pending_since = Some(Instant::now() - IDLE_PAUSE_CHECK_LIMIT);
        // No controller/window is created by this fixture. The actual timer
        // failure/termination path still owns and completes the queued job.
        app.tick();
        assert!(app.state.borrow().closing_at.is_some());
        assert!(!app.state.borrow().script_pending);
        assert!(!app.callback_current(3, 7));
        assert!(app.engine.borrow().is_none());
        assert!(matches!(
            service.bridge.commands.try_recv(),
            Ok(QueuedCommand::Sample { .. })
        ));
        app.state.borrow_mut().closing_at = Some(Instant::now() - FLUSH_TIME);
        app.tick();
        assert!(matches!(
            handle.try_event(),
            Ok(crate::host::HostEvent::SampleCompleted {
                job_id: 23,
                outcome: Err(SourceFailure {
                    code: "idle_pause_guard_deadline",
                    ..
                }),
                ..
            })
        ));
        let mut stopped = false;
        for _ in 0..5 {
            match handle.try_event() {
                Ok(crate::host::HostEvent::Paused { code }) => {
                    assert_eq!(code, "idle_pause_guard_deadline")
                }
                Ok(crate::host::HostEvent::Stopped {
                    cleanup_confirmed: false,
                    ..
                }) => {
                    stopped = true;
                    break;
                }
                _ => panic!("missing terminal cleanup or duplicate completion"),
            }
        }
        assert!(stopped);
        assert!(handle.try_event().is_err());
        assert_eq!(
            handle.try_sample(24, "room"),
            Err(crate::host::CommandError::Closed)
        );
    }

    #[test]
    fn queued_job_waiting_before_runtime_readiness_has_a_fixed_deadline() {
        let (handle, app) = fixture();
        handle.try_sample(25, "room").unwrap();
        let job = app
            .service
            .as_ref()
            .unwrap()
            .bridge
            .control
            .pending_job()
            .unwrap();
        assert_eq!(
            app.service_wait_failure(
                job.queued_at + Duration::from_millis(sampling::NAVIGATION_UI_MS - 1)
            ),
            None
        );
        assert_eq!(
            app.service_wait_failure(
                job.queued_at + Duration::from_millis(sampling::NAVIGATION_UI_MS)
            ),
            Some("sample_queue_deadline")
        );
    }

    #[test]
    fn stale_controller_and_job_callbacks_and_cancelled_jobs_are_rejected_without_native_calls() {
        let (handle, app) = fixture();
        assert!(app.callback_current(3, 7));
        assert!(!app.callback_current(2, 7));
        assert!(!app.callback_current(3, 6));
        handle.cancel();
        assert!(!app.callback_current(3, 7));
        *app.service.as_ref().unwrap().last_cancel.borrow_mut() = 1;
        assert!(app.callback_current(3, 7));
        for id in 1..=RECYCLE_NAVIGATIONS as u64 {
            let intent = app.state.borrow_mut().navigation_gate.begin();
            assert!(app.state.borrow_mut().navigation_gate.issue(intent));
            assert!(app.state.borrow_mut().navigation_gate.started(id));
        }
        assert_eq!(
            app.state.borrow().navigation_gate.retained_ids(),
            RECYCLE_NAVIGATIONS
        );
        assert!(app.state.borrow().navigation_gate.retained_ids() < sampling::MAX_NAVIGATION_IDS);
        app.state.borrow_mut().controller_epoch += 1;
        app.state.borrow_mut().generation += 1;
        app.state.borrow_mut().navigation_gate = sampling::NavigationGate::default();
        assert!(!app.callback_current(3, 7));
        assert!(app.callback_current(4, 8));
        assert_eq!(app.state.borrow().navigation_gate.retained_ids(), 0);
        handle.shutdown();
        assert!(!app.callback_current(4, 8));
    }
}
