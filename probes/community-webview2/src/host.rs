//! Native Windows proof host. No private HTTP client, request replay, browser
//! flags, cookie API, remote debugging port, or automatic collection scheduler.
use community_webview2_proof::{
    channel_login, is_channel_page, is_source_uri, parse_response, select_request, Evidence,
    Failure, MAX_SAMPLES, REQUEST_CAP, RESPONSE_CAP,
};
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows::{
    core::{w, Interface, PCWSTR, PWSTR},
    Win32::{
        Foundation::{E_UNEXPECTED, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::WindowsAndMessaging::*,
    },
};

const LOGIN_ID: usize = 101;
const OPEN_ID: usize = 102;
const SAMPLE_ID: usize = 103;
const ARM_TIME: Duration = Duration::from_secs(25);
const ROUND_GAP: Duration = Duration::from_secs(30);
const FLUSH_TIME: Duration = Duration::from_secs(5);
const MAX_GQL_EVENTS: usize = 64;
const TOOLBAR_HEIGHT: i32 = 92;

// Only executes after an explicit native Sample button click on the selected
// channel page. Reads no inputs, login state, account names, or login DOM.
// If selectors drift, the owner can click Twitch's Community tab manually.
const OPEN_COMMUNITY: &str = r#"(() => {
  const buttons = [...document.querySelectorAll('button,[role="tab"]')];
  const target = buttons.find(e => e.getAttribute('data-a-target') === 'chat-viewers')
    || buttons.find(e => (e.getAttribute('aria-label') || '').trim() === 'Community')
    || buttons.find(e => (e.textContent || '').trim() === 'Community');
  if (!target) return 'manual_community_required';
  target.click();
  return 'native_community_clicked';
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
    response_token: i64,
    exit_token: i64,
    new_window_token: i64,
    navigation_token: i64,
    source_token: i64,
}

#[derive(Default)]
struct State {
    selected_channel: Option<String>,
    armed_until: Option<Instant>,
    last_round: Option<Instant>,
    generation: u64,
    gql_events: usize,
    pending: bool,
    script_pending: bool,
    denied: bool,
    closing_at: Option<Instant>,
    browser_exited: bool,
    evidence_write_failed: bool,
}

struct App {
    hwnd: HWND,
    input: HWND,
    status: HWND,
    engine: RefCell<Option<Engine>>,
    state: RefCell<State>,
    evidence: RefCell<Evidence>,
    evidence_path: PathBuf,
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
        {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.armed_until = None;
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
        {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            state.armed_until = None;
            state.denied |= failure.is_denial();
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

    fn open_login(&self) {
        if self.state.borrow().closing_at.is_some() {
            return;
        }
        self.cancel_round();
        let core = self
            .engine
            .borrow()
            .as_ref()
            .map(|engine| engine.core.clone());
        if let Some(core) = core {
            if unsafe { core.Navigate(w!("https://www.twitch.tv/login")) }.is_err() {
                self.fail("navigation_failed", "Could not open normal Twitch login.");
            } else {
                self.status(
                    "Sign in manually in this app. Then enter a channel and choose Open channel.",
                );
            }
        }
    }

    fn cancel_round(&self) {
        let mut state = self.state.borrow_mut();
        state.generation += 1;
        state.armed_until = None;
    }

    fn navigation_changed(&self) {
        let active = {
            let state = self.state.borrow();
            state.closing_at.is_none()
                && (state.armed_until.is_some() || state.pending || state.script_pending)
        };
        if !active {
            return;
        }
        self.cancel_round();
        {
            let mut evidence = self.evidence.borrow_mut();
            evidence.phase = "sample_cancelled";
            evidence.status = "navigation_changed";
        }
        if self.save() {
            self.status("Navigation cancelled the sample. Open the intended channel before another manual attempt.");
        }
    }

    fn open_channel(&self) {
        if self.state.borrow().closing_at.is_some() {
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
        self.state.borrow_mut().selected_channel = Some(channel.clone());
        self.evidence.borrow_mut().channel = Some(channel.clone());
        let core = self
            .engine
            .borrow()
            .as_ref()
            .map(|engine| engine.core.clone());
        if let Some(core) = core {
            let url = wide(&format!("https://www.twitch.tv/{channel}"));
            if unsafe { core.Navigate(PCWSTR(url.as_ptr())) }.is_err() {
                self.fail(
                    "navigation_failed",
                    "Could not open the selected Twitch channel.",
                );
            } else if self.save() {
                self.status("Wait for chat to load. Close Community if already open, then choose Sample Community.");
            }
        }
    }

    fn arm(self: &Rc<Self>) {
        let channel;
        {
            let state = self.state.borrow();
            if state.closing_at.is_some() || state.denied || state.evidence_write_failed {
                return;
            }
            if state.pending || state.script_pending || state.armed_until.is_some() {
                self.status("A sample or native response callback is already pending.");
                return;
            }
            if state
                .last_round
                .is_some_and(|time| time.elapsed() < ROUND_GAP)
            {
                self.status("Wait at least 30 seconds between manual sample attempts.");
                return;
            }
            let Some(selected) = state.selected_channel.clone() else {
                self.status("Open a channel first.");
                return;
            };
            channel = selected;
        }
        if self.evidence.borrow().manual_attempt_count >= MAX_SAMPLES {
            self.status("Three manual sample attempts are complete. Close this bounded proof.");
            return;
        }
        let Ok(uri) = self.current_uri() else {
            self.fail(
                "source_uri_unavailable",
                "Cannot verify the current channel page.",
            );
            return;
        };
        if !is_channel_page(&uri, &channel) {
            self.status(
                "Sampling requires the selected Twitch channel page. Open the channel again.",
            );
            return;
        }
        let generation;
        {
            let mut state = self.state.borrow_mut();
            state.generation += 1;
            generation = state.generation;
            state.armed_until = Some(Instant::now() + ARM_TIME);
            state.last_round = Some(Instant::now());
            state.gql_events = 0;
            state.script_pending = true;
        }
        {
            let mut evidence = self.evidence.borrow_mut();
            evidence.phase = "armed";
            evidence.status = "awaiting_native_community";
            evidence.manual_attempt_count += 1;
        }
        if !self.save() {
            self.cancel_round();
            return;
        }
        self.status("Sample armed for 25 seconds. Click Twitch's Community tab if it does not open automatically.");
        let weak = Rc::downgrade(self);
        let handler =
            ExecuteScriptCompletedHandler::create(Box::new(move |result, _safe_result| {
                if let Some(app) = weak.upgrade() {
                    app.state.borrow_mut().script_pending = false;
                    if app.is_active(generation) && result.is_err() {
                        app.fail(
                            "community_ui_action_failed",
                            "Community UI action failed. No automatic retry.",
                        );
                    }
                }
                Ok(())
            }));
        let core = self
            .engine
            .borrow()
            .as_ref()
            .map(|engine| engine.core.clone());
        if let Some(core) = core {
            let script = wide(OPEN_COMMUNITY);
            if unsafe { core.ExecuteScript(PCWSTR(script.as_ptr()), &handler) }.is_err() {
                self.state.borrow_mut().script_pending = false;
                self.fail(
                    "community_ui_action_failed",
                    "Community UI action failed. No automatic retry.",
                );
            }
        }
    }

    fn is_active(&self, generation: u64) -> bool {
        let state = self.state.borrow();
        state.generation == generation
            && state.closing_at.is_none()
            && !state.denied
            && state.armed_until.is_some_and(|time| Instant::now() < time)
    }

    fn observe(
        self: &Rc<Self>,
        args: ICoreWebView2WebResourceResponseReceivedEventArgs,
    ) -> windows::core::Result<()> {
        let (generation, channel) = {
            let state = self.state.borrow();
            if state.closing_at.is_some()
                || state.denied
                || state.pending
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
        unsafe {
            let request = args.Request()?;
            let mut uri = PWSTR::null();
            request.Uri(&mut uri)?;
            let uri = take_pwstr(uri);
            if !is_source_uri(&uri) {
                return Ok(());
            }
            {
                let mut state = self.state.borrow_mut();
                state.gql_events += 1;
                if state.gql_events > MAX_GQL_EVENTS {
                    drop(state);
                    self.fail(
                        "gql_event_limit",
                        "Sample stopped at its native event budget.",
                    );
                    return Ok(());
                }
            }
            let mut method = PWSTR::null();
            request.Method(&mut method)?;
            if take_pwstr(method) != "POST" {
                return Ok(());
            }
            // Only gql POST bodies are read. Login/password requests and all
            // request/response headers are never inspected.
            let Ok(content) = request.Content() else {
                return Ok(());
            };
            let bytes = match read_stream(&content, REQUEST_CAP, true) {
                Ok(bytes) => bytes,
                Err(StreamFailure::Limit) => {
                    self.sample_failure(Failure::RequestTooLarge);
                    return Ok(());
                }
                Err(StreamFailure::Unavailable) => {
                    self.sample_failure(Failure::RequestReadFailed);
                    return Ok(());
                }
            };
            let selection = match select_request(&bytes, &channel) {
                Ok(Some(selected)) => selected,
                Ok(None) => return Ok(()),
                Err(failure) => {
                    self.sample_failure(failure);
                    return Ok(());
                }
            };
            // Drop the raw request body before requesting any response body.
            drop(bytes);
            if !self
                .current_uri()
                .is_ok_and(|uri| is_channel_page(&uri, &channel))
            {
                self.fail(
                    "channel_navigation_changed",
                    "The page changed while sampling. No source sample was accepted.",
                );
                return Ok(());
            }
            let response = args.Response()?;
            let mut status = 0;
            response.StatusCode(&mut status)?;
            let status = u16::try_from(status).unwrap_or(0);
            if matches!(status, 401 | 403 | 429) {
                self.sample_failure(Failure::HttpDenied);
                return Ok(());
            }
            if status != 200 {
                self.sample_failure(Failure::HttpError);
                return Ok(());
            }
            self.state.borrow_mut().pending = true;
            let weak = Rc::downgrade(self);
            let handler = WebResourceResponseViewGetContentCompletedHandler::create(Box::new(
                move |result, stream| {
                    let Some(app) = weak.upgrade() else {
                        return Ok(());
                    };
                    app.state.borrow_mut().pending = false;
                    if !app.is_active(generation) {
                        return Ok(());
                    }
                    if !app
                        .current_uri()
                        .is_ok_and(|uri| is_channel_page(&uri, &channel))
                    {
                        app.fail("channel_navigation_changed", "The page changed while the native response was pending. No source sample was accepted.");
                        return Ok(());
                    }
                    if result.is_err() || stream.is_none() {
                        app.fail(
                            "response_content_unavailable",
                            "Native response content was unavailable. No automatic retry.",
                        );
                        return Ok(());
                    }
                    let stream = stream.unwrap();
                    let bytes = match read_stream(&stream, RESPONSE_CAP, false) {
                        Ok(bytes) => bytes,
                        Err(StreamFailure::Limit) => {
                            app.sample_failure(Failure::ResponseTooLarge);
                            return Ok(());
                        }
                        Err(StreamFailure::Unavailable) => {
                            app.sample_failure(Failure::ResponseReadFailed);
                            return Ok(());
                        }
                    };
                    let parsed = parse_response(&bytes, &selection, status);
                    drop(bytes);
                    match parsed {
                        Ok(parsed) => {
                            app.cancel_round();
                            app.evidence
                                .borrow_mut()
                                .add(parsed, &selection, unix_seconds());
                            if app.save() {
                                let evidence = app.evidence.borrow();
                                let sample = evidence.samples.last().unwrap();
                                app.status(&format!("Sample {}/3 saved: reported count {}, {} unique accounts. Wait 30 seconds before another manual sample.",
                                evidence.sample_count, sample.count, sample.unique_accounts));
                            }
                        }
                        Err(failure) => app.sample_failure(failure),
                    }
                    Ok(())
                },
            ));
            if response.GetContent(&handler).is_err() {
                self.state.borrow_mut().pending = false;
                self.fail(
                    "response_content_unavailable",
                    "Native response content was unavailable. No automatic retry.",
                );
            }
        }
        Ok(())
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
                let _ = engine
                    .core2
                    .remove_WebResourceResponseReceived(engine.response_token);
                let _ = engine
                    .core
                    .remove_NewWindowRequested(engine.new_window_token);
                let _ = engine
                    .core
                    .remove_NavigationStarting(engine.navigation_token);
                let _ = engine.core.remove_SourceChanged(engine.source_token);
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

    fn tick(&self) {
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
            self.fail(
                "native_sample_timeout",
                "No matching native Community response within 25 seconds. No automatic retry.",
            );
        }
    }
}

thread_local! {
    static EXIT_ENV: RefCell<Option<(ICoreWebView2Environment, ICoreWebView2Environment5, i64)>> = const { RefCell::new(None) };
}

/// Read at most cap+1 bytes, restore the request stream's exact prior position,
/// and reject streams that cannot be positioned safely. Never changes content.
enum StreamFailure {
    Limit,
    Unavailable,
}

impl From<windows::core::Error> for StreamFailure {
    fn from(_: windows::core::Error) -> Self {
        Self::Unavailable
    }
}

unsafe fn read_stream(
    stream: &IStream,
    cap: usize,
    preserve: bool,
) -> std::result::Result<Vec<u8>, StreamFailure> {
    let mut position = 0;
    if preserve {
        stream.Seek(0, STREAM_SEEK_CUR, Some(&mut position))?;
    }
    stream.Seek(0, STREAM_SEEK_SET, None)?;
    let result = (|| {
        let mut output = Vec::with_capacity((cap + 1).min(8192));
        loop {
            let mut chunk = [0u8; 8192];
            let wanted = chunk.len().min(cap + 1 - output.len());
            let mut read = 0;
            stream
                .Read(chunk.as_mut_ptr().cast(), wanted as u32, Some(&mut read))
                .ok()?;
            if read == 0 {
                return Ok(output);
            }
            if read as usize > wanted {
                return Err(StreamFailure::Unavailable);
            }
            output.extend_from_slice(&chunk[..read as usize]);
            if output.len() > cap {
                return Err(StreamFailure::Limit);
            }
        }
    })();
    if preserve {
        stream.Seek(
            i64::try_from(position).map_err(|_| StreamFailure::Unavailable)?,
            STREAM_SEEK_SET,
            None,
        )?;
    }
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
    let weak = Rc::downgrade(app);
    let response_handler =
        WebResourceResponseReceivedEventHandler::create(Box::new(move |_, args| {
            if let (Some(app), Some(args)) = (weak.upgrade(), args) {
                if app.observe(args).is_err() {
                    app.fail(
                        "native_observer_failed",
                        "Native response observation failed. No automatic retry.",
                    );
                }
            }
            Ok(())
        }));
    let mut response_token = 0;
    core2.add_WebResourceResponseReceived(&response_handler, &mut response_token)?;
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
    let navigation_handler = NavigationStartingEventHandler::create(Box::new(move |_, _| {
        if let Some(app) = weak.upgrade() {
            app.navigation_changed();
        }
        Ok(())
    }));
    let mut navigation_token = 0;
    core.add_NavigationStarting(&navigation_handler, &mut navigation_token)?;
    let weak = Rc::downgrade(app);
    let source_handler = SourceChangedEventHandler::create(Box::new(move |_, _| {
        if let Some(app) = weak.upgrade() {
            app.navigation_changed();
        }
        Ok(())
    }));
    let mut source_token = 0;
    core.add_SourceChanged(&source_handler, &mut source_token)?;
    controller.SetIsVisible(true)?;
    *app.engine.borrow_mut() = Some(Engine {
        controller,
        core,
        core2,
        environment,
        environment5,
        response_token,
        exit_token,
        new_window_token,
        navigation_token,
        source_token,
    });
    close_on_failure.0 = None;
    app.resize();
    app.evidence.borrow_mut().phase = "ready";
    app.evidence.borrow_mut().status = "manual_login_required";
    app.evidence.borrow_mut().browser_cleanup = "browser_running";
    app.save();
    app.open_login();
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

unsafe fn make_window(directory: PathBuf) -> windows::core::Result<Rc<App>> {
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
    let app = Rc::new(App {
        hwnd,
        input,
        status,
        engine: RefCell::new(None),
        state: RefCell::new(State::default()),
        evidence: RefCell::new(Evidence::default()),
        evidence_path: directory.join("aggregate-evidence.json"),
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

fn startup() -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
            .ok_or_else(|| windows::core::Error::from_hresult(E_UNEXPECTED))?;
        let app = make_window(directory.clone())?;
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
            } else if fs::create_dir_all(&profile).is_err() {
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

pub fn run() {
    if startup().is_err() {
        unsafe {
            MessageBoxW(None, w!("The standalone proof could not create its native window or initialize COM. No software was installed."),
                w!("Community WebView2 proof"), MB_OK | MB_ICONERROR);
        }
    }
}
