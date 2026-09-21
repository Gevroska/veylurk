use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

const PROTOCOL_VERSION: u8 = 1;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_REPLY_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BrowserProbeError {
    #[error("Node.js or the browser helper could not start: {0}")]
    Launch(String),
    #[error("browser challenge detected; stopped without attempting to solve it")]
    Challenge,
    #[error("Twitch chat UI is unavailable: {0}")]
    Unavailable(String),
    #[error("Twitch UI changed or did not become ready: {0}")]
    UiChanged(String),
    #[error("browser helper protocol failed: {0}")]
    Protocol(String),
    #[error("page channel {actual:?} did not match requested channel {requested:?}")]
    ChannelMismatch { requested: String, actual: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
            return Err(BrowserProbeError::UiChanged(
                "unexpected page origin".into(),
            ));
        }
        if self.status != "ok" || self.reason != "sample_complete" {
            return Err(BrowserProbeError::UiChanged(
                "browser helper returned an invalid sample status".into(),
            ));
        }
        if self.document_lang.len() > 24
            || !matches!(self.ready_state.as_str(), "interactive" | "complete")
            || self.known_error_title_present
            || !self.viewer_toggle_present
            || self.role_lists == 0
            || self.rendered_row_count == 0
            || self.scroll_rounds > 40
        {
            return Err(BrowserProbeError::UiChanged(
                "browser helper returned inconsistent DOM evidence".into(),
            ));
        }
        let mut normalized = BTreeSet::new();
        for raw in self.usernames {
            let login = raw.trim().to_ascii_lowercase();
            if login.is_empty()
                || login.len() > 25
                || !login
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
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
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!("invalid Twitch channel login: {raw:?}"));
        }
        if seen.insert(channel.clone()) {
            result.push(channel);
        }
    }
    if result.is_empty() || result.len() > 3 {
        return Err("between 1 and 3 distinct channels are required".into());
    }
    Ok(result)
}

pub fn find_node_and_helper(
    node_override: Option<&Path>,
    helper_override: Option<&Path>,
) -> Result<(PathBuf, PathBuf), BrowserProbeError> {
    let executable = std::env::current_exe().map_err(|error| {
        BrowserProbeError::Launch(format!("could not locate the probe executable: {error}"))
    })?;
    let directory = executable
        .parent()
        .ok_or_else(|| BrowserProbeError::Launch("probe executable has no parent folder".into()))?;
    let bundled_node = directory.join("node.exe");
    let node = match node_override {
        Some(path) if path.is_file() => path.to_path_buf(),
        Some(_) => {
            return Err(BrowserProbeError::Launch(
                "--node-path is not a file".into(),
            ))
        }
        None if bundled_node.is_file() => bundled_node,
        None => PathBuf::from("node"),
    };
    let helper = helper_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| directory.join("browser-helper").join("helper.mjs"));
    if !helper.is_file() {
        return Err(BrowserProbeError::Launch(format!(
            "browser helper missing at {}; restore the complete Windows artifact",
            helper.display()
        )));
    }
    Ok((node, helper))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    v: u8,
    id: u64,
    ok: bool,
    ready: Option<bool>,
    sample: Option<DomSample>,
    error: Option<HelperFailure>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperFailure {
    code: FailureCode,
    reason: FailureReason,
    phase: FailurePhase,
    error_class: ErrorClass,
    network_code: Option<String>,
    page_error_class: Option<ErrorClass>,
    page_error_count: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailurePhase {
    Launch,
    Navigation,
    Panel,
    Rows,
}

impl FailurePhase {
    fn label(&self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Navigation => "navigation",
            Self::Panel => "panel",
            Self::Rows => "rows",
        }
    }
}

#[derive(Debug, Deserialize)]
enum ErrorClass {
    TypeError,
    ReferenceError,
    SyntaxError,
    RangeError,
    SecurityError,
    NetworkError,
    TimeoutError,
    OtherError,
}

impl ErrorClass {
    fn label(&self) -> &'static str {
        match self {
            Self::TypeError => "TypeError",
            Self::ReferenceError => "ReferenceError",
            Self::SyntaxError => "SyntaxError",
            Self::RangeError => "RangeError",
            Self::SecurityError => "SecurityError",
            Self::NetworkError => "NetworkError",
            Self::TimeoutError => "TimeoutError",
            Self::OtherError => "OtherError",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailureCode {
    Challenge,
    Unavailable,
    UiChanged,
    Protocol,
    BrowserLaunch,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailureReason {
    ChallengeIndicator,
    ViewerToggleMissing,
    ViewerToggleMissingAfterPanelDisappeared,
    StalePanelCloseFailed,
    PanelDisappearedAfterReopen,
    ViewerRowsTimeout,
    PanelInputTimeout,
    InvalidUsername,
    SamplePanelCloseFailed,
    UnexpectedOrigin,
    UnexpectedChannel,
    BrowserOperationFailed,
}

impl FailureReason {
    fn label(&self) -> &'static str {
        match self {
            Self::ChallengeIndicator => "challenge_indicator",
            Self::ViewerToggleMissing => "viewer_toggle_missing",
            Self::ViewerToggleMissingAfterPanelDisappeared => {
                "viewer_toggle_missing_after_panel_disappeared"
            }
            Self::StalePanelCloseFailed => "stale_panel_close_failed",
            Self::PanelDisappearedAfterReopen => "panel_disappeared_after_reopen",
            Self::ViewerRowsTimeout => "viewer_rows_timeout",
            Self::PanelInputTimeout => "panel_input_timeout",
            Self::InvalidUsername => "invalid_username",
            Self::SamplePanelCloseFailed => "sample_panel_close_failed",
            Self::UnexpectedOrigin => "unexpected_origin",
            Self::UnexpectedChannel => "unexpected_channel",
            Self::BrowserOperationFailed => "browser_operation_failed",
        }
    }
}

fn read_bounded_line<R: Read>(reader: &mut R) -> Result<Vec<u8>, BrowserProbeError> {
    let mut line = Vec::new();
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return Err(BrowserProbeError::Protocol(
                    "browser helper closed its response stream".into(),
                ))
            }
            Ok(_) if byte[0] == b'\n' => return Ok(line),
            Ok(_) if line.len() >= MAX_REPLY_BYTES => {
                return Err(BrowserProbeError::Protocol(
                    "browser helper response exceeded 1 MiB".into(),
                ))
            }
            Ok(_) => line.push(byte[0]),
            Err(_) => {
                return Err(BrowserProbeError::Protocol(
                    "could not read browser helper response".into(),
                ))
            }
        }
    }
}

fn decode_reply(line: &[u8], expected_id: u64) -> Result<Reply, BrowserProbeError> {
    let reply: Reply = serde_json::from_slice(line).map_err(|_| {
        BrowserProbeError::Protocol("browser helper sent invalid JSON/schema".into())
    })?;
    if reply.v != PROTOCOL_VERSION || reply.id != expected_id {
        return Err(BrowserProbeError::Protocol(
            "browser helper version or request ID mismatch".into(),
        ));
    }
    if reply.ok && reply.error.is_some()
        || !reply.ok && (reply.error.is_none() || reply.sample.is_some())
    {
        return Err(BrowserProbeError::Protocol(
            "browser helper response fields were inconsistent".into(),
        ));
    }
    if let Some(failure) = &reply.error {
        if failure.page_error_count > 1000
            || failure.network_code.as_ref().is_some_and(|code| {
                !code.starts_with("net::ERR_")
                    || code.len() <= 9
                    || code.len() > 57
                    || !code[9..]
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
            })
        {
            return Err(BrowserProbeError::Protocol(
                "browser helper failure diagnostic was invalid".into(),
            ));
        }
    }
    Ok(reply)
}

fn classify_failure(failure: HelperFailure) -> BrowserProbeError {
    let mut detail = format!(
        "phase={} reason={} class={}",
        failure.phase.label(),
        failure.reason.label(),
        failure.error_class.label()
    );
    if let Some(code) = failure.network_code {
        detail.push_str(&format!(" network={code}"));
    }
    if failure.page_error_count > 0 {
        detail.push_str(&format!(" page_errors={}", failure.page_error_count));
        if let Some(class) = failure.page_error_class {
            detail.push_str(&format!(" page_class={}", class.label()));
        }
    }
    match failure.code {
        FailureCode::Challenge => BrowserProbeError::Challenge,
        FailureCode::Unavailable => BrowserProbeError::Unavailable(detail),
        FailureCode::UiChanged => BrowserProbeError::UiChanged(detail),
        FailureCode::Protocol => BrowserProbeError::Protocol(detail),
        FailureCode::BrowserLaunch => BrowserProbeError::Launch(detail),
    }
}

pub struct BrowserSession {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Result<Vec<u8>, BrowserProbeError>>,
    next_id: u64,
    #[cfg(windows)]
    job: HANDLE,
}

impl BrowserSession {
    pub fn launch(
        node: &Path,
        helper: &Path,
        channels: &[String],
    ) -> Result<Self, BrowserProbeError> {
        let mut command = Command::new(node);
        command
            .arg(helper)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(parent) = helper.parent() {
            command.current_dir(parent);
            if std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").is_none() {
                if let Some(bundle_root) = parent.parent() {
                    command.env("PLAYWRIGHT_BROWSERS_PATH", bundle_root.join("browsers"));
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|error| {
            BrowserProbeError::Launch(format!("could not start Node.js: {error}"))
        })?;
        let (stdin, stdout) = match (child.stdin.take(), child.stdout.take()) {
            (Some(stdin), Some(stdout)) => (stdin, stdout),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(BrowserProbeError::Protocol("missing helper pipe".into()));
            }
        };
        #[cfg(windows)]
        let job = match create_kill_on_close_job(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let (sender, replies) = mpsc::sync_channel(2);
        thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let line = read_bounded_line(&mut stdout);
                let stop = line.is_err();
                if sender.send(line).is_err() || stop {
                    break;
                }
            }
        });
        let mut session = Self {
            child,
            stdin,
            replies,
            next_id: 1,
            #[cfg(windows)]
            job,
        };
        let ready = session.exchange(
            "init",
            json!({"channels": channels}),
            Duration::from_secs(65),
        )?;
        if !ready.ok {
            return Err(classify_failure(
                ready.error.expect("checked response shape"),
            ));
        }
        if ready.ready != Some(true) || ready.sample.is_some() {
            return Err(BrowserProbeError::Protocol(
                "invalid helper init response".into(),
            ));
        }
        Ok(session)
    }

    fn exchange(
        &mut self,
        operation: &str,
        fields: Value,
        timeout: Duration,
    ) -> Result<Reply, BrowserProbeError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({"v": PROTOCOL_VERSION, "id": id, "op": operation});
        if let (Some(object), Some(extra)) = (request.as_object_mut(), fields.as_object()) {
            object.extend(extra.clone());
        }
        let mut encoded = serde_json::to_vec(&request).expect("protocol request serializes");
        if encoded.len() > MAX_REQUEST_BYTES {
            return Err(BrowserProbeError::Protocol(
                "browser helper request exceeded 64 KiB".into(),
            ));
        }
        encoded.push(b'\n');
        self.stdin
            .write_all(&encoded)
            .and_then(|_| self.stdin.flush())
            .map_err(|_| {
                BrowserProbeError::Protocol("could not write browser helper request".into())
            })?;
        let line = match self.replies.recv_timeout(timeout) {
            Ok(result) => result?,
            Err(RecvTimeoutError::Timeout) => {
                return Err(BrowserProbeError::Protocol(
                    "browser helper response timed out".into(),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(BrowserProbeError::Protocol(
                    "browser helper stopped responding".into(),
                ))
            }
        };
        decode_reply(&line, id)
    }

    pub fn collect(
        &mut self,
        channel: &str,
        failure_screenshot: Option<&Path>,
    ) -> Result<DomSample, BrowserProbeError> {
        let reply = self.exchange(
            "collect",
            json!({"channel": channel, "failure_screenshot": failure_screenshot}),
            Duration::from_secs(25),
        )?;
        if !reply.ok {
            return Err(classify_failure(
                reply.error.expect("checked response shape"),
            ));
        }
        if reply.ready.is_some() {
            return Err(BrowserProbeError::Protocol(
                "invalid helper collect response".into(),
            ));
        }
        reply
            .sample
            .ok_or_else(|| {
                BrowserProbeError::Protocol("helper omitted a successful sample".into())
            })?
            .validate(channel)
    }

    pub fn shutdown(&mut self) {
        let _ = self.exchange("shutdown", json!({}), Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
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
                "could not contain browser helper process tree".into(),
            ));
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn channels_are_validated_deduplicated_and_bounded() {
        assert_eq!(
            normalize_channels(&["OuaisEddy".into(), "ouaiseddy".into(), "dofla".into()]).unwrap(),
            ["ouaiseddy", "dofla"]
        );
        assert!(normalize_channels(&["bad/name".into()]).is_err());
        assert!(normalize_channels(&[]).is_err());
        assert!(normalize_channels(&["a".into(), "b".into(), "c".into(), "d".into()]).is_err());
    }

    #[test]
    fn protocol_rejects_wrong_id_unknown_failure_and_oversized_line() {
        let wrong_id = br#"{"v":1,"id":2,"ok":true,"ready":true}"#;
        assert!(matches!(
            decode_reply(wrong_id, 1),
            Err(BrowserProbeError::Protocol(_))
        ));
        let unknown_failure = br#"{"v":1,"id":1,"ok":false,"error":{"code":"unknown","reason":"browser_operation_failed","phase":"panel","error_class":"OtherError","network_code":null,"page_error_class":null,"page_error_count":0}}"#;
        assert!(matches!(
            decode_reply(unknown_failure, 1),
            Err(BrowserProbeError::Protocol(_))
        ));
        let unsafe_code = br#"{"v":1,"id":1,"ok":false,"error":{"code":"ui_changed","reason":"browser_operation_failed","phase":"navigation","error_class":"OtherError","network_code":"net::ERR_FAILED user data","page_error_class":null,"page_error_count":0}}"#;
        assert!(matches!(
            decode_reply(unsafe_code, 1),
            Err(BrowserProbeError::Protocol(_))
        ));
        let safe_code = br#"{"v":1,"id":1,"ok":false,"error":{"code":"browser_launch","reason":"browser_operation_failed","phase":"navigation","error_class":"OtherError","network_code":"net::ERR_BLOCKED_BY_CLIENT","page_error_class":null,"page_error_count":0}}"#;
        let error = classify_failure(decode_reply(safe_code, 1).unwrap().error.unwrap());
        assert!(error.to_string().contains("phase=navigation"));
        assert!(error.to_string().contains("net::ERR_BLOCKED_BY_CLIENT"));
        let mut oversized = Cursor::new(vec![b'x'; MAX_REPLY_BYTES + 2]);
        assert!(matches!(
            read_bounded_line(&mut oversized),
            Err(BrowserProbeError::Protocol(_))
        ));
    }

    #[test]
    fn sample_requires_exact_identity_and_rendered_evidence() {
        let sample = json!({
            "channel":"test", "origin":"https://www.twitch.tv", "status":"ok", "reason":"sample_complete",
            "usernames":["Alice", "alice", "Bob_2"], "role_lists":1, "scroll_rounds":2,
            "reached_end":true, "ready_state":"complete", "document_lang":"en-US",
            "known_error_title_present":false, "viewer_toggle_present":true,
            "viewer_input_present":false, "rendered_row_count":3, "login_prompt_present":false
        });
        let validated = serde_json::from_value::<DomSample>(sample.clone())
            .unwrap()
            .validate("test")
            .unwrap();
        assert_eq!(validated.usernames, ["alice", "bob_2"]);
        assert!(matches!(
            serde_json::from_value::<DomSample>(sample.clone())
                .unwrap()
                .validate("other"),
            Err(BrowserProbeError::ChannelMismatch { .. })
        ));
        let mut wrong_origin = sample;
        wrong_origin["origin"] = json!("https://example.com");
        assert!(matches!(
            serde_json::from_value::<DomSample>(wrong_origin)
                .unwrap()
                .validate("test"),
            Err(BrowserProbeError::UiChanged(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn node_helper_handshake_and_repeated_samples_use_bounded_protocol() {
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("browser-helper")
            .join("fixtures")
            .join("protocol-stub.mjs");
        let mut session = BrowserSession::launch(Path::new("node"), &helper, &["test".into()])
            .expect("Node.js is required for the Stage 3 protocol test");
        assert_eq!(
            session.collect("test", None).unwrap().usernames,
            ["alice_1"]
        );
        assert_eq!(
            session.collect("test", None).unwrap().usernames,
            ["alice_1"]
        );
        session.shutdown();
    }
}
