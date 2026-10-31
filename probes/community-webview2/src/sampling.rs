//! Pure bounded scheduling and native request identity. No network transport.
use crate::{
    channel_login, is_channel_page, is_source_uri, select_request, Failure, ParsedSample,
    Selection, REQUEST_CAP, RESPONSE_CAP,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const START_GAP_MS: u64 = 30_000;
pub const NAVIGATION_UI_MS: u64 = 25_000;
pub const PAUSED_READY_STABLE_MS: u64 = 2_000;
pub const RESPONSE_MS: u64 = 25_000;
pub const EVENT_CAP: usize = 192 * 1024;
pub const BODY_ENVELOPE_CAP: usize = RESPONSE_CAP * 6 + 1024;
pub const MAX_EVENTS: usize = 2048;
pub const MAX_CANDIDATES: usize = 4;
pub const MAX_NAVIGATION_IDS: usize = 64;

/// Structural observations only; no account labels, chat text, or media URLs.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PlayerObservation {
    pub signed_in: bool,
    pub chat_ready: bool,
    pub community_ready: bool,
    pub video_present: bool,
    pub video_ready: bool,
    pub video_paused: Option<bool>,
    pub play_control: bool,
    pub pause_clicked: bool,
}

/// One ordinary pause action per navigation. Stable readiness must precede
/// observer arming. A later loss of verified pause is terminal, never retried.
#[derive(Default)]
pub struct PlayerGate {
    pause_clicked: bool,
    pause_verified: bool,
    stable_since: Option<u64>,
    ready: bool,
}

impl PlayerGate {
    pub fn may_click_pause(&self) -> bool {
        !self.pause_clicked && !self.pause_verified
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn pause_verified(&self) -> bool {
        self.pause_verified
    }
    pub fn observe(
        &mut self,
        now: u64,
        observation: &PlayerObservation,
        require_readiness: bool,
    ) -> Result<bool, &'static str> {
        if observation.pause_clicked {
            if !self.may_click_pause() {
                return Err("unexpected_repeated_pause_action");
            }
            self.pause_clicked = true;
        }
        let paused = observation.video_present
            && observation.video_ready
            && observation.video_paused == Some(true)
            && observation.play_control;
        if !paused {
            if self.pause_verified {
                return Err("verified_player_pause_lost");
            }
            self.stable_since = None;
            return Ok(false);
        }
        self.pause_verified = true;
        if self.ready && !observation.signed_in {
            return Err("signed_in_ui_lost");
        }
        if !require_readiness {
            return Ok(self.ready && observation.signed_in);
        }
        if !observation.signed_in || !observation.chat_ready || !observation.community_ready {
            self.stable_since = None;
            self.ready = false;
            return Ok(false);
        }
        let since = self.stable_since.get_or_insert(now);
        self.ready = now.saturating_sub(*since) >= PAUSED_READY_STABLE_MS;
        Ok(self.ready)
    }
}

/// A navigation intent invalidates the previous ID before asynchronous observer
/// shutdown. Only an issued current intent can acquire a NavigationStarting ID.
#[derive(Default)]
pub struct NavigationGate {
    intent: u64,
    issued: bool,
    started_id: Option<u64>,
    seen_ids: HashSet<u64>,
}

impl NavigationGate {
    pub fn begin(&mut self) -> u64 {
        self.intent += 1;
        self.issued = false;
        self.started_id = None;
        self.intent
    }
    pub fn issue(&mut self, intent: u64) -> bool {
        if intent != self.intent {
            return false;
        }
        self.issued = true;
        true
    }
    pub fn started(&mut self, id: u64) -> bool {
        if !self.issued {
            return false;
        }
        // Redirects retain their navigation ID. Once this intent acquires its
        // ID, neither a superseded redirect nor another navigation may replace
        // it. Retain IDs across intents so a delayed old Starting cannot win
        // the race before the new intent's first Starting event either.
        if let Some(current) = self.started_id {
            return current == id;
        }
        if self.seen_ids.contains(&id) || self.seen_ids.len() >= MAX_NAVIGATION_IDS {
            return false;
        }
        self.seen_ids.insert(id);
        self.started_id = Some(id);
        true
    }
    pub fn has_started(&self) -> bool {
        self.issued && self.started_id.is_some()
    }
    pub fn completed(&self, id: u64) -> bool {
        self.issued && self.started_id == Some(id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub channels: Vec<String>,
    pub circuits: usize,
    pub max_seconds: u64,
}

impl Config {
    pub fn new(
        channels: Vec<String>,
        circuits: usize,
        max_seconds: u64,
    ) -> Result<Self, &'static str> {
        let channels: Vec<String> = channels
            .iter()
            .map(|value| {
                channel_login(value)
                    .filter(|v| !matches!(v.as_str(), "login" | "signup" | "settings"))
            })
            .collect::<Option<_>>()
            .ok_or("invalid_channel")?;
        if !(2..=3).contains(&channels.len())
            || channels.iter().collect::<HashSet<_>>().len() != channels.len()
        {
            return Err("two_or_three_distinct_channels_required");
        }
        if !(1..=3).contains(&circuits) {
            return Err("circuits_must_be_1_to_3");
        }
        if !(60..=600).contains(&max_seconds) {
            return Err("runtime_must_be_60_to_600_seconds");
        }
        Ok(Self {
            channels,
            circuits,
            max_seconds,
        })
    }
}

/// Argument parsing is offline; invalid inputs never initialize the browser.
pub fn arguments(args: &[String]) -> Result<Option<Config>, &'static str> {
    if args.is_empty() {
        return Ok(None);
    }
    if args.first().map(String::as_str) != Some("--autonomous") {
        return Err("expected_autonomous_mode");
    }
    let mut channels = Vec::new();
    let mut circuits = None;
    let mut max_seconds = None;
    let mut index = 1;
    while index < args.len() {
        let value = args.get(index + 1).ok_or("missing_argument_value")?;
        match args[index].as_str() {
            "--channel" if channels.len() < 3 => channels.push(value.clone()),
            "--circuits" if circuits.is_none() => {
                circuits = Some(value.parse().map_err(|_| "invalid_circuits")?)
            }
            "--max-seconds" if max_seconds.is_none() => {
                max_seconds = Some(value.parse().map_err(|_| "invalid_runtime")?)
            }
            _ => return Err("unsupported_or_duplicate_argument"),
        }
        index += 2;
    }
    Config::new(channels, circuits.unwrap_or(2), max_seconds.unwrap_or(240)).map(Some)
}

#[derive(Clone, Debug, Serialize)]
pub struct Attempt {
    pub sequence: usize,
    pub circuit: usize,
    pub channel: String,
    pub started_ms: u64,
}

pub struct Schedule {
    pub config: Config,
    pub active: Option<Attempt>,
    next: usize,
    last_start: Option<u64>,
    disabled: HashSet<String>,
    pub stopped: bool,
}

impl Schedule {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            active: None,
            next: 0,
            last_start: None,
            disabled: HashSet::new(),
            stopped: false,
        }
    }
    pub fn expired(&self, now: u64) -> bool {
        now >= self.config.max_seconds * 1000
    }
    pub fn complete(&self) -> bool {
        self.active.is_none() && self.next == self.config.channels.len() * self.config.circuits
    }
    pub fn start_due(&mut self, now: u64) -> Option<Attempt> {
        if self.stopped
            || self.active.is_some()
            || self.complete()
            || self.expired(now)
            || self
                .last_start
                .is_some_and(|last| now.saturating_sub(last) < START_GAP_MS)
        {
            return None;
        }
        let count = self.config.channels.len();
        while self.next < count * self.config.circuits
            && self
                .disabled
                .contains(&self.config.channels[self.next % count])
        {
            self.next += 1;
        }
        if self.next == count * self.config.circuits {
            return None;
        }
        let attempt = Attempt {
            sequence: self.next + 1,
            circuit: self.next / count + 1,
            channel: self.config.channels[self.next % count].clone(),
            started_ms: now,
        };
        self.next += 1;
        self.last_start = Some(now);
        self.active = Some(attempt.clone());
        Some(attempt)
    }
    pub fn finish(&mut self) {
        self.active = None;
    }
    pub fn note_trigger_started(&mut self, now: u64) {
        self.last_start = Some(self.last_start.unwrap_or(0).max(now));
    }
    pub fn finish_failed(&mut self) {
        if let Some(attempt) = self.active.take() {
            self.disabled.insert(attempt.channel);
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Provenance {
    pub request_after_trigger_verified: bool,
    pub request_after_trigger_ms: Option<f64>,
    pub request_to_response_ms: Option<f64>,
    pub request_to_finished_ms: Option<f64>,
    pub from_disk_cache: Option<bool>,
    pub from_service_worker: Option<bool>,
    pub from_prefetch_cache: Option<bool>,
    pub served_from_cache_event: bool,
    pub encoded_bytes: Option<u64>,
    pub raw_encoded_data_length: f64,
    pub request_monotonic_seconds: f64,
    pub response_monotonic_seconds: f64,
    pub finished_monotonic_seconds: f64,
    pub finished_minus_response_ms: Option<f64>,
    pub timing_order: &'static str,
    pub transport: &'static str,
    pub server_freshness: &'static str,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRequest {
    pub url: String,
    pub method: String,
    pub post_data: Option<String>,
    // Headers/cookies/initiator stacks are deliberately not deserialized.
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEvent {
    pub request_id: String,
    pub frame_id: Option<String>,
    #[serde(rename = "documentURL")]
    pub document_url: String,
    pub timestamp: f64,
    pub wall_time: f64,
    pub request: NativeRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeResponse {
    pub url: String,
    pub status: u16,
    pub from_disk_cache: Option<bool>,
    pub from_service_worker: Option<bool>,
    pub from_prefetch_cache: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseEvent {
    pub request_id: String,
    pub frame_id: Option<String>,
    pub timestamp: f64,
    pub response: NativeResponse,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishedEvent {
    pub request_id: String,
    pub timestamp: f64,
    pub encoded_data_length: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdEvent {
    pub request_id: String,
}

pub fn event<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, &'static str> {
    if json.len() > EVENT_CAP {
        return Err("native_event_size_limit");
    }
    serde_json::from_str(json).map_err(|_| "native_event_invalid")
}

#[derive(Deserialize)]
struct FrameIdentity {
    id: String,
    url: String,
}
#[derive(Deserialize)]
struct FrameNode {
    frame: FrameIdentity,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameTree {
    frame_tree: FrameNode,
}

pub fn root_frame(json: &str, channel: &str) -> Result<String, &'static str> {
    let tree: FrameTree = event(json)?;
    let frame = tree.frame_tree.frame;
    if frame.id.is_empty() || frame.id.len() > 128 || !is_channel_page(&frame.url, channel) {
        return Err("native_root_frame_mismatch");
    }
    Ok(frame.id)
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    frame: String,
    selection: Selection,
    request_time: f64,
    wall_time: f64,
    response: Option<ResponseEvent>,
    finished: Option<FinishedEvent>,
    cached: bool,
    failed: bool,
}

/// One finite, numeric-only diagnostic for an already matched source request.
/// Nonfinite input is represented as null with a specific reason code. No raw
/// event, URL, request ID, channel variable, account, or header is serializable.
#[derive(Clone, Debug, Serialize)]
pub struct TimingFailure {
    pub code: &'static str,
    pub request_monotonic_seconds: Option<f64>,
    pub response_monotonic_seconds: Option<f64>,
    pub finished_monotonic_seconds: Option<f64>,
    pub request_to_response_ms: Option<f64>,
    pub request_to_finished_ms: Option<f64>,
    pub finished_minus_response_ms: Option<f64>,
    pub raw_encoded_data_length: Option<f64>,
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

impl TimingFailure {
    fn new(candidate: &Candidate, finished: &FinishedEvent, code: &'static str) -> Self {
        let response = candidate.response.as_ref().map(|value| value.timestamp);
        Self {
            code,
            request_monotonic_seconds: finite(candidate.request_time),
            response_monotonic_seconds: response.and_then(finite),
            finished_monotonic_seconds: finite(finished.timestamp),
            request_to_response_ms: response
                .and_then(|value| finite((value - candidate.request_time) * 1000.0)),
            request_to_finished_ms: finite((finished.timestamp - candidate.request_time) * 1000.0),
            finished_minus_response_ms: response
                .and_then(|value| finite((finished.timestamp - value) * 1000.0)),
            raw_encoded_data_length: finite(finished.encoded_data_length),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BodyRequest {
    pub id: String,
    pub selection: Selection,
    pub status: u16,
    pub provenance: Provenance,
}

/// At most four candidate operations may await the click's browser timestamp.
/// The timestamp can arrive after request/response events. Selection still uses
/// initiation time, never callback arrival time. IDs never leave process memory.
pub struct Tracker {
    generation: u64,
    channel: String,
    expected_frame: String,
    trigger: Option<f64>,
    candidates: Vec<Candidate>,
    events: usize,
    body_taken: bool,
    timing_failure: Option<TimingFailure>,
}

impl Tracker {
    pub fn new(generation: u64, channel: String, expected_frame: String) -> Self {
        Self {
            generation,
            channel,
            expected_frame,
            trigger: None,
            candidates: Vec::new(),
            events: 0,
            body_taken: false,
            timing_failure: None,
        }
    }
    pub fn count_event(&mut self) -> Result<(), &'static str> {
        self.events += 1;
        if self.events > MAX_EVENTS {
            Err("native_event_count_limit")
        } else {
            Ok(())
        }
    }
    pub fn set_trigger(&mut self, unix_ms: f64) -> Result<(), &'static str> {
        if !unix_ms.is_finite() || unix_ms <= 0.0 || self.trigger.is_some() {
            return Err("invalid_trigger_time");
        }
        self.trigger = Some(unix_ms / 1000.0);
        self.candidates
            .retain(|candidate| candidate.wall_time >= unix_ms / 1000.0);
        Ok(())
    }
    pub fn request(&mut self, generation: u64, value: RequestEvent) -> Result<(), &'static str> {
        if generation != self.generation
            || !is_source_uri(&value.request.url)
            || value.request.method != "POST"
            || !is_channel_page(&value.document_url, &self.channel)
        {
            return Ok(());
        }
        if !value.timestamp.is_finite() || !value.wall_time.is_finite() || value.wall_time <= 0.0 {
            return Err("invalid_native_request_time");
        }
        if self
            .trigger
            .is_some_and(|trigger| value.wall_time < trigger)
        {
            return Ok(());
        }
        let frame = value
            .frame_id
            .filter(|frame| !frame.is_empty() && frame.len() <= 128)
            .ok_or("missing_native_frame")?;
        if frame != self.expected_frame {
            return Ok(());
        }
        if value.request_id.is_empty() || value.request_id.len() > 128 {
            return Err("native_request_id_limit");
        }
        let Some(body) = value.request.post_data else {
            return Err("native_request_body_unavailable");
        };
        if body.len() > REQUEST_CAP {
            return Err("request_size_limit");
        }
        let Some(selection) =
            select_request(body.as_bytes(), &self.channel).map_err(Failure::code)?
        else {
            return Ok(());
        };
        if self
            .candidates
            .iter()
            .any(|candidate| candidate.id == value.request_id)
        {
            return Err("reused_native_request_id");
        }
        if self.candidates.len() >= MAX_CANDIDATES {
            return Err("native_candidate_limit");
        }
        self.candidates.push(Candidate {
            id: value.request_id,
            frame,
            selection,
            request_time: value.timestamp,
            wall_time: value.wall_time,
            response: None,
            finished: None,
            cached: false,
            failed: false,
        });
        if self.body_taken {
            return Err("ambiguous_native_source");
        }
        Ok(())
    }
    pub fn response(&mut self, generation: u64, value: ResponseEvent) -> Result<(), &'static str> {
        if generation != self.generation {
            return Ok(());
        }
        let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|c| c.id == value.request_id)
        else {
            return Ok(());
        };
        if value.frame_id.as_deref() != Some(&candidate.frame)
            || !is_source_uri(&value.response.url)
            || !value.timestamp.is_finite()
            || value.timestamp < candidate.request_time
        {
            return Err("native_response_identity_mismatch");
        }
        if candidate.response.is_some() {
            return Err("duplicate_native_response");
        }
        candidate.response = Some(value);
        Ok(())
    }
    pub fn finished(&mut self, generation: u64, value: FinishedEvent) -> Result<(), &'static str> {
        if generation != self.generation {
            return Ok(());
        }
        let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|c| c.id == value.request_id)
        else {
            return Ok(());
        };
        // Blink's URLLoaderClient uses -1 for unknown encoded data length.
        // The decoded body and protocol envelope retain independent caps.
        let failure = if !value.timestamp.is_finite() {
            Some("native_finished_timestamp_nonfinite")
        } else if value.timestamp < candidate.request_time {
            Some("native_finished_before_request")
        } else if !value.encoded_data_length.is_finite() {
            Some("native_encoded_length_nonfinite")
        } else if value.encoded_data_length < 0.0 && value.encoded_data_length != -1.0 {
            Some("native_encoded_length_invalid_negative")
        } else if value.encoded_data_length > (RESPONSE_CAP + REQUEST_CAP) as f64 {
            Some("response_size_limit")
        } else {
            None
        };
        if let Some(code) = failure {
            if self.timing_failure.is_none() {
                self.timing_failure = Some(TimingFailure::new(candidate, &value, code));
            }
            return Err(code);
        }
        candidate.finished = Some(value);
        Ok(())
    }
    pub fn cached(&mut self, id: &str) {
        if let Some(candidate) = self.candidates.iter_mut().find(|c| c.id == id) {
            candidate.cached = true;
        }
    }
    pub fn failed(&mut self, id: &str) {
        if let Some(candidate) = self.candidates.iter_mut().find(|c| c.id == id) {
            candidate.failed = true;
        }
    }
    fn ready(&self) -> Result<Option<BodyRequest>, &'static str> {
        if let Some(failure) = &self.timing_failure {
            return Err(failure.code);
        }
        let Some(trigger) = self.trigger else {
            return Ok(None);
        };
        if self
            .candidates
            .iter()
            .filter(|c| c.wall_time >= trigger)
            .count()
            > 1
        {
            return Err("ambiguous_native_source");
        }
        let Some(candidate) = self
            .candidates
            .iter()
            .filter(|c| c.wall_time >= trigger)
            .min_by(|a, b| a.wall_time.total_cmp(&b.wall_time))
        else {
            return Ok(None);
        };
        if candidate.failed {
            return Err("native_loading_failed");
        }
        let Some(response) = &candidate.response else {
            return Ok(None);
        };
        if matches!(response.response.status, 401 | 403 | 429) {
            return Err("http_denied");
        }
        if response.response.status != 200 {
            return Err("http_error");
        }
        let Some(finished) = &candidate.finished else {
            return Ok(None);
        };
        // Chromium records responseReceived at event reporting time but uses
        // the network-stack finish time for loadingFinished. The frontend
        // explicitly handles finish < response-event time. Keep raw values;
        // that cross-event ordering is not request identity evidence.
        let r = &response.response;
        let cached = candidate.cached
            || r.from_disk_cache == Some(true)
            || r.from_service_worker == Some(true)
            || r.from_prefetch_cache == Some(true);
        let transport = if cached {
            "cache_or_service_worker"
        } else if r.from_disk_cache == Some(false)
            && r.from_service_worker == Some(false)
            && r.from_prefetch_cache == Some(false)
            && finished.encoded_data_length > 0.0
        {
            "network_response"
        } else {
            "unknown"
        };
        let result = BodyRequest {
            id: candidate.id.clone(),
            selection: candidate.selection.clone(),
            status: r.status,
            provenance: Provenance {
                request_after_trigger_verified: true,
                request_after_trigger_ms: finite((candidate.wall_time - trigger) * 1000.0),
                request_to_response_ms: finite(
                    (response.timestamp - candidate.request_time) * 1000.0,
                ),
                request_to_finished_ms: finite(
                    (finished.timestamp - candidate.request_time) * 1000.0,
                ),
                from_disk_cache: r.from_disk_cache,
                from_service_worker: r.from_service_worker,
                from_prefetch_cache: r.from_prefetch_cache,
                served_from_cache_event: candidate.cached,
                encoded_bytes: (finished.encoded_data_length >= 0.0)
                    .then_some(finished.encoded_data_length as u64),
                raw_encoded_data_length: finished.encoded_data_length,
                request_monotonic_seconds: candidate.request_time,
                response_monotonic_seconds: response.timestamp,
                finished_monotonic_seconds: finished.timestamp,
                finished_minus_response_ms: finite(
                    (finished.timestamp - response.timestamp) * 1000.0,
                ),
                timing_order: if finished.timestamp < response.timestamp {
                    "finish_before_response_event"
                } else {
                    "finish_at_or_after_response_event"
                },
                transport,
                server_freshness: "unknown",
            },
        };
        Ok(Some(result))
    }

    pub fn take_ready(&mut self) -> Result<Option<BodyRequest>, &'static str> {
        let ready = self.ready()?;
        if self.body_taken {
            return Ok(None);
        }
        self.body_taken = ready.is_some();
        Ok(ready)
    }

    /// Recheck all events received while the asynchronous body read was pending.
    /// This also refreshes late cache evidence before aggregate acceptance.
    pub fn complete_body(&self, generation: u64, id: &str) -> Result<BodyRequest, &'static str> {
        if generation != self.generation || !self.body_taken {
            return Err("stale_native_body_callback");
        }
        let ready = self.ready()?.ok_or("native_body_identity_unavailable")?;
        if ready.id != id {
            return Err("native_body_identity_mismatch");
        }
        Ok(ready)
    }

    pub fn timing_failure(&self, code: &'static str) -> Option<TimingFailure> {
        self.timing_failure
            .as_ref()
            .filter(|value| value.code == code)
            .cloned()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BodyEnvelope {
    body: String,
    base64_encoded: bool,
}

/// JSON string escaping may expand the protocol envelope sixfold. Decoded text
/// still has a strict 1 MiB cap. Base64 is fail-closed, never guessed as JSON.
pub fn response_body(json: &str) -> Result<Vec<u8>, &'static str> {
    if json.len() > BODY_ENVELOPE_CAP {
        return Err("response_size_limit");
    }
    let value: BodyEnvelope = serde_json::from_str(json).map_err(|_| "native_body_invalid")?;
    if value.base64_encoded {
        return Err("native_body_encoding_unsupported");
    }
    if value.body.len() > RESPONSE_CAP {
        return Err("response_size_limit");
    }
    Ok(value.body.into_bytes())
}

#[derive(Debug, Serialize)]
pub struct AttemptEvidence {
    pub attempt: Attempt,
    pub ended_ms: Option<u64>,
    pub status: &'static str,
    pub video_element_present: Option<bool>,
    pub video_paused: Option<bool>,
    pub player_pause_verified: bool,
    pub pause_verified_before_trigger: bool,
    pub player_pause_last_checked_ms: Option<u64>,
    pub normal_pause_clicks: usize,
    pub signed_in_ui_verified: bool,
    pub http_status: Option<u16>,
    pub provenance: Option<Provenance>,
    pub reported_chat_count: Option<u64>,
    pub categories: Option<BTreeMap<String, usize>>,
    pub unique_accounts: Option<usize>,
    pub overlap_with_previous: Option<usize>,
    pub union_accounts: Option<usize>,
    pub same_accounts_as_previous: Option<bool>,
}

#[derive(Default, Debug, Serialize)]
pub struct ChannelEvidence {
    pub attempts: usize,
    pub samples: usize,
    pub union_accounts: usize,
    #[serde(skip)]
    previous: Option<HashSet<String>>,
    #[serde(skip)]
    union: HashSet<String>,
}

#[derive(Debug, Serialize)]
pub struct RunEvidence {
    pub mode: &'static str,
    pub circuits: usize,
    pub max_seconds: u64,
    pub status: &'static str,
    pub attempts: Vec<AttemptEvidence>,
    pub channels: BTreeMap<String, ChannelEvidence>,
    pub source_samples_received: usize,
    pub fresh_server_samples_proven: bool,
}

impl RunEvidence {
    pub fn new(config: &Config) -> Self {
        Self {
            mode: "finite_autonomous_full_channel_paused",
            circuits: config.circuits,
            max_seconds: config.max_seconds,
            status: "starting",
            attempts: Vec::new(),
            channels: config
                .channels
                .iter()
                .map(|c| (c.clone(), ChannelEvidence::default()))
                .collect(),
            source_samples_received: 0,
            fresh_server_samples_proven: false,
        }
    }
    pub fn start(&mut self, attempt: Attempt) {
        self.channels.get_mut(&attempt.channel).unwrap().attempts += 1;
        self.attempts.push(AttemptEvidence {
            attempt,
            ended_ms: None,
            status: "navigating",
            video_element_present: None,
            video_paused: None,
            player_pause_verified: false,
            pause_verified_before_trigger: false,
            player_pause_last_checked_ms: None,
            normal_pause_clicks: 0,
            signed_in_ui_verified: false,
            http_status: None,
            provenance: None,
            reported_chat_count: None,
            categories: None,
            unique_accounts: None,
            overlap_with_previous: None,
            union_accounts: None,
            same_accounts_as_previous: None,
        });
    }
    pub fn finish(&mut self, now: u64, status: &'static str) {
        if let Some(a) = self.attempts.last_mut() {
            if a.ended_ms.is_none() {
                a.ended_ms = Some(now);
                a.status = status;
            }
        }
    }
    pub fn finalize(&mut self, now: u64, reason: &'static str) {
        if matches!(self.status, "starting" | "running") {
            self.finish(now, reason);
            self.status = reason;
        }
    }
    pub fn sample(&mut self, parsed: ParsedSample, body: &BodyRequest) {
        let attempt = self.attempts.last_mut().unwrap();
        let channel = self.channels.get_mut(&attempt.attempt.channel).unwrap();
        attempt.overlap_with_previous = channel
            .previous
            .as_ref()
            .map(|previous| previous.intersection(&parsed.accounts).count());
        attempt.same_accounts_as_previous = channel
            .previous
            .as_ref()
            .map(|previous| previous == &parsed.accounts);
        channel.union.extend(parsed.accounts.iter().cloned());
        channel.union_accounts = channel.union.len();
        channel.samples += 1;
        attempt.unique_accounts = Some(parsed.accounts.len());
        attempt.union_accounts = Some(channel.union_accounts);
        channel.previous = Some(parsed.accounts);
        attempt.reported_chat_count = Some(parsed.count);
        attempt.categories = Some(parsed.categories);
        attempt.provenance = Some(body.provenance.clone());
        attempt.http_status = Some(body.status);
        self.source_samples_received += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paused_channel() -> PlayerObservation {
        PlayerObservation {
            signed_in: true,
            chat_ready: true,
            community_ready: true,
            video_present: true,
            video_ready: true,
            video_paused: Some(true),
            play_control: true,
            pause_clicked: false,
        }
    }

    #[test]
    fn player_readiness_requires_confirmed_pause_and_stable_signed_in_chat() {
        let mut gate = PlayerGate::default();
        assert!(!gate
            .observe(0, &PlayerObservation::default(), true)
            .unwrap());
        let mut playing = paused_channel();
        playing.video_paused = Some(false);
        playing.play_control = false;
        playing.pause_clicked = true;
        assert!(!gate.observe(100, &playing, true).unwrap());
        assert!(!gate.may_click_pause());
        assert!(!gate.pause_verified());
        let mut loading = paused_channel();
        loading.signed_in = false;
        assert!(!gate.observe(200, &loading, true).unwrap());
        assert!(gate.pause_verified());
        assert!(!gate.ready());
        let ready = paused_channel();
        assert!(!gate.observe(500, &ready, true).unwrap());
        loading.signed_in = true;
        loading.chat_ready = false;
        assert!(!gate.observe(2_000, &loading, true).unwrap());
        assert!(!gate.observe(2_100, &ready, true).unwrap());
        assert!(!gate.observe(4_099, &ready, true).unwrap());
        assert!(gate.observe(4_100, &ready, true).unwrap());
        assert!(gate.ready());
        assert!(!gate.may_click_pause());
    }

    #[test]
    fn pause_guard_survives_open_community_and_idle_but_not_resumed_playback() {
        let mut gate = PlayerGate::default();
        let ready = paused_channel();
        assert!(!gate.observe(0, &ready, true).unwrap());
        assert!(gate.observe(PAUSED_READY_STABLE_MS, &ready, true).unwrap());
        let mut opened = ready.clone();
        opened.community_ready = false;
        opened.chat_ready = false;
        assert!(gate.observe(3_000, &opened, false).unwrap());
        assert!(gate.observe(30_000, &opened, false).unwrap());
        opened.video_paused = Some(false);
        assert_eq!(
            gate.observe(30_001, &opened, false),
            Err("verified_player_pause_lost")
        );
        let next_navigation = PlayerGate::default();
        assert!(!next_navigation.ready());
        assert!(!next_navigation.pause_verified());
        assert!(next_navigation.may_click_pause());
    }

    #[test]
    fn pause_guard_rejects_missing_video_auth_loss_and_repeated_pause_actions() {
        for missing in ["video", "control", "signed_in"] {
            let mut gate = PlayerGate::default();
            let mut observed = paused_channel();
            gate.observe(0, &observed, true).unwrap();
            assert!(gate
                .observe(PAUSED_READY_STABLE_MS, &observed, true)
                .unwrap());
            match missing {
                "video" => observed.video_present = false,
                "control" => observed.play_control = false,
                _ => observed.signed_in = false,
            }
            let expected = if missing == "signed_in" {
                "signed_in_ui_lost"
            } else {
                "verified_player_pause_lost"
            };
            assert_eq!(gate.observe(3_000, &observed, false), Err(expected));
        }
        let mut gate = PlayerGate::default();
        let mut observed = paused_channel();
        observed.pause_clicked = true;
        gate.observe(0, &observed, true).unwrap();
        assert_eq!(
            gate.observe(1, &observed, true),
            Err("unexpected_repeated_pause_action")
        );
    }

    #[test]
    fn autonomous_defaults_are_full_channel_evidence_and_finite_240_seconds() {
        let config = arguments(&[
            "--autonomous".into(),
            "--channel".into(),
            "first".into(),
            "--channel".into(),
            "second".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.circuits, 2);
        assert_eq!(config.max_seconds, 240);
        let mut evidence = RunEvidence::new(&config);
        evidence.start(Schedule::new(config).start_due(0).unwrap());
        assert_eq!(evidence.mode, "finite_autonomous_full_channel_paused");
        assert!(!evidence.attempts[0].player_pause_verified);
        assert!(!evidence.attempts[0].pause_verified_before_trigger);
        assert_eq!(evidence.attempts[0].normal_pause_clicks, 0);
    }

    #[test]
    fn new_navigation_intent_rejects_old_completion_before_and_after_issue() {
        let mut gate = NavigationGate::default();
        let a = gate.begin();
        assert!(gate.issue(a));
        assert!(gate.started(100));
        assert!(gate.completed(100));
        let b = gate.begin();
        assert!(!gate.completed(100));
        assert!(!gate.started(100));
        assert!(!gate.issue(a));
        assert!(gate.issue(b));
        assert!(!gate.started(100));
        assert!(!gate.completed(100));
        assert!(gate.started(200));
        assert!(!gate.started(100));
        assert!(!gate.started(300));
        assert!(gate.started(200)); // A redirect of the current navigation.
        assert!(!gate.completed(100));
        assert!(gate.completed(200));
    }

    #[test]
    fn navigation_identity_retention_is_bounded_and_never_evicts_old_ids() {
        let mut gate = NavigationGate::default();
        for id in 0..MAX_NAVIGATION_IDS as u64 {
            let intent = gate.begin();
            assert!(gate.issue(intent));
            assert!(gate.started(id));
            assert!(gate.completed(id));
        }
        let intent = gate.begin();
        assert!(gate.issue(intent));
        assert!(!gate.started(0));
        assert!(!gate.started(MAX_NAVIGATION_IDS as u64));
        assert!(!gate.has_started());
        assert_eq!(gate.seen_ids.len(), MAX_NAVIGATION_IDS);
    }

    #[test]
    fn late_second_source_request_invalidates_pending_body_completion() {
        let mut tracker = Tracker::new(2, "test".into(), "top".into());
        tracker.set_trigger(100_000.0).unwrap();
        tracker.request(2, req("first", 101.0)).unwrap();
        tracker.response(2, resp("first", Some(false))).unwrap();
        tracker.finished(2, finish("first")).unwrap();
        let pending = tracker.take_ready().unwrap().unwrap();
        assert_eq!(
            tracker.request(2, req("second", 102.0)).unwrap_err(),
            "ambiguous_native_source"
        );
        let mut denial = resp("second", Some(false));
        denial.response.status = 403;
        tracker.response(2, denial).unwrap();
        assert_eq!(
            tracker.complete_body(2, &pending.id).unwrap_err(),
            "ambiguous_native_source"
        );
        assert_eq!(
            tracker.complete_body(1, &pending.id).unwrap_err(),
            "stale_native_body_callback"
        );
    }

    #[test]
    fn pending_body_rechecks_late_cache_or_failure_events() {
        let mut tracker = Tracker::new(2, "test".into(), "top".into());
        tracker.set_trigger(100_000.0).unwrap();
        tracker.request(2, req("first", 101.0)).unwrap();
        tracker.response(2, resp("first", Some(false))).unwrap();
        tracker.finished(2, finish("first")).unwrap();
        let pending = tracker.take_ready().unwrap().unwrap();
        tracker.cached("first");
        assert_eq!(
            tracker
                .complete_body(2, &pending.id)
                .unwrap()
                .provenance
                .transport,
            "cache_or_service_worker"
        );
        tracker.failed("first");
        assert_eq!(
            tracker.complete_body(2, &pending.id).unwrap_err(),
            "native_loading_failed"
        );
    }

    #[test]
    fn closing_finalizes_active_attempt_and_preserves_terminal_failure() {
        let config = Config::new(vec!["a".into(), "b".into()], 1, 60).unwrap();
        let attempt = Attempt {
            sequence: 2,
            circuit: 1,
            channel: "b".into(),
            started_ms: 30_000,
        };
        let mut cancelled = RunEvidence::new(&config);
        cancelled.start(attempt.clone());
        cancelled.status = "running";
        cancelled.finalize(31_000, "operator_cancelled");
        assert_eq!(cancelled.status, "operator_cancelled");
        assert_eq!(cancelled.attempts[0].ended_ms, Some(31_000));
        assert_eq!(cancelled.attempts[0].status, "operator_cancelled");
        let mut denied = RunEvidence::new(&config);
        denied.start(attempt);
        denied.finish(31_000, "integrity_denied");
        denied.status = "integrity_denied";
        denied.finalize(32_000, "circuits_completed_with_gaps");
        denied.finalize(33_000, "operator_cancelled");
        denied.finish(34_000, "run_deadline");
        assert_eq!(denied.status, "integrity_denied");
        assert_eq!(denied.attempts[0].ended_ms, Some(31_000));
        assert_eq!(denied.attempts[0].status, "integrity_denied");
    }
    fn req(id: &str, wall: f64) -> RequestEvent {
        event(&json!({"requestId":id,"frameId":"top","documentURL":"https://www.twitch.tv/test","timestamp":20.0,"wallTime":wall,"request":{"url":"https://gql.twitch.tv/gql","method":"POST","postData":"{\"operationName\":\"CommunityTab\",\"variables\":{\"login\":\"test\"}}","headers":{"Authorization":"secret"}}}).to_string()).unwrap()
    }
    fn resp(id: &str, disk: Option<bool>) -> ResponseEvent {
        ResponseEvent {
            request_id: id.into(),
            frame_id: Some("top".into()),
            timestamp: 20.1,
            response: NativeResponse {
                url: "https://gql.twitch.tv/gql".into(),
                status: 200,
                from_disk_cache: disk,
                from_service_worker: Some(false),
                from_prefetch_cache: Some(false),
            },
        }
    }
    fn finish(id: &str) -> FinishedEvent {
        FinishedEvent {
            request_id: id.into(),
            timestamp: 20.2,
            encoded_data_length: 1000.0,
        }
    }
    #[test]
    fn fair_round_robin_has_no_retry_and_global_start_spacing() {
        let mut s =
            Schedule::new(Config::new(vec!["a".into(), "b".into(), "c".into()], 2, 420).unwrap());
        for (i, c) in ["a", "b", "c", "a", "b", "c"].iter().enumerate() {
            let now = i as u64 * START_GAP_MS;
            if i > 0 {
                assert!(s.start_due(now - 1).is_none());
            }
            let a = s.start_due(now).unwrap();
            assert_eq!(&a.channel, c);
            assert_eq!(a.circuit, i / 3 + 1);
            assert!(s.start_due(now + START_GAP_MS).is_none());
            s.finish();
        }
        assert!(s.complete());
        assert!(s.start_due(600_000).is_none());
    }
    #[test]
    fn config_and_total_runtime_are_bounded() {
        assert!(Config::new(vec!["A".into(), "a".into()], 1, 60).is_err());
        assert!(Config::new(vec!["a".into(), "b".into()], 4, 60).is_err());
        assert!(Config::new(vec!["a".into(), "b".into()], 1, 601).is_err());
        let mut s = Schedule::new(Config::new(vec!["a".into(), "b".into()], 1, 60).unwrap());
        assert!(s.start_due(60_000).is_none());
        assert!(arguments(&["--autonomous".into(), "--channel".into(), "a".into()]).is_err());
    }

    #[test]
    fn failed_channel_does_not_retry_or_starve_other_channels() {
        let mut s = Schedule::new(Config::new(vec!["a".into(), "b".into()], 2, 420).unwrap());
        assert_eq!(s.start_due(0).unwrap().channel, "a");
        s.finish_failed();
        assert_eq!(s.start_due(40_000).unwrap().channel, "b");
        s.finish();
        let last = s.start_due(70_000).unwrap();
        assert_eq!(last.channel, "b");
        assert_eq!(last.circuit, 2);
        s.finish();
        assert!(s.complete());
    }

    #[test]
    fn two_posttrigger_candidates_are_ambiguous_not_an_earliest_guess() {
        let mut t = Tracker::new(2, "test".into(), "top".into());
        t.request(2, req("a", 101.0)).unwrap();
        t.request(2, req("b", 102.0)).unwrap();
        t.set_trigger(100_000.0).unwrap();
        assert_eq!(t.take_ready().unwrap_err(), "ambiguous_native_source");
    }
    #[test]
    fn delayed_pretrigger_request_and_previous_generation_cannot_become_samples() {
        let mut t = Tracker::new(2, "test".into(), "top".into());
        t.request(1, req("old-generation", 101.0)).unwrap();
        t.request(2, req("before-click", 99.0)).unwrap();
        t.response(2, resp("before-click", Some(false))).unwrap();
        t.finished(2, finish("before-click")).unwrap();
        assert!(t.take_ready().unwrap().is_none());
        t.set_trigger(100_000.0).unwrap();
        assert!(t.take_ready().unwrap().is_none());
        t.request(2, req("late-prearm-event", 99.5)).unwrap();
        assert!(t.take_ready().unwrap().is_none());
    }
    #[test]
    fn callback_reordering_keeps_exact_id_and_frame() {
        let mut t = Tracker::new(2, "test".into(), "top".into());
        t.request(2, req("current", 100.1)).unwrap();
        t.finished(2, finish("current")).unwrap();
        t.response(2, resp("unrelated", Some(false))).unwrap();
        t.response(2, resp("current", Some(false))).unwrap();
        t.set_trigger(100_000.0).unwrap();
        let b = t.take_ready().unwrap().unwrap();
        assert_eq!(b.id, "current");
        assert_eq!(b.provenance.transport, "network_response");
        assert_eq!(b.provenance.server_freshness, "unknown");
        assert!(t.take_ready().unwrap().is_none());
        let mut t = Tracker::new(2, "test".into(), "top".into());
        t.request(2, req("a", 101.0)).unwrap();
        let mut r = resp("a", Some(false));
        r.frame_id = Some("other".into());
        assert!(t.response(2, r).is_err());
    }
    #[test]
    fn cache_and_unknown_metadata_never_prove_freshness() {
        for (disk, cached, expected) in [
            (Some(true), false, "cache_or_service_worker"),
            (None, false, "unknown"),
            (Some(false), true, "cache_or_service_worker"),
        ] {
            let mut t = Tracker::new(2, "test".into(), "top".into());
            t.set_trigger(100_000.0).unwrap();
            t.request(2, req("a", 101.0)).unwrap();
            if cached {
                t.cached("a");
            }
            t.response(2, resp("a", disk)).unwrap();
            t.finished(2, finish("a")).unwrap();
            let b = t.take_ready().unwrap().unwrap();
            assert_eq!(b.provenance.transport, expected);
            assert_eq!(b.provenance.server_freshness, "unknown");
        }
    }
    #[test]
    fn network_finish_before_response_event_preserves_raw_times_and_identity() {
        for finish_callback_first in [false, true] {
            let mut tracker = Tracker::new(2, "test".into(), "top".into());
            tracker.request(2, req("matched", 100.1)).unwrap();
            let mut finished = finish("matched");
            finished.timestamp = 20.05;
            if finish_callback_first {
                tracker.finished(2, finished).unwrap();
                tracker.response(2, resp("matched", Some(false))).unwrap();
            } else {
                tracker.response(2, resp("matched", Some(false))).unwrap();
                tracker.finished(2, finished).unwrap();
            }
            tracker.set_trigger(100_000.0).unwrap();
            let pending = tracker.take_ready().unwrap().unwrap();
            let accepted = tracker.complete_body(2, &pending.id).unwrap();
            let provenance = accepted.provenance;
            assert!(provenance.request_after_trigger_verified);
            assert_eq!(provenance.request_monotonic_seconds, 20.0);
            assert_eq!(provenance.response_monotonic_seconds, 20.1);
            assert_eq!(provenance.finished_monotonic_seconds, 20.05);
            assert!((provenance.finished_minus_response_ms.unwrap() + 50.0).abs() < 0.0001);
            assert_eq!(provenance.timing_order, "finish_before_response_event");
            assert_eq!(provenance.server_freshness, "unknown");
            assert_eq!(
                tracker.complete_body(1, &pending.id).unwrap_err(),
                "stale_native_body_callback"
            );
            assert_eq!(
                tracker.complete_body(2, "unrelated").unwrap_err(),
                "native_body_identity_mismatch"
            );
        }
    }

    #[test]
    fn unknown_wire_length_accepts_bounded_body_without_freshness_claim() {
        let mut tracker = Tracker::new(2, "test".into(), "top".into());
        tracker.set_trigger(100_000.0).unwrap();
        tracker.request(2, req("matched", 100.1)).unwrap();
        tracker.response(2, resp("matched", Some(false))).unwrap();
        let mut finished = finish("matched");
        finished.encoded_data_length = -1.0;
        tracker.finished(2, finished).unwrap();
        let pending = tracker.take_ready().unwrap().unwrap();
        let accepted = tracker.complete_body(2, &pending.id).unwrap();
        assert_eq!(accepted.provenance.encoded_bytes, None);
        assert_eq!(accepted.provenance.raw_encoded_data_length, -1.0);
        assert_eq!(accepted.provenance.transport, "unknown");
        assert_eq!(accepted.provenance.server_freshness, "unknown");
        let body = json!({"data":{"channel":{"chatters":{
            "count":1,"broadcasters":[],"staff":[],"moderators":[],"vips":[],
            "chatbots":[],"viewers":[{"login":"private_viewer"}]
        }}}})
        .to_string();
        let bytes = response_body(&json!({"body":body,"base64Encoded":false}).to_string()).unwrap();
        let sample = crate::parse_response(&bytes, &accepted.selection, accepted.status).unwrap();
        let config = Config::new(vec!["test".into(), "other".into()], 1, 60).unwrap();
        let mut evidence = RunEvidence::new(&config);
        evidence.start(Schedule::new(config).start_due(0).unwrap());
        evidence.sample(sample, &accepted);
        assert_eq!(evidence.source_samples_received, 1);
        assert!(!evidence.fresh_server_samples_proven);
        let saved = serde_json::to_string(&evidence).unwrap();
        assert!(!saved.contains("private_viewer"));
        assert!(!saved.contains("network_response"));
        assert_eq!(
            response_body(
                &json!({"body":"x".repeat(RESPONSE_CAP + 1),"base64Encoded":false}).to_string()
            )
            .unwrap_err(),
            "response_size_limit"
        );
        assert_eq!(
            response_body(&" ".repeat(BODY_ENVELOPE_CAP + 1)).unwrap_err(),
            "response_size_limit"
        );
    }

    #[test]
    fn invalid_finished_values_have_distinct_numeric_only_diagnostics() {
        for (timestamp, encoded, expected) in [
            (f64::NAN, 1000.0, "native_finished_timestamp_nonfinite"),
            (f64::INFINITY, 1000.0, "native_finished_timestamp_nonfinite"),
            (19.9, 1000.0, "native_finished_before_request"),
            (20.2, f64::NAN, "native_encoded_length_nonfinite"),
            (20.2, f64::INFINITY, "native_encoded_length_nonfinite"),
            (20.2, -2.0, "native_encoded_length_invalid_negative"),
            (20.2, -0.5, "native_encoded_length_invalid_negative"),
            (
                20.2,
                (RESPONSE_CAP + REQUEST_CAP + 1) as f64,
                "response_size_limit",
            ),
        ] {
            let mut tracker = Tracker::new(2, "test".into(), "top".into());
            tracker.set_trigger(100_000.0).unwrap();
            tracker
                .request(2, req("private_request_id", 100.1))
                .unwrap();
            tracker
                .response(2, resp("private_request_id", Some(false)))
                .unwrap();
            tracker.finished(2, finish("private_request_id")).unwrap();
            let pending = tracker.take_ready().unwrap().unwrap();
            let mut finished = finish("private_request_id");
            finished.timestamp = timestamp;
            finished.encoded_data_length = encoded;
            assert_eq!(tracker.finished(2, finished).unwrap_err(), expected);
            assert_eq!(tracker.take_ready().unwrap_err(), expected);
            assert_eq!(tracker.complete_body(2, &pending.id).unwrap_err(), expected);
            let diagnostic = tracker.timing_failure(expected).unwrap();
            assert_eq!(diagnostic.request_monotonic_seconds, Some(20.0));
            assert_eq!(diagnostic.response_monotonic_seconds, Some(20.1));
            assert_eq!(diagnostic.finished_monotonic_seconds, finite(timestamp));
            assert_eq!(diagnostic.raw_encoded_data_length, finite(encoded));
            let saved = serde_json::to_value(diagnostic).unwrap();
            assert_eq!(saved.as_object().unwrap().len(), 8);
            for (key, value) in saved.as_object().unwrap() {
                assert!(key == "code" || value.is_null() || value.is_number());
            }
            assert!(!saved.to_string().contains("private_request_id"));
            assert!(tracker.timing_failure("unrelated_reason").is_none());
        }
    }

    #[test]
    fn unrelated_or_stale_finish_events_cannot_create_timing_diagnostics() {
        let mut tracker = Tracker::new(2, "test".into(), "top".into());
        tracker.set_trigger(100_000.0).unwrap();
        tracker.request(2, req("matched", 100.1)).unwrap();
        for (generation, id) in [(1, "matched"), (2, "unrelated")] {
            let mut finished = finish(id);
            finished.timestamp = f64::NAN;
            tracker.finished(generation, finished).unwrap();
        }
        assert!(tracker.timing_failure.is_none());
        assert!(tracker.take_ready().unwrap().is_none());
    }

    #[test]
    fn caps_and_denials_fail_closed() {
        let mut t = Tracker::new(2, "test".into(), "top".into());
        t.set_trigger(100_000.0).unwrap();
        t.request(2, req("a", 101.0)).unwrap();
        let mut r = resp("a", Some(false));
        r.response.status = 429;
        t.response(2, r).unwrap();
        assert_eq!(t.take_ready().unwrap_err(), "http_denied");
        assert!(event::<IdEvent>(&" ".repeat(EVENT_CAP + 1)).is_err());
        assert!(response_body(
            &json!({"base64Encoded":false,"body":"x".repeat(RESPONSE_CAP+1)}).to_string()
        )
        .is_err());
        assert!(response_body(r#"{"body":"e30=","base64Encoded":true}"#).is_err());
        let mut t = Tracker::new(2, "test".into(), "top".into());
        for _ in 0..MAX_EVENTS {
            t.count_event().unwrap();
        }
        assert!(t.count_event().is_err());
        for i in 0..MAX_CANDIDATES {
            t.request(2, req(&format!("{i}"), 101.0)).unwrap();
        }
        assert!(t.request(2, req("overflow", 101.0)).is_err());
    }
    #[test]
    fn repeated_accounts_are_recorded_without_cross_channel_union_or_names() {
        let config = Config::new(vec!["test".into(), "other".into()], 2, 420).unwrap();
        let mut s = Schedule::new(config.clone());
        let mut e = RunEvidence::new(&config);
        for i in 0..4 {
            e.start(s.start_due(i * START_GAP_MS).unwrap());
            let mut t = Tracker::new(1, "test".into(), "top".into());
            t.set_trigger(100_000.0).unwrap();
            t.request(1, req("a", 101.0)).unwrap();
            t.response(1, resp("a", Some(false))).unwrap();
            t.finished(1, finish("a")).unwrap();
            let name = if i % 2 == 0 {
                "secret_first"
            } else {
                "secret_second"
            };
            e.sample(
                ParsedSample {
                    count: 100,
                    categories: BTreeMap::new(),
                    accounts: HashSet::from([name.into()]),
                },
                &t.take_ready().unwrap().unwrap(),
            );
            e.finish(i * START_GAP_MS + 1, "sample_received");
            s.finish();
        }
        assert_eq!(e.channels["test"].union_accounts, 1);
        assert_eq!(e.channels["other"].union_accounts, 1);
        assert_eq!(e.attempts[2].same_accounts_as_previous, Some(true));
        assert!(!serde_json::to_string(&e).unwrap().contains("secret_"));
        assert!(!e.fresh_server_samples_proven);
    }
}
