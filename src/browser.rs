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
    #[error("no supported browser was found (Microsoft Edge or Google Chrome)")]
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
    pub usernames: Vec<String>,
    pub role_lists: usize,
    pub scroll_rounds: usize,
    pub reached_end: bool,
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
                return Err(BrowserProbeError::Unavailable(
                    "viewer list was not offered by the ordinary UI".into(),
                ))
            }
            "ok" => {}
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
        return path.is_file().then(|| path.to_path_buf()).ok_or_else(|| {
            BrowserProbeError::Launch(format!("browser does not exist: {}", path.display()))
        });
    }
    let candidates = [
        std::env::var_os("PROGRAMFILES(X86)")
            .map(|p| PathBuf::from(p).join("Microsoft/Edge/Application/msedge.exe")),
        std::env::var_os("PROGRAMFILES")
            .map(|p| PathBuf::from(p).join("Microsoft/Edge/Application/msedge.exe")),
        std::env::var_os("PROGRAMFILES")
            .map(|p| PathBuf::from(p).join("Google/Chrome/Application/chrome.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("Google/Chrome/Application/chrome.exe")),
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
        Ok(BrowserTarget {
            target_id,
            session_id,
            channel: channel.to_owned(),
        })
    }

    pub fn collect(&mut self, target: &BrowserTarget) -> Result<DomSample, BrowserProbeError> {
        let channel = &target.channel;
        let expression = dom_adapter_script(channel);
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"id": id, "sessionId": &target.session_id, "method":"Runtime.evaluate", "params": {"expression": expression, "awaitPromise": true, "returnByValue": true}});
        self.socket
            .send(Message::Text(request.to_string().into()))
            .map_err(|e| BrowserProbeError::Protocol(e.to_string()))?;
        let value = loop {
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
                    break value;
                }
            }
        };
        if let Some(exception) = value.pointer("/result/exceptionDetails") {
            return Err(BrowserProbeError::UiChanged(exception.to_string()));
        }
        let result = value
            .pointer("/result/result/value")
            .cloned()
            .ok_or_else(|| BrowserProbeError::Protocol("evaluation returned no value".into()))?;
        let sample: DomSample = serde_json::from_value(result)
            .map_err(|e| BrowserProbeError::UiChanged(e.to_string()))?;
        sample.validate(channel)
    }

    pub fn close_channel(&mut self, target: BrowserTarget) {
        let _ = self.command("Target.closeTarget", json!({"targetId": target.target_id}));
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
      while (document.readyState !== 'complete' && Date.now() < deadline) await sleep(100);
      while (Date.now() < deadline) {{
        const challengeTitle = /access denied|verify you are human|unusual traffic/i.test(document.title);
        const challengeElement = document.querySelector('iframe[src*="captcha" i], iframe[src*="challenge" i], [data-a-target*="captcha" i], form[action*="challenge" i]');
        if (challengeTitle || challengeElement) return {{channel: expected, origin:location.origin, status:'challenge', usernames:[], role_lists:0, scroll_rounds:0, reached_end:false}};
        const button = document.querySelector('button[data-test-selector="chat-viewer-list"]');
        if (button) {{ button.click(); break; }}
        await sleep(200);
      }}
      let input = null;
      while (Date.now() < deadline) {{
        input = document.querySelector('input[aria-label="Search Chat Viewers"]');
        if (input) break;
        await sleep(150);
      }}
      if (!input) return {{channel: expected, origin:location.origin, status:'unavailable', usernames:[], role_lists:0, scroll_rounds:0, reached_end:false}};
      const found = new Set(); let rounds = 0; let unchanged = 0; let reachedEnd = false;
      while (rounds < 40 && unchanged < 3 && Date.now() < deadline) {{
        const before = found.size;
        document.querySelectorAll('button[data-test-selector="chat-viewers-list__button"][data-username]').forEach(el => found.add(el.dataset.username));
        const lists = [...document.querySelectorAll('[aria-labelledby^="chat-viewers-list-header-"]')];
        const scrollables = [...new Set(lists.map(list => (() => {{ let n=list; while(n && n !== document.body) {{ if(n.scrollHeight > n.clientHeight + 2) return n; n=n.parentElement; }} return list; }})()))];
        for (const scroller of scrollables) {{
          const prior = scroller.scrollTop; scroller.scrollTop = Math.min(scroller.scrollTop + Math.max(scroller.clientHeight * .8, 300), scroller.scrollHeight);
        }}
        reachedEnd = scrollables.length > 0 && scrollables.every(scroller => scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2);
        unchanged = found.size === before ? unchanged + 1 : 0; rounds += 1; await sleep(100);
      }}
      document.querySelectorAll('button[data-test-selector="chat-viewers-list__button"][data-username]').forEach(el => found.add(el.dataset.username));
      const validLocation = location.protocol === 'https:' && location.hostname === 'www.twitch.tv';
      const actual = (location.pathname.match(/^\/popout\/([^/]+)\/chat/i) || [,''])[1].toLowerCase();
      if (!validLocation) return {{channel: actual, origin:location.origin, status:'wrong_origin', usernames:[], role_lists:0, scroll_rounds:rounds, reached_end:false}};
      const back = [...document.querySelectorAll('button')].find(el => /^go back to chat$/i.test(el.getAttribute('aria-label') || el.textContent.trim()));
      if (back) back.click(); else document.querySelector('button[data-test-selector="chat-viewer-list"]')?.click();
      return {{channel: actual, origin:location.origin, status:'ok', usernames:[...found], role_lists:document.querySelectorAll('[aria-labelledby^="chat-viewers-list-header-"]').length, scroll_rounds:rounds, reached_end:reachedEnd}};
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
            usernames: vec!["Alice".into(), "alice".into(), "Bob_2".into()],
            role_lists: 1,
            scroll_rounds: 2,
            reached_end: true,
        }
        .validate("test")
        .unwrap();
        assert_eq!(sample.usernames, ["alice", "bob_2"]);
        assert!(matches!(
            DomSample {
                channel: "other".into(),
                origin: "https://www.twitch.tv".into(),
                status: "ok".into(),
                usernames: vec!["a".into()],
                role_lists: 1,
                scroll_rounds: 1,
                reached_end: true
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
            usernames: vec![],
            role_lists: 0,
            scroll_rounds: 0,
            reached_end: false,
        };
        assert!(matches!(
            challenge.validate("x"),
            Err(BrowserProbeError::Challenge)
        ));
        let empty = DomSample {
            channel: "x".into(),
            origin: "https://www.twitch.tv".into(),
            status: "ok".into(),
            usernames: vec![],
            role_lists: 1,
            scroll_rounds: 3,
            reached_end: true,
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
            usernames: vec!["alice".into()],
            role_lists: 1,
            scroll_rounds: 1,
            reached_end: true,
        };
        assert!(matches!(
            wrong_origin.validate("x"),
            Err(BrowserProbeError::UiChanged(_))
        ));
        let incomplete = r#"{"channel":"x","origin":"https://www.twitch.tv","status":"ok","usernames":["alice"]}"#;
        assert!(serde_json::from_str::<DomSample>(incomplete).is_err());
    }
}
