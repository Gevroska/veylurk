use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        JobObjects::CreateJobObjectW,
        JobObjects::{
            AssignProcessToJobObject, JobObjectExtendedLimitInformation, SetInformationJobObject,
            TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
    },
};

const MAX_CDP_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BrowserProbeError {
    #[error("Brave Browser was not found")]
    BrowserNotFound,
    #[error("browser launch failed: {0}")]
    Launch(String),
    #[error("browser challenge detected; stopped without attempting to solve it")]
    Challenge,
    #[error("Twitch chat UI is unavailable: {0}")]
    Unavailable(String),
    #[error("Twitch UI changed or did not become ready: {0}")]
    UiChanged(String),
    #[error("browser protocol failed: {0}")]
    Protocol(String),
    #[error("page channel {actual:?} did not match requested channel {requested:?}")]
    ChannelMismatch { requested: String, actual: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct DomSample {
    pub channel: String,
    pub origin: String,
    pub status: String,
    pub reason: String,
    pub usernames: Vec<String>,
    pub role_lists: usize,
    pub scroll_rounds: usize,
    pub reached_end: bool,
    pub ready_state: String,
    pub document_lang: String,
    pub known_error_title_present: bool,
    pub viewer_toggle_present: bool,
    pub viewer_input_present: bool,
    pub rendered_row_count: usize,
    pub login_prompt_present: bool,
}

impl DomSample {
    pub fn validate(mut self, requested: &str) -> Result<Self, BrowserProbeError> {
        if !self.channel.eq_ignore_ascii_case(requested) {
            return Err(BrowserProbeError::ChannelMismatch {
                requested: requested.to_owned(),
                actual: self.channel,
            });
        }
        if self.origin != "https://www.twitch.tv" {
            return Err(BrowserProbeError::UiChanged(format!(
                "unexpected page origin {:?}",
                self.origin
            )));
        }
        match self.status.as_str() {
            "challenge" => return Err(BrowserProbeError::Challenge),
            "unavailable" => {
                return Err(BrowserProbeError::Unavailable(format!(
                    "reason={}; ready_state={}; lang={:?}; known_error_title={}; toggle={}; input={}; role_lists={}; rendered_rows={}; login_prompt={}",
                    self.reason,
                    self.ready_state,
                    self.document_lang,
                    self.known_error_title_present,
                    self.viewer_toggle_present,
                    self.viewer_input_present,
                    self.role_lists,
                    self.rendered_row_count,
                    self.login_prompt_present
                )))
            }
            "ok" => {}
            "ui_changed" => {
                return Err(BrowserProbeError::UiChanged(format!(
                    "reason={}; ready_state={}; lang={:?}; toggle={}; input={}; role_lists={}; rendered_rows={}",
                    self.reason,
                    self.ready_state,
                    self.document_lang,
                    self.viewer_toggle_present,
                    self.viewer_input_present,
                    self.role_lists,
                    self.rendered_row_count
                )))
            }
            value => {
                return Err(BrowserProbeError::UiChanged(format!(
                    "unknown DOM adapter status {value:?}"
                )))
            }
        }
        let mut normalized = BTreeSet::new();
        for raw in self.usernames {
            let login = raw.trim().to_ascii_lowercase();
            if login.is_empty()
                || login.len() > 25
                || !login
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(BrowserProbeError::UiChanged(
                    "viewer button contained an invalid username".into(),
                ));
            }
            normalized.insert(login);
        }
        if normalized.is_empty() {
            return Err(BrowserProbeError::Unavailable(
                "viewer panel rendered no account rows".into(),
            ));
        }
        self.usernames = normalized.into_iter().collect();
        Ok(self)
    }
}

pub fn normalize_channels(input: &[String]) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for raw in input {
        let channel = raw.trim().to_ascii_lowercase();
        if channel.is_empty()
            || channel.len() > 25
            || !channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!("invalid Twitch channel login: {raw:?}"));
        }
        if seen.insert(channel.clone()) {
            result.push(channel);
        }
    }
    if result.len() > 3 {
        return Err("at most 3 distinct channels are allowed in this probe".into());
    }
    Ok(result)
}

pub fn find_browser(explicit: Option<&Path>) -> Result<PathBuf, BrowserProbeError> {
    if let Some(path) = explicit {
        let is_brave = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("brave.exe"));
        return (path.is_file() && is_brave)
            .then(|| path.to_path_buf())
            .ok_or_else(|| {
                BrowserProbeError::Launch(format!(
                    "path is not a Brave Browser executable: {}",
                    path.display()
                ))
            });
    }
    let candidates = [
        std::env::var_os("PROGRAMFILES")
            .map(|p| PathBuf::from(p).join("BraveSoftware/Brave-Browser/Application/brave.exe")),
        std::env::var_os("PROGRAMFILES(X86)")
            .map(|p| PathBuf::from(p).join("BraveSoftware/Brave-Browser/Application/brave.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("BraveSoftware/Brave-Browser/Application/brave.exe")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .ok_or(BrowserProbeError::BrowserNotFound)
}

pub struct BrowserSession {
    child: Child,
    profile: PathBuf,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    #[cfg(windows)]
    job: HANDLE,
}

pub struct BrowserTarget {
    target_id: String,
    session_id: String,
    channel: String,
}

impl BrowserSession {
    pub fn launch(browser: &Path, startup_timeout: Duration) -> Result<Self, BrowserProbeError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let profile = std::env::temp_dir().join(format!(
            "veylurk-browser-probe-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&profile).map_err(|e| BrowserProbeError::Launch(e.to_string()))?;
        let mut command = Command::new(browser);
        command.args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--remote-debugging-address=127.0.0.1",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
        ]);
        command.arg(format!("--user-data-dir={}", profile.display()));
        command.arg("about:blank");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|e| BrowserProbeError::Launch(e.to_string()))?;
        #[cfg(windows)]
        let job = match create_kill_on_close_job(&child) {
            Ok(job) => job,
            Err(error) => {
                cleanup_failed_launch(&mut child, &profile);
                return Err(error);
            }
        };
        let deadline = Instant::now() + startup_timeout;
        let active_port = profile.join("DevToolsActivePort");
        let (port, path) = loop {
            if Instant::now() >= deadline {
                #[cfg(windows)]
                unsafe {
                    TerminateJobObject(job, 1);
                    CloseHandle(job);
                }
                cleanup_failed_launch(&mut child, &profile);
                return Err(BrowserProbeError::Launch(
                    "DevTools endpoint timed out".into(),
                ));
            }
            if let Ok(text) = fs::read_to_string(&active_port) {
                let mut lines = text.lines();
                if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                    break (port.to_owned(), path.to_owned());
                }
            }
            thread::sleep(Duration::from_millis(50));
        };
        let url = format!("ws://127.0.0.1:{port}{path}");
        let (mut socket, _) = match connect(url.as_str()) {
            Ok(result) => result,
            Err(error) => {
                #[cfg(windows)]
                unsafe {
                    TerminateJobObject(job, 1);
                    CloseHandle(job);
                }
                cleanup_failed_launch(&mut child, &profile);
                return Err(BrowserProbeError::Protocol(error.to_string()));
            }
        };
        let timeout_result = match socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream
                .set_read_timeout(Some(Duration::from_secs(25)))
                .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(25)))),
            _ => Err(std::io::Error::other(
                "unexpected TLS on loopback CDP socket",
            )),
        };
        if let Err(error) = timeout_result {
            #[cfg(windows)]
            unsafe {
                TerminateJobObject(job, 1);
                CloseHandle(job);
            }
            cleanup_failed_launch(&mut child, &profile);
            return Err(BrowserProbeError::Protocol(error.to_string()));
        }
        Ok(Self {
            child,
            profile,
            socket,
            next_id: 1,
            #[cfg(windows)]
            job,
        })
    }

    fn command(&mut self, method: &str, params: Value) -> Result<Value, BrowserProbeError> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({"id": id, "method": method, "params": params});
        self.socket
            .send(Message::Text(payload.to_string().into()))
            .map_err(|e| BrowserProbeError::Protocol(e.to_string()))?;
        loop {
            let message = self
                .socket
                .read()
                .map_err(|e| BrowserProbeError::Protocol(e.to_string()))?;
            if message.len() > MAX_CDP_MESSAGE_BYTES {
                return Err(BrowserProbeError::Protocol(
                    "CDP message exceeded 4 MiB".into(),
                ));
            }
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|e| BrowserProbeError::Protocol(e.to_string()))?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(error) = value.get("error") {
                        return Err(BrowserProbeError::Protocol(error.to_string()));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
            }
        }
    }

    fn evaluate(
        &mut self,
        session_id: &str,
        expression: &str,
        await_promise: bool,
    ) -> Result<Value, BrowserProbeError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"id": id, "sessionId": session_id, "method":"Runtime.evaluate", "params": {"expression": expression, "awaitPromise": await_promise, "returnByValue": true}});
        self.socket
            .send(Message::Text(request.to_string().into()))
            .map_err(|error| BrowserProbeError::Protocol(error.to_string()))?;
        loop {
            let message = self
                .socket
                .read()
                .map_err(|error| BrowserProbeError::Protocol(error.to_string()))?;
            if message.len() > MAX_CDP_MESSAGE_BYTES {
                return Err(BrowserProbeError::Protocol(
                    "CDP message exceeded 4 MiB".into(),
                ));
            }
            if let Message::Text(text) = message {
                let response: Value = serde_json::from_str(&text)
                    .map_err(|error| BrowserProbeError::Protocol(error.to_string()))?;
                if response.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = response.get("error") {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown CDP error");
                    return Err(BrowserProbeError::Protocol(format!(
                        "Runtime.evaluate error {code}: {message}"
                    )));
                }
                if let Some(exception) = response.pointer("/result/exceptionDetails") {
                    return Err(BrowserProbeError::UiChanged(bounded_exception_diagnostic(
                        exception,
                    )));
                }
                return response
                    .pointer("/result/result/value")
                    .cloned()
                    .ok_or_else(|| {
                        BrowserProbeError::Protocol("evaluation returned no value".into())
                    });
            }
        }
    }

    fn wait_until_channel_ready(
        &mut self,
        session_id: &str,
        channel: &str,
    ) -> Result<(), BrowserProbeError> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let expected_path = format!("/popout/{channel}/chat");
        let mut last_transient = None;
        while Instant::now() < deadline {
            let state = self.evaluate(
                session_id,
                "({origin:location.origin,path:location.pathname.toLowerCase(),ready:document.readyState})",
                false,
            );
            match state {
                Ok(value) if page_state_is_ready(&value, &expected_path) => {
                    return Ok(());
                }
                Ok(_) => last_transient = Some("page identity or ready state did not match".into()),
                Err(BrowserProbeError::Protocol(message))
                    if is_transient_navigation_error(&message) =>
                {
                    last_transient = Some(message)
                }
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(200));
        }
        Err(BrowserProbeError::UiChanged(format!(
            "channel page did not become ready at {expected_path:?}: {}",
            last_transient.unwrap_or_else(|| "no page state received".into())
        )))
    }

    pub fn open_channel(&mut self, channel: &str) -> Result<BrowserTarget, BrowserProbeError> {
        let url = format!("https://www.twitch.tv/popout/{channel}/chat?popout=");
        let created = self.command("Target.createTarget", json!({"url": url}))?;
        let target_id = created
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserProbeError::Protocol("missing targetId".into()))?
            .to_owned();
        let attached = self.command(
            "Target.attachToTarget",
            json!({"targetId": &target_id, "flatten": true}),
        )?;
        let session_id = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserProbeError::Protocol("missing sessionId".into()))?
            .to_owned();
        if let Err(error) = self.wait_until_channel_ready(&session_id, channel) {
            let _ = self.command("Target.closeTarget", json!({"targetId": &target_id}));
            return Err(error);
        }
        Ok(BrowserTarget {
            target_id,
            session_id,
            channel: channel.to_owned(),
        })
    }

    pub fn collect(&mut self, target: &BrowserTarget) -> Result<DomSample, BrowserProbeError> {
        let channel = &target.channel;
        let expression = dom_adapter_script(channel);
        let result = self.evaluate(&target.session_id, &expression, true)?;
        let sample: DomSample = serde_json::from_value(result)
            .map_err(|e| BrowserProbeError::UiChanged(e.to_string()))?;
        sample.validate(channel)
    }

    pub fn close_channel(&mut self, target: BrowserTarget) {
        let _ = self.command("Target.closeTarget", json!({"targetId": target.target_id}));
    }
}

fn page_state_is_ready(value: &Value, expected_path: &str) -> bool {
    value.get("origin").and_then(Value::as_str) == Some("https://www.twitch.tv")
        && value.get("path").and_then(Value::as_str) == Some(expected_path)
        && matches!(
            value.get("ready").and_then(Value::as_str),
            Some("interactive" | "complete")
        )
}

fn is_transient_navigation_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("execution context")
        || message.contains("cannot find context")
        || message.contains("target navigated")
        || message.contains("evaluation returned no value")
}

fn bounded_exception_diagnostic(exception: &Value) -> String {
    let class_name = exception
        .pointer("/exception/className")
        .and_then(Value::as_str)
        .filter(|value| {
            matches!(
                *value,
                "Error"
                    | "TypeError"
                    | "RangeError"
                    | "ReferenceError"
                    | "SyntaxError"
                    | "DOMException"
            )
        })
        .unwrap_or("JavaScriptError");
    let line = exception.get("lineNumber").and_then(Value::as_u64);
    let column = exception.get("columnNumber").and_then(Value::as_u64);
    match (line, column) {
        (Some(line), Some(column)) => format!(
            "{class_name} at adapter line {} column {}",
            line + 1,
            column + 1
        ),
        _ => class_name.to_owned(),
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            TerminateJobObject(self.job, 1);
            CloseHandle(self.job);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        for _ in 0..10 {
            if fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn cleanup_failed_launch(child: &mut Child, profile: &Path) {
    let _ = child.kill();
    let _ = child.wait();
    for _ in 0..10 {
        if fs::remove_dir_all(profile).is_ok() || !profile.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn create_kill_on_close_job(child: &Child) -> Result<HANDLE, BrowserProbeError> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(BrowserProbeError::Launch(
                "could not create browser process job".into(),
            ));
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
            || AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0
        {
            CloseHandle(job);
            return Err(BrowserProbeError::Launch(
                "could not contain browser process tree in a Windows job".into(),
            ));
        }
        Ok(job)
    }
}

fn dom_adapter_script(channel: &str) -> String {
    let channel = serde_json::to_string(channel).expect("channel serializes");
    format!(
        r#"(async () => {{
      const expected = {channel};
      const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
      const deadline = Date.now() + 20000;
      const viewerInput = () => document.querySelector('input[aria-label="Search Chat Viewers"]');
      const rowSelector = 'button[data-test-selector="chat-viewers-list__button"][data-username]';
      const diagnosticResult = (status, reason, usernames = [], extra = {{}}) => ({{
        channel:(location.pathname.match(/^\/popout\/([^/]+)\/chat/i) || [,''])[1].toLowerCase(),
        origin:location.origin,
        status,
        reason,
        usernames,
        role_lists:document.querySelectorAll('[aria-labelledby^="chat-viewers-list-header-"]').length,
        scroll_rounds:0,
        reached_end:false,
        ready_state:document.readyState,
        document_lang:String(document.documentElement?.lang || '').slice(0, 24),
        known_error_title_present:/access denied|verify you are human|unusual traffic/i.test(document.title),
        viewer_toggle_present:Boolean(document.querySelector('button[data-test-selector="chat-viewer-list"]')),
        viewer_input_present:Boolean(viewerInput()),
        rendered_row_count:document.querySelectorAll(rowSelector).length,
        login_prompt_present:Boolean(document.querySelector('button[data-a-target="login-button"], a[data-a-target="login-button"]')),
        ...extra,
      }});
      const closeViewerPanel = async () => {{
        const input = viewerInput();
        if (!input) return true;
        const close = input.closest('.chat-viewers__pane')?.querySelector('button[aria-label="Close"][data-a-target="chat-viewer-list"]');
        const back = [...document.querySelectorAll('button')].find(el => /^go back to chat$/i.test(el.getAttribute('aria-label') || el.textContent.trim()));
        if (close) close.click(); else if (back) back.click(); else document.querySelector('button[data-test-selector="chat-viewer-list"]')?.click();
        const closeDeadline = Date.now() + 2000;
        while (viewerInput() && Date.now() < closeDeadline) await sleep(50);
        return !viewerInput();
      }};
      while (document.readyState !== 'complete' && Date.now() < deadline) await sleep(100);
      if (!await closeViewerPanel()) return diagnosticResult('ui_changed', 'stale_panel_close_failed');
      while (Date.now() < deadline) {{
        const challengeTitle = /access denied|verify you are human|unusual traffic/i.test(document.title);
        const challengeElement = document.querySelector('iframe[src*="captcha" i], iframe[src*="challenge" i], [data-a-target*="captcha" i], form[action*="challenge" i]');
        if (challengeTitle || challengeElement) return diagnosticResult('challenge', 'challenge_indicator');
        const button = document.querySelector('button[data-test-selector="chat-viewer-list"]');
        if (button) {{ button.click(); break; }}
        await sleep(200);
      }}
      let input = null;
      while (Date.now() < deadline) {{
        input = viewerInput();
        if (input) break;
        await sleep(150);
      }}
      if (!input) return diagnosticResult('unavailable', document.querySelector('button[data-test-selector="chat-viewer-list"]') ? 'panel_input_timeout' : 'viewer_toggle_missing');
      let panelReopens = 0;
      let reopenedInputObserved = false;
      while (!document.querySelector(rowSelector) && Date.now() < deadline) {{
        const challengeTitle = /access denied|verify you are human|unusual traffic/i.test(document.title);
        const challengeElement = document.querySelector('iframe[src*="captcha" i], iframe[src*="challenge" i], [data-a-target*="captcha" i], form[action*="challenge" i]');
        if (challengeTitle || challengeElement) return diagnosticResult('challenge', 'challenge_indicator');
        const inputVisible = Boolean(viewerInput());
        if (!inputVisible && panelReopens === 0) {{
          const button = document.querySelector('button[data-test-selector="chat-viewer-list"]');
          if (!button) return diagnosticResult('unavailable', 'viewer_toggle_missing_after_panel_disappeared');
          button.click(); panelReopens += 1;
        }} else if (inputVisible && panelReopens === 1) {{
          reopenedInputObserved = true;
        }} else if (!inputVisible && reopenedInputObserved) {{
          return diagnosticResult('unavailable', 'panel_disappeared_after_reopen');
        }}
        await sleep(150);
      }}
      if (!document.querySelector(rowSelector)) return diagnosticResult('unavailable', 'viewer_rows_timeout');
      const found = new Set(); let rounds = 0; let unchanged = 0; let reachedEnd = false;
      while (rounds < 40 && unchanged < 3 && Date.now() < deadline) {{
        const before = found.size;
        document.querySelectorAll(rowSelector).forEach(el => found.add(el.dataset.username));
        const lists = [...document.querySelectorAll('[aria-labelledby^="chat-viewers-list-header-"]')];
        const scrollables = [...new Set(lists.map(list => (() => {{ let n=list; while(n && n !== document.body) {{ if(n.scrollHeight > n.clientHeight + 2) return n; n=n.parentElement; }} return list; }})()))];
        for (const scroller of scrollables) {{
          const prior = scroller.scrollTop; scroller.scrollTop = Math.min(scroller.scrollTop + Math.max(scroller.clientHeight * .8, 300), scroller.scrollHeight);
        }}
        reachedEnd = scrollables.length > 0 && scrollables.every(scroller => scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
        unchanged = found.size === before ? unchanged + 1 : 0; rounds += 1; await sleep(100);
      }}
      document.querySelectorAll(rowSelector).forEach(el => found.add(el.dataset.username));
      const validLocation = location.protocol === 'https:' && location.hostname === 'www.twitch.tv';
      const actual = (location.pathname.match(/^\/popout\/([^/]+)\/chat/i) || [,''])[1].toLowerCase();
      if (!validLocation) return diagnosticResult('wrong_origin', 'unexpected_origin', [], {{scroll_rounds:rounds}});
      const roleLists = document.querySelectorAll('[aria-labelledby^="chat-viewers-list-header-"]').length;
      const renderedRows = document.querySelectorAll(rowSelector).length;
      if (!await closeViewerPanel()) return diagnosticResult('ui_changed', 'sample_panel_close_failed', [], {{role_lists:roleLists, scroll_rounds:rounds, reached_end:reachedEnd, rendered_row_count:renderedRows}});
      return diagnosticResult('ok', 'sample_complete', [...found], {{role_lists:roleLists, scroll_rounds:rounds, reached_end:reachedEnd, rendered_row_count:renderedRows}});
    }})()"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channels_are_validated_deduplicated_and_bounded() {
        assert_eq!(
            normalize_channels(&["OuaisEddy".into(), "ouaiseddy".into(), "dofla".into()]).unwrap(),
            ["ouaiseddy", "dofla"]
        );
        assert!(normalize_channels(&["bad/name".into()]).is_err());
        assert!(normalize_channels(&["a".into(), "b".into(), "c".into(), "d".into()]).is_err());
    }
    #[test]
    fn sample_validation_deduplicates_and_scopes() {
        let sample = DomSample {
            channel: "Test".into(),
            origin: "https://www.twitch.tv".into(),
            status: "ok".into(),
            reason: "sample_complete".into(),
            usernames: vec!["Alice".into(), "alice".into(), "Bob_2".into()],
            role_lists: 1,
            scroll_rounds: 2,
            reached_end: true,
            ready_state: "complete".into(),
            document_lang: "en".into(),
            known_error_title_present: false,
            viewer_toggle_present: true,
            viewer_input_present: false,
            rendered_row_count: 3,
            login_prompt_present: false,
        }
        .validate("test")
        .unwrap();
        assert_eq!(sample.usernames, ["alice", "bob_2"]);
        assert!(matches!(
            DomSample {
                channel: "other".into(),
                origin: "https://www.twitch.tv".into(),
                status: "ok".into(),
                reason: "sample_complete".into(),
                usernames: vec!["a".into()],
                role_lists: 1,
                scroll_rounds: 1,
                reached_end: true,
                ready_state: "complete".into(),
                document_lang: "en".into(),
                known_error_title_present: false,
                viewer_toggle_present: true,
                viewer_input_present: false,
                rendered_row_count: 1,
                login_prompt_present: false,
            }
            .validate("test"),
            Err(BrowserProbeError::ChannelMismatch { .. })
        ));
    }
    #[test]
    fn explicit_challenge_and_empty_panel_are_not_success() {
        let challenge = DomSample {
            channel: "x".into(),
            origin: "https://www.twitch.tv".into(),
            status: "challenge".into(),
            reason: "challenge_indicator".into(),
            usernames: vec![],
            role_lists: 0,
            scroll_rounds: 0,
            reached_end: false,
            ready_state: "complete".into(),
            document_lang: "en".into(),
            known_error_title_present: true,
            viewer_toggle_present: false,
            viewer_input_present: false,
            rendered_row_count: 0,
            login_prompt_present: false,
        };
        assert!(matches!(
            challenge.validate("x"),
            Err(BrowserProbeError::Challenge)
        ));
        let empty = DomSample {
            channel: "x".into(),
            origin: "https://www.twitch.tv".into(),
            status: "ok".into(),
            reason: "sample_complete".into(),
            usernames: vec![],
            role_lists: 1,
            scroll_rounds: 3,
            reached_end: true,
            ready_state: "complete".into(),
            document_lang: "en".into(),
            known_error_title_present: false,
            viewer_toggle_present: true,
            viewer_input_present: false,
            rendered_row_count: 0,
            login_prompt_present: false,
        };
        assert!(matches!(
            empty.validate("x"),
            Err(BrowserProbeError::Unavailable(_))
        ));
    }

    #[test]
    fn wrong_origin_and_missing_evidence_fields_fail_closed() {
        let wrong_origin = DomSample {
            channel: "x".into(),
            origin: "https://example.com".into(),
            status: "ok".into(),
            reason: "sample_complete".into(),
            usernames: vec!["alice".into()],
            role_lists: 1,
            scroll_rounds: 1,
            reached_end: true,
            ready_state: "complete".into(),
            document_lang: "en".into(),
            known_error_title_present: false,
            viewer_toggle_present: true,
            viewer_input_present: false,
            rendered_row_count: 1,
            login_prompt_present: false,
        };
        assert!(matches!(
            wrong_origin.validate("x"),
            Err(BrowserProbeError::UiChanged(_))
        ));
        let incomplete = r#"{"channel":"x","origin":"https://www.twitch.tv","status":"ok","usernames":["alice"]}"#;
        assert!(serde_json::from_str::<DomSample>(incomplete).is_err());
    }

    #[test]
    fn page_readiness_requires_exact_twitch_identity() {
        let ready =
            json!({"origin":"https://www.twitch.tv","path":"/popout/test/chat","ready":"complete"});
        assert!(page_state_is_ready(&ready, "/popout/test/chat"));
        assert!(!page_state_is_ready(
            &json!({"origin":"https://example.com","path":"/popout/test/chat","ready":"complete"}),
            "/popout/test/chat"
        ));
        assert!(!page_state_is_ready(&ready, "/popout/other/chat"));
        assert!(is_transient_navigation_error(
            "Execution context was destroyed"
        ));
        assert!(!is_transient_navigation_error("permission denied"));
    }

    #[test]
    fn exception_diagnostic_is_bounded_and_omits_page_text() {
        let exception = json!({
            "text": "Uncaught",
            "lineNumber": 41,
            "columnNumber": 7,
            "exception": {
                "className": "TypeError",
                "description": "TypeError: secret-user secret chat body token"
            }
        });
        assert_eq!(
            bounded_exception_diagnostic(&exception),
            "TypeError at adapter line 42 column 8"
        );
    }

    #[test]
    fn generated_dom_adapter_executes_against_minimal_panel_fixture() {
        let node_version = Command::new("node")
            .arg("--version")
            .output()
            .expect("Node.js is required to test the generated DOM adapter");
        assert!(node_version.status.success(), "Node.js did not start");
        let node = "node";
        let expression = dom_adapter_script("test_channel");
        let expression_json = serde_json::to_string(&expression).unwrap();
        let fixture = format!(
            r#"
let panelOpen = false;
let rowsReady = false;
let panelOpens = 0;
const close = {{ click() {{ panelOpen = false; }} }};
const pane = {{ querySelector() {{ return close; }} }};
const input = {{ closest() {{ return pane; }} }};
const toggle = {{
  click() {{
    panelOpens += 1;
    if (panelOpens === 1) {{
      panelOpen = true;
      setTimeout(() => {{ panelOpen = false; }}, 25);
    }} else {{
      setTimeout(() => {{ panelOpen = true; }}, 250);
      setTimeout(() => {{ rowsReady = true; }}, 450);
    }}
  }},
  getAttribute(name) {{ return name === 'aria-label' ? 'Viewers' : null; }},
  textContent: 'Viewers'
}};
const row = {{ dataset: {{ username: 'Alice_1' }} }};
const roleList = {{ scrollHeight: 100, clientHeight: 100, scrollTop: 0, parentElement: null }};
global.location = {{ origin: 'https://www.twitch.tv', protocol: 'https:', hostname: 'www.twitch.tv', pathname: '/popout/test_channel/chat' }};
global.document = {{
  readyState: 'complete', title: '', documentElement: {{ lang: 'en-US' }}, body: {{}},
  querySelector(selector) {{
    if (selector === 'input[aria-label="Search Chat Viewers"]') return panelOpen ? input : null;
    if (selector === 'button[data-test-selector="chat-viewer-list"]') return toggle;
    if (selector === 'button[data-test-selector="chat-viewers-list__button"][data-username]') return panelOpen && rowsReady ? row : null;
    return null;
  }},
  querySelectorAll(selector) {{
    if (selector === 'button') return [toggle];
    if (selector === '[aria-labelledby^="chat-viewers-list-header-"]') return panelOpen ? [roleList] : [];
    if (selector === 'button[data-test-selector="chat-viewers-list__button"][data-username]') return panelOpen && rowsReady ? [row] : [];
    return [];
  }}
}};
roleList.parentElement = document.body;
Promise.resolve(eval({expression_json})).then(value => process.stdout.write(JSON.stringify({{ value, panelOpens }}))).catch(error => {{ console.error(error); process.exit(1); }});
"#
        );
        let output = Command::new(node).arg("-e").arg(fixture).output().unwrap();
        assert!(
            output.status.success(),
            "generated adapter failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.get("panelOpens").and_then(Value::as_u64), Some(2));
        let sample: DomSample = serde_json::from_value(result["value"].clone()).unwrap();
        let sample = sample.validate("test_channel").unwrap();
        assert_eq!(sample.usernames, ["alice_1"]);
        assert_eq!(sample.reason, "sample_complete");
        assert_eq!(sample.rendered_row_count, 1);
    }
}
