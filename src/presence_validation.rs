//! Offline constructed-truth experiments and strict frozen-reference evaluation.
//! This module opens no socket, database, browser, profile or credential source.
use crate::{
    presence_model::{
        self as model, PresenceClassification, PresenceModelQuery, PresenceModelResponse,
        PresenceParameters, PresenceSampleCounts, PresenceStatus,
    },
    timeline::TimelineRange,
};
use chrono::{DateTime, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

const MODEL: &str = "community_presence_v1";
const PROTOCOL: &str = "presence_validation_v1";
const CLIP: f64 = 1e-12;
const BASE: f64 = 1_704_067_200.;
const BOOTSTRAPS: usize = 200;
const DEVELOPMENT_SEED: u64 = 0x4d6f_6465_6c44_6576;
const HOLDOUT_SEED: u64 = 0x486f_6c64_6f75_7431;
const SCENARIOS: [&str; 9] = [
    "on_model",
    "correlated_cached_draws",
    "unequal_inclusion_wrong_n",
    "time_varying_detection",
    "stale_positives",
    "non_exponential_sessions",
    "brief_visits",
    "role_changes",
    "informative_dropout",
];

#[derive(Clone, Copy, Debug, Serialize)]
pub struct SimulationConfig {
    /// Total trajectories per scenario; alternating whole trajectories form the two splits.
    pub trajectories: usize,
    pub observations: usize,
    pub max_seconds: u64,
}
impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            trajectories: 200,
            observations: 120,
            max_seconds: 120,
        }
    }
}

fn stamp(at: f64) -> String {
    DateTime::from_timestamp_micros((at * 1e6).round() as i64)
        .expect("bounded time")
        .to_rfc3339_opts(SecondsFormat::Micros, true)
}
fn defaults() -> PresenceParameters {
    PresenceModelQuery::default()
        .parameters()
        .expect("defaults")
}
fn utc(value: &str) -> Result<f64, String> {
    if !value.ends_with('Z') {
        return Err("timestamps must explicitly use UTC Z".into());
    }
    model::parse_time(value).map_err(|_| "invalid UTC timestamp".into())
}
fn count(map: &mut BTreeMap<String, usize>, reason: &str) {
    *map.entry(reason.into()).or_default() += 1;
}
fn ratio(n: usize, d: usize) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}

// SplitMix64: algorithm and seeds are part of the frozen experiment protocol.
struct Rng(u64);
impl Rng {
    fn word(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        ((self.word() >> 12) as f64 + 0.5) / 4_503_599_627_370_496.
    }
    fn yes(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

#[derive(Clone, Default)]
struct Bin {
    n: usize,
    probability: f64,
    truth: usize,
}
#[derive(Clone, Default)]
struct Scores {
    points: usize,
    scored: usize,
    present_truth: usize,
    brier: f64,
    log_loss: f64,
    strong_present: usize,
    present_errors: usize,
    strong_absent: usize,
    absent_errors: usize,
    uncertain: usize,
    unknown: usize,
    no_result: usize,
    bins: [Bin; 10],
}
impl Scores {
    fn add(&mut self, other: &Self) {
        self.points += other.points;
        self.scored += other.scored;
        self.present_truth += other.present_truth;
        self.brier += other.brier;
        self.log_loss += other.log_loss;
        self.strong_present += other.strong_present;
        self.present_errors += other.present_errors;
        self.strong_absent += other.strong_absent;
        self.absent_errors += other.absent_errors;
        self.uncertain += other.uncertain;
        self.unknown += other.unknown;
        self.no_result += other.no_result;
        for (a, b) in self.bins.iter_mut().zip(&other.bins) {
            a.n += b.n;
            a.probability += b.probability;
            a.truth += b.truth;
        }
    }
    fn point(&mut self, truth: bool, prediction: Option<(Option<f64>, PresenceClassification)>) {
        self.points += 1;
        self.present_truth += usize::from(truth);
        let Some((probability, classification)) = prediction else {
            self.no_result += 1;
            return;
        };
        match classification {
            PresenceClassification::EstimatedPresent => {
                self.strong_present += 1;
                self.present_errors += usize::from(!truth);
            }
            PresenceClassification::EstimatedAbsent => {
                self.strong_absent += 1;
                self.absent_errors += usize::from(truth);
            }
            PresenceClassification::Uncertain => self.uncertain += 1,
            PresenceClassification::Unknown => self.unknown += 1,
        }
        if let Some(p) = probability {
            self.scored += 1;
            let y = f64::from(u8::from(truth));
            self.brier += (p - y).powi(2);
            let clipped = p.clamp(CLIP, 1. - CLIP);
            self.log_loss -= y * clipped.ln() + (1. - y) * (-clipped).ln_1p();
            let bin = &mut self.bins[((p * 10.).floor() as usize).min(9)];
            bin.n += 1;
            bin.probability += p;
            bin.truth += usize::from(truth);
        }
    }
    fn metrics(&self) -> [Option<f64>; 7] {
        [
            (self.scored > 0).then(|| self.brier / self.scored as f64),
            (self.scored > 0).then(|| self.log_loss / self.scored as f64),
            ratio(self.present_errors, self.strong_present),
            ratio(self.absent_errors, self.strong_absent),
            ratio(self.strong_present + self.strong_absent, self.points),
            ratio(self.unknown, self.points),
            ratio(self.no_result, self.points),
        ]
    }
}

fn summarize(groups: &[Scores], deadline: Instant) -> Value {
    let mut sum = Scores::default();
    for group in groups {
        sum.add(group);
    }
    let names = [
        "brier_score",
        "log_loss",
        "strong_present_error_rate",
        "strong_absent_error_rate",
        "strong_coverage",
        "unknown_coverage",
        "no_result_coverage",
    ];
    let mut draws: [Vec<f64>; 7] = std::array::from_fn(|_| Vec::new());
    let mut rng = Rng(0xb007_57a9);
    let mut replicates = 0;
    if groups.len() >= 2 {
        for _ in 0..BOOTSTRAPS {
            if Instant::now() >= deadline {
                break;
            }
            let mut resample = Scores::default();
            for _ in groups {
                resample.add(&groups[(rng.word() % groups.len() as u64) as usize]);
            }
            for (values, value) in draws.iter_mut().zip(resample.metrics()) {
                if let Some(value) = value {
                    values.push(value);
                }
            }
            replicates += 1;
        }
    }
    let mut uncertainty = BTreeMap::new();
    for (name, values) in names.iter().zip(&mut draws) {
        values.sort_by(f64::total_cmp);
        let interval = if groups.len() >= 2 && values.len() == BOOTSTRAPS {
            Some([
                values[(values.len() as f64 * 0.025).floor() as usize],
                values[((values.len() as f64 * 0.975).floor() as usize).min(values.len() - 1)],
            ])
        } else {
            None
        };
        uncertainty.insert(
            *name,
            json!({"percentile_95_interval": interval,
            "defined_replicates": values.len(), "undefined_replicates": replicates-values.len()}),
        );
    }
    let metrics: BTreeMap<_, _> = names.into_iter().zip(sum.metrics()).collect();
    json!({"independent_units": groups.len(), "independent_units_with_eligible_points": groups.iter().filter(|g|g.points>0).count(), "eligible_labeled_points": sum.points,
        "present_truth_points": sum.present_truth, "scored_points": sum.scored,
        "counts": {"strong_present": sum.strong_present, "strong_present_errors": sum.present_errors,
            "strong_absent": sum.strong_absent, "strong_absent_errors": sum.absent_errors,
            "uncertain": sum.uncertain, "unknown": sum.unknown, "no_result": sum.no_result},
        "coverage":{"strong_present":ratio(sum.strong_present,sum.points),"strong_absent":ratio(sum.strong_absent,sum.points),
            "uncertain":ratio(sum.uncertain,sum.points),"unknown":ratio(sum.unknown,sum.points),
            "no_result":ratio(sum.no_result,sum.points),"numerical_probability":ratio(sum.scored,sum.points)},
        "metrics": metrics, "reliability_bins": sum.bins.iter().enumerate().map(|(i,b)| json!({
            "lower_inclusive": i as f64/10., "upper": (i+1) as f64/10., "upper_inclusive": i==9,
            "count": b.n, "mean_probability": (b.n>0).then(|| b.probability/b.n as f64),
            "observed_fraction": ratio(b.truth,b.n)})).collect::<Vec<_>>(),
        "uncertainty": {"method": "whole_dependency_group_percentile_bootstrap_descriptive",
            "replicates_requested": BOOTSTRAPS, "replicates_completed": replicates,
            "complete": groups.len()<2 || replicates==BOOTSTRAPS, "intervals": uncertainty,
            "warning": "No cell-level binomial confidence; sparse tails and zero-error bootstrap intervals cannot establish a 1% bound."}})
}

fn envelope(kind: &str) -> Value {
    json!({"schema_version": 1, "protocol_version": PROTOCOL, "classification": kind,
        "network_used": false, "database_used": false, "calibration_status": "uncalibrated",
        "model_version": MODEL, "parameters": defaults(), "threshold": 0.99,
        "probability_kind": "model_based_pointwise", "inference": "historical_smoother",
        "log_loss_probability_clip": CLIP, "weighting": "pooled_predeclared_points; resample_whole_dependency_groups",
        "claims": {"empirically_calibrated": false, "video_viewing_or_watch_time": false}})
}

fn response(from: f64, to: f64) -> PresenceModelResponse {
    PresenceModelResponse {
        model_version: MODEL,
        calibration_status: "uncalibrated",
        probability_kind: "model_based_pointwise",
        inference: "historical_smoother",
        threshold: 0.99,
        left_censored: true,
        account: "synthetic_account".into(),
        channel: "synthetic_channel".into(),
        broadcast_id: None,
        effective_range: TimelineRange {
            from: stamp(from),
            to: stamp(to),
        },
        context_range: TimelineRange {
            from: stamp(from - 21600.),
            to: stamp(to),
        },
        as_of: stamp(to),
        generated_at: stamp(to),
        parameters: defaults(),
        sensitivity_scenarios: 0,
        complete: true,
        status: PresenceStatus::NoData,
        sample_counts: PresenceSampleCounts::default(),
        excluded_reason_counts: BTreeMap::new(),
        cells: Vec::new(),
        cell_width_seconds: 0.,
    }
}

fn sample(index: usize, at: f64, positive: bool, n: i64, k: i64) -> model::Sample {
    model::Sample {
        id: index as i64 + 1,
        run_id: 1,
        outcome: "success".into(),
        started: stamp(at - 1.),
        completed: Some(stamp(at)),
        received: Some(stamp(at)),
        reported: Some(n),
        returned: Some(k),
        categories: [Some(0), Some(0), Some(0), Some(0), Some(0), Some(k)],
        post_trigger: Some(true),
        timing: [Some(1.), Some(2.), Some(3.), Some(1.)],
        cache: [Some(false); 4],
        encoded: Some(100),
        transport: Some("network_response".into()),
        http_status: Some(200),
        target_roles: positive.then_some(32),
        unique_members: k,
        viewer_only: k,
    }
}

// Independent continuous-time truth path, generated before sampling or inference.
struct Truth(Vec<(f64, bool)>);
impl Truth {
    fn at(&self, at: f64) -> bool {
        self.0[self.0.partition_point(|(t, _)| *t <= at).saturating_sub(1)].1
    }
}
fn truth_path(rng: &mut Rng, scenario: &str, end: f64) -> Truth {
    let mut state = rng.yes(0.5);
    let mut at = BASE - 300.;
    let mut path = vec![(at, state)];
    while at < end {
        let mean = if state { 3600. } else { 43200. };
        let dwell = if scenario == "non_exponential_sessions" {
            mean * (0.5 + rng.unit())
        } else if scenario == "brief_visits" {
            if state {
                8. + rng.unit() * 25.
            } else {
                80. + rng.unit() * 200.
            }
        } else {
            -rng.unit().ln() * mean
        };
        at += dwell;
        state = !state;
        path.push((at, state));
    }
    Truth(path)
}

struct Trajectory {
    response: PresenceModelResponse,
    truth: Truth,
    observations: usize,
    generated_exclusions: BTreeMap<String, usize>,
}
fn trajectory(scenario: &str, seed: u64, observations: usize, deadline: Instant) -> Trajectory {
    let mut rng = Rng(seed);
    let end = BASE + observations as f64 * 300.;
    let truth = truth_path(&mut rng, scenario, end);
    let mut samples = Vec::with_capacity(observations);
    let mut previous = false;
    let mut generated_exclusions = BTreeMap::new();
    let inclusion_multiplier = if rng.yes(0.5) { 0.12 } else { 1.8 };
    for i in 0..observations {
        // Irregular times, variable known counts, all fixed before scoring cell midpoints.
        let at = BASE + i as f64 * 300. + rng.unit() * 30.;
        let actual_n = 100 + (rng.word() % 401) as i64;
        let k = (actual_n / 2).min(150);
        let n = if scenario == "unequal_inclusion_wrong_n" {
            (actual_n * 3).max(k)
        } else {
            actual_n
        };
        let detection = if scenario == "time_varying_detection" {
            if (i / 8).is_multiple_of(2) {
                0.05
            } else {
                0.95
            }
        } else {
            0.5
        };
        let member = truth.at(at - 30.);
        let multiplier = if scenario == "unequal_inclusion_wrong_n" {
            inclusion_multiplier
        } else {
            1.
        };
        let p = (detection * k as f64 / actual_n as f64 * multiplier).clamp(1e-6, 1. - 1e-6);
        let mut positive = rng.yes(if member { p } else { 0.01 * p });
        if scenario == "stale_positives" && truth.at(at - 900.) {
            positive = rng.yes(p);
        }
        if scenario == "correlated_cached_draws" && !i.is_multiple_of(5) {
            positive = previous;
        }
        previous = positive;
        let mut row = sample(i, at, positive, n, k);
        if scenario == "correlated_cached_draws" && i % 17 == 8 {
            // A declared cache flag must be rejected; other repeated blocks model unflagged server reuse.
            row.cache[0] = Some(true);
            count(&mut generated_exclusions, "declared_cache");
        }
        if scenario == "role_changes" && (i / 10) % 3 == 1 {
            if positive {
                row.target_roles = Some(4);
                row.categories[2] = Some(1);
                row.categories[5] = Some(k - 1);
                row.viewer_only = k - 1;
            }
            count(&mut generated_exclusions, "viewer_role_ineligible");
        }
        if scenario == "informative_dropout" && !member && rng.yes(0.8) {
            row.outcome = "failed".into();
            count(&mut generated_exclusions, "state_dependent_failure");
        }
        samples.push(row);
    }
    let successful = samples.iter().filter(|s| s.outcome == "success").count();
    let result = model::analyze(
        response(BASE, end),
        model::Extraction {
            successful,
            samples,
            limit: false,
            cancelled: false,
        },
        model::Control {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline,
        },
    );
    Trajectory {
        response: result,
        truth,
        observations,
        generated_exclusions,
    }
}

fn simulation_scores(t: &Trajectory, scenario: &str) -> (Scores, BTreeMap<String, usize>) {
    let mut scores = Scores::default();
    let mut exclusions = BTreeMap::new();
    let from = utc(&t.response.effective_range.from).expect("generated time");
    let to = utc(&t.response.effective_range.to).expect("generated time");
    // This reproduces the production viewport grid even when no response cells exist.
    let cells = 2048_usize.min(((to - from) * 1000.).floor().max(1.) as usize);
    for i in 0..cells {
        let at = from + (i as f64 + 0.5) * (to - from) / cells as f64;
        if scenario == "role_changes" && (((at - BASE) / 300.).floor() as usize / 10) % 3 == 1 {
            count(&mut exclusions, "truth_viewer_role_ineligible");
            continue;
        }
        scores.point(
            t.truth.at(at),
            t.response
                .cells
                .get(i)
                .map(|c| (c.presence_probability, c.classification)),
        );
    }
    (scores, exclusions)
}

/// Deterministic except elapsed time/deadline truncation; never tunes the frozen production model.
pub fn simulate(config: SimulationConfig) -> Result<Value, String> {
    if !(2..=200).contains(&config.trajectories)
        || !(3..=120).contains(&config.observations)
        || !(1..=120).contains(&config.max_seconds)
    {
        return Err("simulation exceeds protocol bounds".into());
    }
    let start = Instant::now();
    let deadline = start + Duration::from_secs(config.max_seconds);
    let mut rows = Vec::new();
    let mut complete = true;
    // Interleave scenarios so a time cap does not systematically omit the last stress case.
    let mut groups: Vec<[Vec<Scores>; 2]> = (0..SCENARIOS.len())
        .map(|_| [Vec::new(), Vec::new()])
        .collect();
    let mut failures = vec![0_usize; SCENARIOS.len()];
    let mut attempted = vec![0_usize; SCENARIOS.len()];
    let mut exclusions: Vec<BTreeMap<String, usize>> = vec![BTreeMap::new(); SCENARIOS.len()];
    let mut statuses: Vec<BTreeMap<String, usize>> = vec![BTreeMap::new(); SCENARIOS.len()];
    let mut generated: Vec<BTreeMap<String, usize>> = vec![BTreeMap::new(); SCENARIOS.len()];
    let mut observation_counts = vec![0_usize; SCENARIOS.len()];
    let mut sample_counts = vec![PresenceSampleCounts::default(); SCENARIOS.len()];
    'run: for i in 0..config.trajectories {
        for (s, scenario) in SCENARIOS.iter().enumerate() {
            if Instant::now() >= deadline {
                complete = false;
                break 'run;
            }
            let split = i % 2;
            let seed = if split == 0 {
                DEVELOPMENT_SEED
            } else {
                HOLDOUT_SEED
            };
            let seed = seed
                .wrapping_add((s as u64 + 1).wrapping_mul(0x100000001b3))
                .wrapping_add((i / 2) as u64);
            let t = trajectory(scenario, seed, config.observations, deadline);
            attempted[s] += 1;
            observation_counts[s] += t.observations;
            let totals = &mut sample_counts[s];
            let current = &t.response.sample_counts;
            totals.successful_in_context += current.successful_in_context;
            totals.attempts_in_id_span += current.attempts_in_id_span;
            totals.eligible_observations += current.eligible_observations;
            totals.target_positive_receipts += current.target_positive_receipts;
            totals.segments += current.segments;
            totals.modeled_segments += current.modeled_segments;
            let key = serde_json::to_value(t.response.status).expect("enum");
            count(&mut statuses[s], key.as_str().expect("status"));
            if !t.response.complete {
                failures[s] += 1;
                complete = false;
            }
            for (reason, n) in &t.response.excluded_reason_counts {
                *exclusions[s].entry(reason.clone()).or_default() += n;
            }
            for (reason, n) in &t.generated_exclusions {
                *generated[s].entry(reason.clone()).or_default() += n;
            }
            let (score, excluded) = simulation_scores(&t, scenario);
            for (reason, n) in excluded {
                *exclusions[s].entry(reason).or_default() += n;
            }
            groups[s][split].push(score);
        }
    }
    for (s, scenario) in SCENARIOS.iter().enumerate() {
        let development = summarize(&groups[s][0], deadline);
        let holdout = summarize(&groups[s][1], deadline);
        rows.push(json!({"scenario":scenario,"requested_independent_trajectories":config.trajectories,
            "attempted_trajectories":attempted[s],"unattempted_trajectories":config.trajectories-attempted[s],
            "incomplete_inference_trajectories":failures[s],"generated_observations":observation_counts[s],
            "production_sample_counts":sample_counts[s],"requested_midpoints_in_attempted_trajectories":attempted[s]*2048,
            "response_statuses":statuses[s],"production_and_truth_exclusions":exclusions[s],
            "generator_events":generated[s],"development":development,"holdout":holdout}));
    }
    if Instant::now() >= deadline {
        complete = false;
    }
    let mut report = envelope("simulation_only");
    report["complete"] = json!(complete);
    report["status"] = json!(if complete {
        "completed"
    } else {
        "incomplete_runtime_cap"
    });
    report["config"] = json!(config);
    report["elapsed_seconds"] = json!(start.elapsed().as_secs_f64());
    report["seed_protocol"] = json!({"algorithm":"splitmix64_v1","development_seed":DEVELOPMENT_SEED.to_string(),
        "holdout_seed":HOLDOUT_SEED.to_string(),"assignment":"alternating_whole_trajectories; seeds separated before inference",
        "tuning_performed":false,"holdout_reusable_for_tuning":false});
    report["truth_provenance"] = json!({"kind":"constructed_independent_continuous_time_paths",
        "labels_derived_from_model_posterior":false,"initial_membership_probability":0.5,
        "on_model_means_seconds":[3600,43200],"on_model_detection_scale":0.5,"on_model_false_report_ratio":0.01,
        "on_model_lag_seconds":30,"caveat":"Central generative law at trajectory start; production first-positive support selection and prior reset remain active and may cause selection effects."});
    report["scenarios"] = json!(rows);
    report["scenario_definitions"] = json!({
        "on_model":"Continuous-time exponential on/off truth, means 3600/43200 seconds; independent Bernoulli observations with variable k/N, central detection/false-report/lag settings. First-positive selection remains active.",
        "correlated_cached_draws":"Five-draw repeated-positive blocks despite fresh-request provenance; every seventeenth draw at offset eight is explicitly cache-flagged and rejected by production.",
        "unequal_inclusion_wrong_n":"Trajectory inclusion multiplier 0.12 or 1.8 with equal probability, and reported N three times actual N; effects intentionally combined in this declared stress case.",
        "time_varying_detection":"Detection alternates 0.05/0.95 in eight-observation blocks instead of fixed 0.5.",
        "stale_positives":"Additional positives may reflect membership 900 seconds earlier instead of the nominal 30-second lag.",
        "non_exponential_sessions":"Uniform dwell durations between 0.5 and 1.5 times each central dwell mean.",
        "brief_visits":"Present spells 8-33 seconds, absent spells 80-280 seconds; truth evaluated at all production midpoints, including visits between receipts.",
        "role_changes":"Every middle ten-observation block in a thirty-observation cycle is Viewer-role ineligible; detected privileged roles retain production role-unsupported behavior and ineligible truth points are counted exclusions.",
        "informative_dropout":"80% of observations with absent latent state are failures; production splits segments at these failures."
    });
    report["real_reference"] = missing_reference();
    Ok(report)
}

pub fn missing_reference() -> Value {
    let mut report = envelope("not_run_missing_independent_reference");
    report["complete"] = json!(false);
    report["status"] = json!("not_run_missing_independent_reference");
    report["required_evidence"]=json!(["Independent Viewer-role server membership and defensible absence intervals",
        "Entry/exit/return uncertainty and clock bounds; complete simultaneous-session control for absence",
        "Blinded frozen predictions, model/settings/as_of identity, and whole-session dependency-group holdout splits",
        "Representative independent groups and nonzero support for both strong classes"]);
    report
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Development,
    Holdout,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    InventedFixture,
    IndependentMembership,
    ClientSessionProxy,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceInput {
    pub schema_version: u32,
    pub artifact_kind: ReferenceKind,
    pub model_version: String,
    pub parameters: PresenceParameters,
    pub threshold: f64,
    pub as_of: String,
    pub protocol_frozen_at: String,
    pub predictions_frozen_at: String,
    pub labels_unblinded_at: String,
    pub split_rule: String,
    pub trajectories: Vec<ReferenceTrajectory>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceTrajectory {
    pub trajectory_id: String,
    pub account_id: String,
    pub channel_id: String,
    pub session_id: String,
    pub dependency_group: String,
    pub split: Split,
    pub provenance: ReferenceProvenance,
    pub from: String,
    pub to: String,
    pub intervals: Vec<ReferenceInterval>,
    /// Fixed target timestamps, including points for accounts/episodes never detected.
    pub evaluated_at: Vec<String>,
    pub prediction: FrozenPrediction,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceProvenance {
    pub kind: ReferenceKind,
    pub evidence_id: String,
    pub independent_of_community: bool,
    pub viewer_role_eligible: bool,
    pub all_sessions_controlled: bool,
    pub clock_uncertainty_seconds: f64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceInterval {
    pub from: String,
    pub to: String,
    pub present: bool,
    pub entry_uncertainty_seconds: f64,
    pub exit_uncertainty_seconds: f64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenPrediction {
    pub model_version: String,
    pub parameters: PresenceParameters,
    pub threshold: f64,
    pub as_of: String,
    pub complete: bool,
    pub status: String,
    pub source_eligibility_verified: bool,
    pub first_positive_support_preserved: bool,
    pub cells: Vec<FrozenCell>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenCell {
    pub from: String,
    pub to: String,
    pub evaluated_at: String,
    #[serde(deserialize_with = "required_nullable_probability")]
    pub presence_probability: Option<f64>,
    #[serde(deserialize_with = "required_nullable_probability")]
    pub sensitivity_min: Option<f64>,
    #[serde(deserialize_with = "required_nullable_probability")]
    pub sensitivity_max: Option<f64>,
    pub classification: String,
}

// Custom deserialization intentionally makes an explicit null mandatory. Ordinary
// Option fields would accept an omitted key, contrary to the frozen JSON schema.
fn required_nullable_probability<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    Option::<f64>::deserialize(deserializer)
}

fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn bounded_uncertainty(x: f64) -> bool {
    x.is_finite() && (0. ..=86400.).contains(&x)
}
fn class(value: &str) -> Result<PresenceClassification, String> {
    match value {
        "estimated_present" => Ok(PresenceClassification::EstimatedPresent),
        "estimated_absent" => Ok(PresenceClassification::EstimatedAbsent),
        "uncertain" => Ok(PresenceClassification::Uncertain),
        "unknown" => Ok(PresenceClassification::Unknown),
        _ => Err("invalid classification".into()),
    }
}

/// Validates typed JSON schema and semantic identity/ordering/split/provenance before scoring labels.
pub fn evaluate_reference(input: ReferenceInput) -> Result<Value, String> {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(120);
    if input.schema_version != 1
        || input.model_version != MODEL
        || input.parameters != defaults()
        || input.threshold != 0.99
    {
        return Err("reference protocol/model/settings identity mismatch".into());
    }
    let cutoff = utc(&input.as_of)?;
    let protocol = utc(&input.protocol_frozen_at)?;
    let frozen = utc(&input.predictions_frozen_at)?;
    let unblinded = utc(&input.labels_unblinded_at)?;
    if protocol > frozen
        || frozen > unblinded
        || cutoff > frozen
        || input.split_rule != "whole_account_session_and_dependency_group"
    {
        return Err(
            "predictions, cutoff, labels or split provenance is not frozen in order".into(),
        );
    }
    if input.trajectories.is_empty() || input.trajectories.len() > 2000 {
        return Err("reference trajectory limit".into());
    }
    let mut ids = BTreeSet::new();
    let mut assignments: BTreeMap<String, Split> = BTreeMap::new();
    let mut account_groups: BTreeMap<&str, &str> = BTreeMap::new();
    let mut channel_groups: BTreeMap<&str, Vec<(f64, f64, &str)>> = BTreeMap::new();
    let mut aggregate_points = 0_usize;
    let mut aggregate_cells = 0_usize;
    // Validate every trajectory and both sides of a split before any score is exposed.
    for (validated, t) in input.trajectories.iter().enumerate() {
        if Instant::now() >= deadline {
            let mut report = envelope("incomplete_reference_runtime_cap");
            report["complete"] = json!(false);
            report["status"] = json!("incomplete_runtime_cap");
            report["validated_trajectories"] = json!(validated);
            report["requested_trajectories"] = json!(input.trajectories.len());
            report["validation_complete"] = json!(false);
            report["elapsed_seconds"] = json!(start.elapsed().as_secs_f64());
            return Ok(report);
        }
        for id in [
            &t.trajectory_id,
            &t.account_id,
            &t.channel_id,
            &t.session_id,
            &t.dependency_group,
            &t.provenance.evidence_id,
        ] {
            if !opaque(id) {
                return Err("only bounded opaque identifiers are accepted".into());
            }
        }
        if !ids.insert(&t.trajectory_id) {
            return Err("duplicate trajectory identity".into());
        }
        for key in [
            format!("account:{}", t.account_id),
            format!("session:{}:{}", t.account_id, t.session_id),
            format!("group:{}", t.dependency_group),
        ] {
            if assignments
                .insert(key, t.split)
                .is_some_and(|split| split != t.split)
            {
                return Err("account/session/dependency-group leakage across splits".into());
            }
        }
        let from = utc(&t.from)?;
        let to = utc(&t.to)?;
        aggregate_points += t.evaluated_at.len();
        aggregate_cells += t.prediction.cells.len();
        if aggregate_points > 20_000 || aggregate_cells > 20_000 {
            return Err("reference aggregate point/cell limit".into());
        }
        if account_groups
            .insert(&t.account_id, &t.dependency_group)
            .is_some_and(|group| group != t.dependency_group)
        {
            return Err("repeated account must remain one dependency group".into());
        }
        let channel = channel_groups.entry(&t.channel_id).or_default();
        if channel
            .iter()
            .any(|(left, right, group)| from < *right && *left < to && *group != t.dependency_group)
        {
            return Err("overlapping channel-time spans must share a dependency group".into());
        }
        channel.push((from, to, &t.dependency_group));
        if from >= to
            || to > cutoff
            || !bounded_uncertainty(t.provenance.clock_uncertainty_seconds)
            || t.intervals.len() > 1000
            || t.evaluated_at.len() > 2048
            || t.evaluated_at.is_empty()
        {
            return Err("invalid reference range, clock bound or point limit".into());
        }
        if t.provenance.kind != input.artifact_kind || !t.provenance.independent_of_community {
            return Err(
                "independent reference provenance required; Community cannot label itself".into(),
            );
        }
        let mut previous = from;
        for interval in &t.intervals {
            let left = utc(&interval.from)?;
            let right = utc(&interval.to)?;
            if left < previous
                || left < from
                || left >= right
                || right > to
                || !bounded_uncertainty(interval.entry_uncertainty_seconds)
                || !bounded_uncertainty(interval.exit_uncertainty_seconds)
            {
                return Err("reference intervals overlap or have invalid uncertainty/range".into());
            }
            if !interval.present
                && (!t.provenance.all_sessions_controlled
                    || input.artifact_kind == ReferenceKind::ClientSessionProxy)
            {
                return Err("absence requires independent complete session control; client proxy cannot label global absence".into());
            }
            previous = right;
        }
        let mut last = from - 1.;
        for at in &t.evaluated_at {
            let at = utc(at)?;
            if at <= last || at < from || at >= to {
                return Err("evaluation timestamps are not ordered within range".into());
            }
            last = at;
        }
        let p = &t.prediction;
        if p.model_version != input.model_version
            || p.parameters != input.parameters
            || p.threshold != input.threshold
            || p.as_of != input.as_of
            || !p.source_eligibility_verified
            || !p.first_positive_support_preserved
            || p.cells.len() > 2048
        {
            return Err("frozen prediction identity/eligibility/support mismatch".into());
        }
        let incomplete_status =
            ["input_limit", "analysis_busy", "analysis_timeout"].contains(&p.status.as_str());
        if ![
            "ok",
            "no_data",
            "insufficient_data",
            "role_unsupported",
            "input_limit",
            "analysis_busy",
            "analysis_timeout",
        ]
        .contains(&p.status.as_str())
            || p.complete == incomplete_status
            || ((!p.complete || p.status != "ok") && !p.cells.is_empty())
        {
            return Err("inconsistent prediction result/status".into());
        }
        let mut edge = from;
        for c in &p.cells {
            let left = utc(&c.from)?;
            let right = utc(&c.to)?;
            let at = utc(&c.evaluated_at)?;
            if left < edge
                || left < from
                || right > to
                || left >= right
                || at <= left
                || at >= right
                || (at - (left + right) / 2.).abs() > 2e-6
            {
                return Err(
                    "prediction cells must be ordered nonoverlapping production midpoints".into(),
                );
            }
            edge = right;
            let classification = class(&c.classification)?;
            match (c.presence_probability, c.sensitivity_min, c.sensitivity_max) {
                (Some(prob), Some(min), Some(max))
                    if [prob, min, max]
                        .iter()
                        .all(|x| x.is_finite() && (0. ..=1.).contains(x))
                        && min <= prob
                        && prob <= max =>
                {
                    let expected = if min >= 0.99 {
                        PresenceClassification::EstimatedPresent
                    } else if max <= 0.01 {
                        PresenceClassification::EstimatedAbsent
                    } else {
                        PresenceClassification::Uncertain
                    };
                    if classification != expected {
                        return Err(
                            "classification does not match frozen sensitivity thresholds".into(),
                        );
                    }
                }
                (None, None, None) if classification == PresenceClassification::Unknown => {}
                _ => return Err("invalid or inconsistent frozen probability range".into()),
            }
        }
    }
    let mut grouped: [BTreeMap<String, Scores>; 2] = std::array::from_fn(|_| BTreeMap::new());
    let mut exclusions = BTreeMap::new();
    let mut status_counts = BTreeMap::new();
    let mut total = 0_usize;
    let mut transitions = json!({"uncertain_boundary_points_excluded":0,"lead_lag_error":"not_identified_from_pointwise_predictions",
        "reason":"Pointwise historical probabilities do not define estimated transition times; report interval-censored boundary exclusions instead."});
    for t in &input.trajectories {
        count(&mut status_counts, &t.prediction.status);
        let split = usize::from(t.split == Split::Holdout);
        let score = grouped[split]
            .entry(t.dependency_group.clone())
            .or_default();
        for at in &t.evaluated_at {
            if Instant::now() >= deadline {
                let mut report = envelope("incomplete_reference_runtime_cap");
                report["complete"] = json!(false);
                report["status"] = json!("incomplete_runtime_cap");
                report["processed_reference_points"] = json!(total);
                report["exclusions"] = json!(exclusions);
                return Ok(report);
            }
            total += 1;
            let time = utc(at)?;
            if !t.provenance.viewer_role_eligible {
                count(&mut exclusions, "viewer_role_ineligible");
                continue;
            }
            let nominal = t.intervals.iter().find(|interval| {
                utc(&interval.from).expect("validated") <= time
                    && time < utc(&interval.to).expect("validated")
            });
            let Some(interval) = nominal else {
                count(&mut exclusions, "unlabeled_interval");
                continue;
            };
            let clock = t.provenance.clock_uncertainty_seconds;
            if time - clock < utc(&interval.from)? + interval.entry_uncertainty_seconds
                || time + clock >= utc(&interval.to)? - interval.exit_uncertainty_seconds
            {
                count(&mut exclusions, "clock_or_transition_boundary");
                continue;
            }
            let prediction = t
                .prediction
                .cells
                .iter()
                .find(|cell| (utc(&cell.evaluated_at).expect("validated") - time).abs() <= 1e-6)
                .map(|cell| {
                    Ok::<_, String>((cell.presence_probability, class(&cell.classification)?))
                })
                .transpose()?;
            score.point(interval.present, prediction);
        }
    }
    transitions["uncertain_boundary_points_excluded"] = json!(exclusions
        .get("clock_or_transition_boundary")
        .copied()
        .unwrap_or(0));
    let kind = match input.artifact_kind {
        ReferenceKind::InventedFixture => "reference_evaluator_verified",
        ReferenceKind::IndependentMembership => "independent_reference_descriptive",
        ReferenceKind::ClientSessionProxy => "client_session_proxy_agreement_only",
    };
    let mut report = envelope(kind);
    report["complete"] = json!(true);
    report["status"] = json!(if input.artifact_kind == ReferenceKind::InventedFixture {
        "reference_evaluator_verified"
    } else {
        "insufficient_reference_data"
    });
    report["reference_kind"] = json!(input.artifact_kind);
    report["real_server_membership_calibration_status"] = json!("not_established");
    report["as_of"] = json!(input.as_of);
    report["requested_reference_points"] = json!(total);
    report["trajectories"] = json!(input.trajectories.len());
    report["response_statuses"] = json!(status_counts);
    report["exclusions"] = json!(exclusions);
    report["transition_stratum"] = transitions;
    report["development"] = summarize(&grouped[0].values().cloned().collect::<Vec<_>>(), deadline);
    report["holdout"] = summarize(&grouped[1].values().cloned().collect::<Vec<_>>(), deadline);
    if Instant::now() >= deadline {
        report["complete"] = json!(false);
        report["status"] = json!("incomplete_runtime_cap");
    }
    report["elapsed_seconds"] = json!(start.elapsed().as_secs_f64());
    report["uncertainty_assumptions"]=json!(["Dependency groups are declared independent by the reference provider, not proven by schema validation",
        "Opaque evidence IDs and blinding timestamps are provenance assertions; evaluator cannot authenticate them",
        "Pooled points define this fixture/dataset domain; representativeness and both 1% tails remain unproven",
        "Invented fixtures and client-session proxies cannot calibrate server membership"]);
    Ok(report)
}

#[cfg(test)]
#[path = "presence_validation_tests.rs"]
mod tests;
