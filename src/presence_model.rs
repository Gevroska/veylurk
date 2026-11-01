//! Conditional, uncalibrated Community receipt model. Never a viewing or departure record.
use crate::{
    store::StoreError,
    timeline::{Timeline, TimelineRange},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

pub(crate) const MAX_SAMPLES: usize = 50_000;
pub(crate) const MAX_ID_SPAN: i64 = 200_000;
pub(crate) const MAX_MEMBERS: i64 = 5_000_000;
const MAX_CELLS: usize = 2048;
static ANALYSIS_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceModelQuery {
    pub account: String,
    pub channel: Option<String>,
    pub broadcast_id: Option<i64>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub connected_minutes: Option<f64>,
    pub disconnected_minutes: Option<f64>,
    pub detection_scale: Option<f64>,
    pub false_report_ratio: Option<f64>,
    pub lag_seconds: Option<f64>,
    pub max_gap_seconds: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct PresenceParameters {
    pub connected_minutes: f64,
    pub disconnected_minutes: f64,
    pub detection_scale: f64,
    pub false_report_ratio: f64,
    pub lag_seconds: f64,
    pub max_gap_seconds: f64,
}

impl PresenceModelQuery {
    pub(crate) fn parameters(&self) -> Result<PresenceParameters, StoreError> {
        fn value(input: Option<f64>, default: f64, min: f64, max: f64) -> Result<f64, StoreError> {
            let value = input.unwrap_or(default);
            if value.is_finite() && (min..=max).contains(&value) {
                Ok(value)
            } else {
                Err(StoreError::Invalid)
            }
        }
        Ok(PresenceParameters {
            connected_minutes: value(self.connected_minutes, 60., 5., 1440.)?,
            disconnected_minutes: value(self.disconnected_minutes, 720., 30., 10080.)?,
            detection_scale: value(self.detection_scale, 0.5, 0.05, 1.)?,
            false_report_ratio: value(self.false_report_ratio, 0.01, 0.0001, 0.1)?,
            lag_seconds: value(self.lag_seconds, 30., 0., 300.)?,
            max_gap_seconds: value(self.max_gap_seconds, 3600., 60., 21600.)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceStatus {
    Ok,
    NoData,
    InsufficientData,
    RoleUnsupported,
    InputLimit,
    AnalysisBusy,
    AnalysisTimeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceClassification {
    EstimatedPresent,
    EstimatedAbsent,
    Uncertain,
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PresenceSampleCounts {
    pub successful_in_context: usize,
    pub attempts_in_id_span: usize,
    pub eligible_observations: usize,
    pub target_positive_receipts: usize,
    pub segments: usize,
    pub modeled_segments: usize,
}

#[derive(Debug, Serialize)]
pub struct PresenceCell {
    pub from: String,
    pub to: String,
    pub evaluated_at: String,
    pub presence_probability: Option<f64>,
    pub sensitivity_min: Option<f64>,
    pub sensitivity_max: Option<f64>,
    pub classification: PresenceClassification,
    pub unknown_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct PresenceModelResponse {
    pub model_version: &'static str,
    pub calibration_status: &'static str,
    pub probability_kind: &'static str,
    pub inference: &'static str,
    pub threshold: f64,
    /// Every modeled segment begins at its first eligible positive; arrival is unknown.
    pub left_censored: bool,
    pub account: String,
    pub channel: String,
    pub broadcast_id: Option<i64>,
    pub effective_range: TimelineRange,
    pub context_range: TimelineRange,
    pub as_of: String,
    pub generated_at: String,
    pub parameters: PresenceParameters,
    pub sensitivity_scenarios: usize,
    pub complete: bool,
    pub status: PresenceStatus,
    pub sample_counts: PresenceSampleCounts,
    pub excluded_reason_counts: BTreeMap<String, usize>,
    pub cells: Vec<PresenceCell>,
    pub cell_width_seconds: f64,
}

pub(crate) fn parse_time(value: &str) -> Result<f64, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.timestamp_micros() as f64 / 1_000_000.)
        .map_err(|_| StoreError::Invalid)
}

fn format_time(seconds: f64) -> Result<String, StoreError> {
    if !seconds.is_finite() {
        return Err(StoreError::Invalid);
    }
    DateTime::from_timestamp_micros((seconds * 1_000_000.).round() as i64)
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Micros, true))
        .ok_or(StoreError::Invalid)
}

impl PresenceModelResponse {
    pub(crate) fn new(
        account: String,
        axis: Timeline,
        parameters: PresenceParameters,
    ) -> Result<Self, StoreError> {
        let start = DateTime::parse_from_rfc3339(&axis.effective_range.from)
            .map_err(|_| StoreError::Invalid)?
            .with_timezone(&Utc)
            .checked_sub_signed(Duration::hours(6))
            .ok_or(StoreError::Invalid)?;
        let context_range = TimelineRange {
            from: start.to_rfc3339_opts(SecondsFormat::Millis, true),
            to: axis.as_of.clone(),
        };
        Ok(Self {
            model_version: "community_presence_v1",
            calibration_status: "uncalibrated",
            probability_kind: "model_based_pointwise",
            inference: "historical_smoother",
            threshold: 0.99,
            left_censored: true,
            account,
            channel: axis.channel.ok_or(StoreError::Invalid)?,
            broadcast_id: axis.broadcast.map(|b| b.id),
            effective_range: axis.effective_range,
            context_range,
            as_of: axis.as_of,
            generated_at: axis.generated_at,
            parameters,
            sensitivity_scenarios: 0,
            complete: true,
            status: PresenceStatus::NoData,
            sample_counts: PresenceSampleCounts::default(),
            excluded_reason_counts: BTreeMap::new(),
            cells: Vec::new(),
            cell_width_seconds: 0.,
        })
    }
    pub(crate) fn incomplete(mut self, status: PresenceStatus) -> Self {
        self.status = status;
        self.complete = false;
        self.cells.clear();
        self.sensitivity_scenarios = 0;
        self
    }
}

pub(crate) struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        ANALYSIS_ACTIVE.store(false, Ordering::Release);
    }
}
pub(crate) fn try_permit() -> Option<Arc<Permit>> {
    ANALYSIS_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| Arc::new(Permit))
}
pub(crate) struct CancellationGuard(pub Arc<AtomicBool>);
impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
pub(crate) struct Control {
    pub cancelled: Arc<AtomicBool>,
    pub deadline: Instant,
}
impl Control {
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }
}

#[cfg(test)]
pub(crate) async fn test_serial_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    SERIAL.lock().await
}

#[derive(Clone, Debug)]
pub(crate) struct Sample {
    pub id: i64,
    pub run_id: i64,
    pub outcome: String,
    pub started: String,
    pub completed: Option<String>,
    pub received: Option<String>,
    pub reported: Option<i64>,
    pub returned: Option<i64>,
    pub categories: [Option<i64>; 6],
    pub post_trigger: Option<bool>,
    pub timing: [Option<f64>; 4],
    pub cache: [Option<bool>; 4],
    pub encoded: Option<i64>,
    pub transport: Option<String>,
    pub http_status: Option<i64>,
    pub target_roles: Option<i64>,
    pub unique_members: i64,
    pub viewer_only: i64,
}

#[derive(Default)]
pub(crate) struct Extraction {
    pub successful: usize,
    pub samples: Vec<Sample>,
    pub limit: bool,
    pub cancelled: bool,
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    at: f64,
    positive: bool,
    k: f64,
    n: f64,
}

fn eligibility(
    sample: &Sample,
    context_start: f64,
    context_end: f64,
) -> Result<Observation, &'static str> {
    if sample.outcome != "success" {
        return Err("non_success");
    }
    let at = sample
        .received
        .as_deref()
        .and_then(|v| parse_time(v).ok())
        .ok_or("invalid_timestamp")?;
    if at < context_start || at >= context_end {
        return Err("outside_context");
    }
    let started = parse_time(&sample.started).map_err(|_| "invalid_timestamp")?;
    let completed = sample
        .completed
        .as_deref()
        .and_then(|v| parse_time(v).ok())
        .ok_or("invalid_timestamp")?;
    if started > at || completed != at {
        return Err("invalid_timestamp");
    }
    if sample.http_status != Some(200) || sample.post_trigger != Some(true) {
        return Err("unverified_native_request");
    }
    if sample.transport.as_deref() != Some("network_response")
        || sample.cache.iter().any(|v| *v != Some(false))
        || sample.encoded.is_none_or(|v| v <= 0)
    {
        return Err("cache_or_unknown_transport");
    }
    let [Some(after), Some(response), Some(finished), Some(delta)] = sample.timing else {
        return Err("invalid_native_timing");
    };
    if [after, response, finished, delta]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 86_400_000.)
        || after < 0.
        || response < 0.
        || finished < 0.
        || ((finished - response) - delta).abs() > 0.001
    {
        return Err("invalid_native_timing");
    }
    let (Some(n), Some(returned)) = (sample.reported, sample.returned) else {
        return Err("invalid_population");
    };
    if n <= 0
        || returned < 0
        || n < returned
        || returned > 4096
        || sample.unique_members != returned
    {
        return Err("invalid_population");
    }
    if sample
        .categories
        .iter()
        .any(|c| c.is_none_or(|v| !(0..=4096).contains(&v)))
    {
        return Err("invalid_categories");
    }
    let total: i64 = sample.categories.iter().flatten().sum();
    if total > 4096
        || total < returned
        || sample.viewer_only <= 0
        || sample.viewer_only > sample.categories[5].unwrap_or(0)
    {
        return Err("invalid_categories");
    }
    Ok(Observation {
        at,
        positive: sample.target_roles == Some(32),
        k: sample.viewer_only as f64,
        n: n as f64,
    })
}

fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.into()).or_default() += 1;
}

fn segments(
    input: &Extraction,
    response: &mut PresenceModelResponse,
    control: &Control,
) -> Result<Vec<Vec<Observation>>, ()> {
    let start = parse_time(&response.context_range.from).map_err(|_| ())?;
    let end = parse_time(&response.context_range.to).map_err(|_| ())?;
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut previous: Option<(i64, f64)> = None;
    let finish = |current: &mut Vec<Observation>,
                  result: &mut Vec<Vec<Observation>>,
                  counts: &mut PresenceSampleCounts| {
        if current.is_empty() {
            return;
        }
        counts.segments += 1;
        if let Some(first) = current.iter().position(|o| o.positive) {
            if current.len() - first >= 3 {
                result.push(current.drain(first..).collect());
                counts.modeled_segments += 1;
            }
        }
        current.clear();
    };
    for sample in &input.samples {
        if control.stopped() {
            return Err(());
        }
        let receipt = sample.received.as_deref().and_then(|v| parse_time(v).ok());
        if sample.outcome == "success" && receipt.is_some_and(|at| at >= start && at < end) {
            if sample.target_roles.is_some() {
                response.sample_counts.target_positive_receipts += 1;
            }
            if sample.target_roles.is_some_and(|roles| roles != 32) {
                response.status = PresenceStatus::RoleUnsupported;
            }
        }
        match eligibility(sample, start, end) {
            Ok(observation) => {
                response.sample_counts.eligible_observations += 1;
                if let Some((run, at)) = previous {
                    let reason = if run != sample.run_id {
                        Some("run_change")
                    } else if observation.at <= at {
                        Some("nonincreasing_time")
                    } else if observation.at - at > response.parameters.max_gap_seconds {
                        Some("gap")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        increment(&mut response.excluded_reason_counts, reason);
                        finish(&mut current, &mut result, &mut response.sample_counts);
                    }
                }
                previous = Some((sample.run_id, observation.at));
                current.push(observation);
            }
            Err(reason) => {
                increment(&mut response.excluded_reason_counts, reason);
                finish(&mut current, &mut result, &mut response.sample_counts);
                previous = None;
            }
        }
    }
    finish(&mut current, &mut result, &mut response.sample_counts);
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Scenario {
    connected: f64,
    disconnected: f64,
    detection: f64,
    ratio: f64,
    prior: f64,
    lag: f64,
}
fn scenarios(p: PresenceParameters) -> Vec<Scenario> {
    let mut values = vec![Scenario {
        connected: p.connected_minutes * 60.,
        disconnected: p.disconnected_minutes * 60.,
        detection: p.detection_scale,
        ratio: p.false_report_ratio,
        prior: 0.5,
        lag: p.lag_seconds,
    }];
    for mask in 0..64 {
        let value = Scenario {
            connected: p.connected_minutes * 60. * if mask & 1 == 0 { 0.5 } else { 2. },
            disconnected: p.disconnected_minutes * 60. * if mask & 2 == 0 { 0.5 } else { 2. },
            detection: (p.detection_scale * if mask & 4 == 0 { 0.5 } else { 2. }).clamp(0.05, 1.),
            ratio: (p.false_report_ratio * if mask & 8 == 0 { 0.1 } else { 10. })
                .clamp(0.0001, 0.1),
            prior: if mask & 16 == 0 { 0.1 } else { 0.9 },
            lag: if mask & 32 == 0 {
                0.
            } else {
                2. * p.lag_seconds
            },
        };
        if !values.contains(&value) {
            values.push(value);
        }
    }
    values
}

fn transition(s: Scenario, dt: f64) -> [[f64; 2]; 2] {
    let a = 1. / s.disconnected;
    let b = 1. / s.connected;
    let pi = a / (a + b);
    let changed = -(-(a + b) * dt.max(0.)).exp_m1();
    [
        [1. - pi * changed, pi * changed],
        [(1. - pi) * changed, 1. - (1. - pi) * changed],
    ]
}
fn normalize(v: [f64; 2]) -> Option<[f64; 2]> {
    let total = v[0] + v[1];
    (total.is_finite() && total > 0. && v.iter().all(|p| p.is_finite() && *p >= 0.))
        .then(|| [v[0] / total, v[1] / total])
}
fn likelihood(o: Observation, s: Scenario) -> [f64; 2] {
    let p = (s.detection * o.k / o.n).clamp(1e-6, 1. - 1e-6);
    let q = s.ratio * p;
    if o.positive {
        [q, p]
    } else {
        [1. - q, 1. - p]
    }
}
fn forward(v: [f64; 2], t: [[f64; 2]; 2]) -> [f64; 2] {
    [
        v[0] * t[0][0] + v[1] * t[1][0],
        v[0] * t[0][1] + v[1] * t[1][1],
    ]
}
fn backward(v: [f64; 2], t: [[f64; 2]; 2]) -> [f64; 2] {
    [
        t[0][0] * v[0] + t[0][1] * v[1],
        t[1][0] * v[0] + t[1][1] * v[1],
    ]
}
fn product(a: [f64; 2], b: [f64; 2]) -> Option<[f64; 2]> {
    normalize([a[0] * b[0], a[1] * b[1]])
}

struct Smoother {
    filtered: Vec<[f64; 2]>,
    future: Vec<[f64; 2]>,
}
impl Smoother {
    fn new(observations: &[Observation], s: Scenario, control: &Control) -> Option<Self> {
        let mut filtered = Vec::with_capacity(observations.len());
        let mut state = [1. - s.prior, s.prior];
        for (i, o) in observations.iter().enumerate() {
            if control.stopped() {
                return None;
            }
            if i > 0 {
                state = forward(state, transition(s, o.at - observations[i - 1].at));
            }
            state = product(state, likelihood(*o, s))?;
            filtered.push(state);
        }
        let mut future = vec![[0.5, 0.5]; observations.len()];
        for i in (0..observations.len() - 1).rev() {
            if control.stopped() {
                return None;
            }
            future[i] = normalize(backward(
                product(future[i + 1], likelihood(observations[i + 1], s))?,
                transition(s, observations[i + 1].at - observations[i].at),
            ))?;
        }
        Some(Self { filtered, future })
    }
    fn at(&self, obs: &[Observation], s: Scenario, at: f64) -> Option<f64> {
        if at < obs.first()?.at || at > obs.last()?.at {
            return None;
        }
        let index = obs.partition_point(|o| o.at <= at).checked_sub(1)?;
        if index + 1 == obs.len() {
            return product(self.filtered[index], self.future[index]).map(|v| v[1]);
        }
        let left = forward(self.filtered[index], transition(s, at - obs[index].at));
        let right = backward(
            product(self.future[index + 1], likelihood(obs[index + 1], s))?,
            transition(s, obs[index + 1].at - at),
        );
        product(left, right).map(|v| v[1])
    }
}

pub(crate) fn analyze(
    mut response: PresenceModelResponse,
    input: Extraction,
    control: Control,
) -> PresenceModelResponse {
    response.sensitivity_scenarios = 0;
    response.sample_counts.successful_in_context = input.successful;
    response.sample_counts.attempts_in_id_span = input.samples.len();
    if input.limit {
        return response.incomplete(PresenceStatus::InputLimit);
    }
    if input.cancelled || control.stopped() {
        return response.incomplete(PresenceStatus::AnalysisTimeout);
    }
    let segments = match segments(&input, &mut response, &control) {
        Ok(value) => value,
        Err(()) => return response.incomplete(PresenceStatus::AnalysisTimeout),
    };
    if response.status == PresenceStatus::RoleUnsupported {
        return response;
    }
    if input.successful == 0 {
        return response;
    }
    if segments.is_empty() {
        response.status = PresenceStatus::InsufficientData;
        return response;
    }
    let (Ok(from), Ok(to)) = (
        parse_time(&response.effective_range.from),
        parse_time(&response.effective_range.to),
    ) else {
        return response.incomplete(PresenceStatus::AnalysisTimeout);
    };
    if from >= to {
        return response;
    }
    // Millisecond input bounds need no finer cell density; microsecond output keeps
    // every midpoint distinct from its edges even for the shortest valid viewport.
    let count = MAX_CELLS.min(((to - from) * 1_000.).floor().max(1.) as usize);
    let width = (to - from) / count as f64;
    response.cell_width_seconds = width;
    let mut central = vec![None; count];
    let mut minimum = vec![1_f64; count];
    let mut maximum = vec![0_f64; count];
    let mut supported = vec![true; count];
    let scenarios = scenarios(response.parameters);
    let scenario_count = scenarios.len();
    for (scenario_index, scenario) in scenarios.into_iter().enumerate() {
        if control.stopped() {
            return response.incomplete(PresenceStatus::AnalysisTimeout);
        }
        let mut probabilities = vec![None; count];
        let mut matches = vec![0_u8; count];
        let mut boundary = vec![false; count];
        for observations in &segments {
            if control.stopped() {
                return response.incomplete(PresenceStatus::AnalysisTimeout);
            }
            let start = observations[0].at - scenario.lag;
            let end = observations.last().unwrap().at - scenario.lag;
            for edge in [start, end] {
                if edge > from && edge < to {
                    let index = ((edge - from) / width).floor().min((count - 1) as f64) as usize;
                    if from + index as f64 * width < edge
                        && edge < from + (index + 1) as f64 * width
                    {
                        boundary[index] = true;
                    }
                }
            }
            let first = ((start - from) / width).ceil().clamp(0., count as f64) as usize;
            let last = ((end - from) / width).floor().clamp(0., count as f64) as usize;
            if first >= last {
                continue;
            }
            let Some(smoother) = Smoother::new(observations, scenario, &control) else {
                if control.stopped() {
                    return response.incomplete(PresenceStatus::AnalysisTimeout);
                }
                // Unsupported numerical scenarios never contribute an invented probability.
                continue;
            };
            for index in first..last {
                if index.is_multiple_of(64) && control.stopped() {
                    return response.incomplete(PresenceStatus::AnalysisTimeout);
                }
                let left = from + index as f64 * width;
                let right = from + (index + 1) as f64 * width;
                if left < start || right > end {
                    continue;
                }
                matches[index] = matches[index].saturating_add(1);
                probabilities[index] = smoother.at(
                    observations,
                    scenario,
                    from + (index as f64 + 0.5) * width + scenario.lag,
                );
            }
        }
        for index in 0..count {
            if matches[index] != 1 || boundary[index] {
                supported[index] = false;
                continue;
            }
            let Some(probability) = probabilities[index] else {
                supported[index] = false;
                continue;
            };
            if scenario_index == 0 {
                central[index] = Some(probability);
            }
            minimum[index] = minimum[index].min(probability);
            maximum[index] = maximum[index].max(probability);
        }
    }
    response.sensitivity_scenarios = scenario_count;
    for index in 0..count {
        if control.stopped() {
            return response.incomplete(PresenceStatus::AnalysisTimeout);
        }
        let fields = [
            from + index as f64 * width,
            from + (index + 1) as f64 * width,
            from + (index as f64 + 0.5) * width,
        ]
        .map(format_time);
        let [Ok(from), Ok(to), Ok(evaluated_at)] = fields else {
            return response.incomplete(PresenceStatus::AnalysisTimeout);
        };
        let valid = supported[index] && central[index].is_some();
        response.cells.push(PresenceCell {
            from,
            to,
            evaluated_at,
            presence_probability: if valid { central[index] } else { None },
            sensitivity_min: valid.then_some(minimum[index]),
            sensitivity_max: valid.then_some(maximum[index]),
            classification: if !valid {
                PresenceClassification::Unknown
            } else if minimum[index] >= 0.99 {
                PresenceClassification::EstimatedPresent
            } else if maximum[index] <= 0.01 {
                PresenceClassification::EstimatedAbsent
            } else {
                PresenceClassification::Uncertain
            },
            unknown_reason: (!valid).then_some("outside_support_or_segment_boundary"),
        });
    }
    response.status = PresenceStatus::Ok;
    response
}

#[cfg(test)]
#[path = "presence_model_tests.rs"]
mod tests;
