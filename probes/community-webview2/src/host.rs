//! Native Windows proof host. No private HTTP client, request replay, browser
//! flags, cookie API, remote debugging port, or production collection.
use community_webview2_proof::{
    channel_login, is_channel_page, is_popout_page, parse_response,
    sampling::{self, BodyRequest, Config, RunEvidence, Schedule, Tracker},
    Evidence, Failure, MAX_SAMPLES,
};
use serde::Deserialize;
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

// Reads only source-page structure; never executes on login. The browser clock
// immediately before the ordinary click fences delayed pre-arm requests.
const COMMUNITY_UI: &str = r#"(() => {
  const expected = __POPOUT__ ? '/popout/__CHANNEL__/chat' : '/__CHANNEL__';
  const video_present = !!document.querySelector('video');
  const result = code => ({code, video_present, trigger_unix_ms:null});
  if (location.origin !== 'https://www.twitch.tv' || location.pathname.toLowerCase() !== expected)
    return result('wrong_page');
  if (/access denied|verify you are human|unusual traffic/i.test(document.title)
      || document.querySelector('iframe[src*="captcha" i],iframe[src*="challenge" i],form[action*="challenge" i]'))
    return result('challenge_denied');
  if (__POPOUT__ && video_present) return result('video_present');
  const visible = e => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().height > 0;
  const buttons = [...document.querySelectorAll('button,[role="tab"]')].filter(visible);
  const target = buttons.find(e => e.getAttribute('data-test-selector') === 'chat-viewer-list')
    || buttons.find(e => e.getAttribute('data-a-target') === 'chat-viewers')
    || buttons.find(e => (e.getAttribute('aria-label') || '').trim() === 'Community')
    || buttons.find(e => (e.textContent || '').trim() === 'Community');
  if (!target || target.disabled) return result('waiting_ui');
  if (!__CLICK__) return result('ready');
  const trigger_unix_ms = performance.timeOrigin + performance.now();
  target.click();
  return {code:'native_community_clicked',video_present,trigger_unix_ms};
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
    denied: bool,
    closing_at: Option<Instant>,
    browser_exited: bool,
    evidence_write_failed: bool,
    tracker: Option<Tracker>,
    network_enabled: bool,
    network_pending: bool,
    navigation_ready: bool,
    navigation_gate: sampling::NavigationGate,
    frame_id: Option<String>,
    schedule: Option<Schedule>,
    auto_stage: Option<AutoStage>,
}

#[derive(Clone, Copy)]
enum AutoStage {
    Navigating(Instant),
    Ready(Instant),
    Sampling,
}

#[derive(Deserialize)]
struct UiResult {
    code: String,
    video_present: bool,
    trigger_unix_ms: Option<f64>,
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
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl App {
    fn status(&self, message: &str) {
        let text = wide(message);
        unsafe {
            let _ = SetWindowTextW(self.status, PCWSTR(text.as_ptr()));
        }
    }

    fn save(&self) -> bool {
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
        state.network_pending = false;
        state.tracker = None;
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
        self.state.borrow().schedule.is_some()
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
            state.navigation_gate.begin()
        };
        let generation = self.state.borrow().generation;
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _| {
                if let Some(app) = weak.upgrade() {
                    if app.state.borrow().generation != generation
                        || app.state.borrow().closing_at.is_some()
                    {
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
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, _| {
                if let Some(app) = weak.upgrade() {
                    if app.state.borrow().generation != generation
                        || app.state.borrow().closing_at.is_some()
                    {
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
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                let Some(app) = weak.upgrade() else {
                    return Ok(());
                };
                if app.state.borrow().generation != generation
                    || app.state.borrow().closing_at.is_some()
                {
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
                    app.begin_sample();
                } else {
                    let stage = app.state.borrow().auto_stage;
                    if let Some(AutoStage::Navigating(deadline)) = stage {
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

    fn begin_sample(self: &Rc<Self>) {
        let generation;
        let now = self.elapsed_ms();
        {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            generation = state.generation;
            state.armed_until = Some(Instant::now() + ARM_TIME);
            state.last_round = Some(Instant::now());
            if let Some(schedule) = state.schedule.as_mut() {
                schedule.note_trigger_started(now);
            }
            state.tracker = Some(Tracker::new(
                generation,
                state.selected_channel.clone().unwrap_or_default(),
                state.frame_id.clone().unwrap_or_default(),
            ));
            if state.schedule.is_some() {
                state.auto_stage = Some(AutoStage::Sampling);
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

    fn ui_step(self: &Rc<Self>, click: bool) {
        let (channel, generation, automatic) = {
            let state = self.state.borrow();
            if state.script_pending || state.closing_at.is_some() {
                return;
            }
            (
                state.selected_channel.clone().unwrap_or_default(),
                state.generation,
                state.schedule.is_some(),
            )
        };
        let Ok(uri) = self.current_uri() else {
            self.fail(
                "source_uri_unavailable",
                "Cannot verify the current source page.",
            );
            return;
        };
        if !is_channel_page(&uri, &channel) || automatic && !is_popout_page(&uri, &channel) {
            self.sample_failure(Failure::AuthenticationDenied);
            return;
        }
        let popout = is_popout_page(&uri, &channel);
        let script = COMMUNITY_UI
            .replace("__CHANNEL__", &channel)
            .replace("__POPOUT__", if popout { "true" } else { "false" })
            .replace("__CLICK__", if click { "true" } else { "false" });
        self.state.borrow_mut().script_pending = true;
        let weak = Rc::downgrade(self);
        let done = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            let Some(app) = weak.upgrade() else {
                return Ok(());
            };
            if app.state.borrow().generation != generation
                || app.state.borrow().closing_at.is_some()
            {
                return Ok(());
            }
            app.state.borrow_mut().script_pending = false;
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
            if let Some(run) = app.evidence.borrow_mut().autonomous.as_mut() {
                if let Some(attempt) = run.attempts.last_mut() {
                    attempt.video_element_present = Some(ui.video_present);
                }
            }
            match ui.code.as_str() {
                "challenge_denied" => app.sample_failure(Failure::ChallengeDenied),
                "wrong_page" => app.sample_failure(Failure::AuthenticationDenied),
                "video_present" => app.fail(
                    "popout_contains_video",
                    "The chat-only prerequisite failed; no source sample is accepted.",
                ),
                "waiting_ui" if !click => (),
                "waiting_ui" => app.fail(
                    "community_control_unavailable",
                    "The ordinary Community control is unavailable. No repeated click.",
                ),
                "ready" if !click => {
                    let stage = app.state.borrow().auto_stage;
                    if let Some(AutoStage::Ready(deadline)) = stage {
                        if Instant::now() >= deadline {
                            app.end_auto_attempt("navigation_ui_deadline", false);
                        } else {
                            app.begin_sample();
                        }
                    }
                }
                "native_community_clicked" if click => {
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
                        Ok(()) => app.try_body(),
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
            Ok(())
        }));
        if let Some(core) = self.core() {
            let script = wide(&script);
            if unsafe { core.ExecuteScript(PCWSTR(script.as_ptr()), &done) }.is_err() {
                self.state.borrow_mut().script_pending = false;
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
        let weak = Rc::downgrade(self);
        let done =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                let Some(app) = weak.upgrade() else {
                    return Ok(());
                };
                if !app.is_active(generation) {
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
                let parsed = parse_response(&bytes, &verified.selection, verified.status);
                drop(bytes);
                match parsed {
                    Ok(parsed) => app.accept_sample(parsed, &verified),
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

    fn accept_sample(&self, parsed: community_webview2_proof::ParsedSample, body: &BodyRequest) {
        if self.automatic() {
            if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
                run.sample(parsed, body);
            }
            self.evidence.borrow_mut().source_proven_in_this_run = true;
            self.end_auto_attempt("sample_received", false);
        } else {
            self.cancel_round();
            self.disable_network();
            let mut evidence = self.evidence.borrow_mut();
            evidence.add(parsed, &body.selection, unix_seconds());
            if let Some(sample) = evidence.samples.last_mut() {
                sample.provenance = Some(body.provenance.clone());
            }
            drop(evidence);
            self.save();
            self.status("Native sample saved. Fresh server state remains unproved; wait 30 seconds for another manual attempt.");
        }
    }

    fn end_auto_attempt(&self, code: &'static str, stop: bool) {
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
                state.auto_stage = Some(AutoStage::Navigating(
                    Instant::now() + Duration::from_millis(sampling::NAVIGATION_UI_MS),
                ));
            }
            if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
                run.start(attempt.clone());
                run.status = "running";
            }
            self.record_action("twitch_chat_popout");
            self.navigate(format!(
                "https://www.twitch.tv/popout/{}/chat",
                attempt.channel
            ));
            return;
        }
        let stage = self.state.borrow().auto_stage;
        match stage {
            Some(AutoStage::Navigating(deadline) | AutoStage::Ready(deadline))
                if Instant::now() >= deadline =>
            {
                self.end_auto_attempt("navigation_ui_deadline", false)
            }
            Some(AutoStage::Navigating(_)) => {
                let (ready, pending) = {
                    let s = self.state.borrow();
                    (s.navigation_ready, s.network_pending)
                };
                if ready && !pending {
                    self.enable_network(false);
                }
            }
            Some(AutoStage::Ready(_)) => self.ui_step(false),
            _ => (),
        }
    }
    fn resize(&self) {
        let engine = self.engine.borrow();
        if let Some(engine) = engine.as_ref() {
            unsafe {
                let mut bounds = RECT::default();
                if GetClientRect(self.hwnd, &mut bounds).is_ok() {
                    bounds.top = TOOLBAR_HEIGHT;
                    bounds.bottom = bounds.bottom.max(bounds.top);
                    let _ = engine.controller.SetBounds(bounds);
                    let _ = MoveWindow(self.status, 12, 48, (bounds.right - 24).max(1), 40, true);
                }
            }
        }
    }

    fn close(&self) {
        if self.state.borrow().closing_at.is_some() {
            return;
        }
        if let Some(run) = self.evidence.borrow_mut().autonomous.as_mut() {
            run.finalize(self.elapsed_ms(), "operator_cancelled");
        }
        self.cancel_round();
        self.state.borrow_mut().closing_at = Some(Instant::now());
        self.evidence.borrow_mut().phase = "closing";
        self.evidence.borrow_mut().browser_cleanup = "waiting_for_browser_exit";
        self.save();
        self.status(
            "Closing the embedded browser and flushing aggregate evidence (up to five seconds)...",
        );
        if let Some(engine) = self.engine.borrow().as_ref() {
            unsafe {
                for (receiver, token) in &engine.protocol_receivers {
                    let _ = receiver.remove_DevToolsProtocolEventReceived(*token);
                }
                let _ = engine
                    .core
                    .remove_NewWindowRequested(engine.new_window_token);
                let _ = engine
                    .core
                    .remove_NavigationStarting(engine.navigation_token);
                let _ = engine.core.remove_SourceChanged(engine.source_token);
                let _ = engine
                    .core
                    .remove_NavigationCompleted(engine.navigation_completed_token);
                let _ = engine
                    .core2
                    .remove_DOMContentLoaded(engine.document_loaded_token);
                let _ = engine
                    .core
                    .remove_ProcessFailed(engine.process_failed_token);
                let _ = engine.controller.Close();
            }
        }
        // Release WebView/controller references immediately. The environment is
        // retained only to receive BrowserProcessExited during the bounded flush.
        if let Some(engine) = self.engine.borrow_mut().take() {
            EXIT_ENV.with(|slot| {
                *slot.borrow_mut() =
                    Some((engine.environment, engine.environment5, engine.exit_token))
            });
        }
    }

    fn tick(self: &Rc<Self>) {
        let closing = self.state.borrow().closing_at;
        if let Some(started) = closing {
            let exited = self.state.borrow().browser_exited;
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
                EXIT_ENV.with(|slot| {
                    if let Some((_, environment5, token)) = slot.borrow_mut().take() {
                        unsafe {
                            let _ = environment5.remove_BrowserProcessExited(token);
                        }
                    }
                });
                unsafe {
                    let _ = DestroyWindow(self.hwnd);
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
                self.end_auto_attempt("native_sample_timeout", false);
            } else {
                self.fail(
                    "native_sample_timeout",
                    "No matching native Community response within 25 seconds. No automatic retry.",
                );
            }
        }
        if self.automatic() {
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
    let weak = Rc::downgrade(app);
    let environment_handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |result, environment| {
            let Some(app) = weak.upgrade() else {
                return Ok(());
            };
            if app.state.borrow().closing_at.is_some() {
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
                    if app.state.borrow().closing_at.is_some() {
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
    let mut close_on_failure = ControllerGuard(Some(controller.clone()));
    let core = controller.CoreWebView2()?;
    let core2: ICoreWebView2_2 = core.cast()?;
    let environment5: ICoreWebView2Environment5 = environment.cast()?;
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
    let exit_handler = BrowserProcessExitedEventHandler::create(Box::new(move |_, _| {
        if let Some(app) = weak.upgrade() {
            app.state.borrow_mut().browser_exited = true;
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
                        app.end_auto_attempt("navigation_failed", false);
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
    if !app.automatic() {
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
            if let Some(app) = app {
                app.resize();
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
                app.close();
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
    if RegisterClassW(&wc) == 0 {
        return Err(windows::core::Error::from_thread());
    }
    let hwnd = CreateWindowExW(
        Default::default(),
        class,
        w!("Veylurk - standalone Community source proof"),
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
    let mut evidence = Evidence::default();
    evidence.schema_version = 2;
    evidence.autonomous = config.as_ref().map(RunEvidence::new);
    let app = Rc::new(App {
        hwnd,
        input,
        status,
        engine: RefCell::new(None),
        state: RefCell::new(State {
            schedule: config.map(Schedule::new),
            ..State::default()
        }),
        evidence: RefCell::new(evidence),
        evidence_path: directory.join("aggregate-evidence.json"),
        started: Instant::now(),
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    if SetTimer(Some(hwnd), 1, 250, None) == 0 {
        APP.with(|slot| slot.borrow_mut().take());
        let _ = DestroyWindow(hwnd);
        return Err(windows::core::Error::from_thread());
    }
    let _ = ShowWindow(hwnd, SW_SHOW);
    app.save();
    Ok(app)
}

fn startup(config: Option<Config>) -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
            .ok_or_else(|| windows::core::Error::from_hresult(E_UNEXPECTED))?;
        let app = make_window(directory.clone(), config)?;
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
            let profile = directory.join("data").join("browser-profile");
            if !available {
                app.fail("runtime_missing", "No usable existing WebView2 runtime was found. Nothing will be downloaded or installed.");
            } else if app.automatic() && !profile.is_dir() {
                app.fail(
                    "existing_profile_required",
                    "First use manual mode to create the application profile and sign in normally.",
                );
            } else if !app.automatic() && fs::create_dir_all(&profile).is_err() {
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
    if startup(config).is_err() {
        unsafe {
            MessageBoxW(None, w!("The standalone proof could not create its native window or initialize COM. No software was installed."),
                w!("Community WebView2 proof"), MB_OK | MB_ICONERROR);
        }
    }
}
