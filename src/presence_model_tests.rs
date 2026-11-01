use super::*;
use std::time::Duration as StdDuration;
const BASE: f64 = 1_704_067_200.;

fn parameters() -> PresenceParameters {
    PresenceModelQuery::default().parameters().unwrap()
}
fn control() -> Control {
    Control {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline: Instant::now() + StdDuration::from_secs(2),
    }
}
fn sample(index: i64, positive: bool) -> Sample {
    let at = format_time(BASE + index as f64 * 60.).unwrap();
    Sample {
        id: index + 1,
        run_id: 1,
        outcome: "success".into(),
        started: format_time(BASE + index as f64 * 60. - 10.).unwrap(),
        completed: Some(at.clone()),
        received: Some(at),
        reported: Some(100),
        returned: Some(2),
        categories: [Some(0), Some(0), Some(0), Some(0), Some(0), Some(2)],
        post_trigger: Some(true),
        timing: [Some(10.), Some(50.), Some(40.), Some(-10.)],
        cache: [Some(false); 4],
        encoded: Some(100),
        transport: Some("network_response".into()),
        http_status: Some(200),
        target_roles: positive.then_some(32),
        unique_members: 2,
        viewer_only: 2,
    }
}
fn response(from: f64, to: f64) -> PresenceModelResponse {
    PresenceModelResponse {
        model_version: "community_presence_v1",
        calibration_status: "uncalibrated",
        probability_kind: "model_based_pointwise",
        inference: "historical_smoother",
        threshold: 0.99,
        left_censored: true,
        account: "alice".into(),
        channel: "room".into(),
        broadcast_id: None,
        effective_range: TimelineRange {
            from: format_time(BASE + from).unwrap(),
            to: format_time(BASE + to).unwrap(),
        },
        context_range: TimelineRange {
            from: format_time(BASE + from - 21600.).unwrap(),
            to: format_time(BASE + to).unwrap(),
        },
        as_of: format_time(BASE + to).unwrap(),
        generated_at: format_time(BASE + to).unwrap(),
        parameters: parameters(),
        sensitivity_scenarios: scenarios(parameters()).len(),
        complete: true,
        status: PresenceStatus::NoData,
        sample_counts: PresenceSampleCounts::default(),
        excluded_reason_counts: BTreeMap::new(),
        cells: Vec::new(),
        cell_width_seconds: 0.,
    }
}
fn input(samples: Vec<Sample>) -> Extraction {
    Extraction {
        successful: samples.iter().filter(|s| s.outcome == "success").count(),
        samples,
        limit: false,
        cancelled: false,
    }
}

#[test]
fn static_miss_example_requires_seventeen_independent_trials_at_quarter_detection() {
    let static_miss = |detection: f64, count: i32| (1. - detection).powi(count);
    assert!(static_miss(0.25, 16) > 0.01);
    assert!(static_miss(0.25, 17) < 0.01);
    // This example is not an arrival/departure inference or the dynamic model's threshold.
    assert!((static_miss(0.25, 16) - 0.010022595757618546).abs() < 1e-15);
}

fn exhaustive(obs: &[Observation], scenario: Scenario, at: f64) -> f64 {
    let mut events: Vec<_> = obs.iter().map(|o| (o.at, Some(*o))).collect();
    events.push((at, None));
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let target = events.iter().position(|e| e.1.is_none()).unwrap();
    let mut total = 0.;
    let mut present = 0.;
    for path in 0..(1_usize << events.len()) {
        let first = path & 1;
        let mut weight = if first == 1 {
            scenario.prior
        } else {
            1. - scenario.prior
        };
        for (i, (time, observation)) in events.iter().enumerate() {
            let state = (path >> i) & 1;
            if i > 0 {
                weight *=
                    transition(scenario, *time - events[i - 1].0)[(path >> (i - 1)) & 1][state];
            }
            if let Some(observation) = observation {
                weight *= likelihood(*observation, scenario)[state];
            }
        }
        total += weight;
        if (path >> target) & 1 == 1 {
            present += weight;
        }
    }
    present / total
}

// Independent oracle: deliberately do not call production transition, likelihood,
// forward/backward or normalization. Enumerate every binary hidden-state path.
fn independent_oracle(obs: &[Observation], s: Scenario, at: f64) -> f64 {
    let mut times: Vec<f64> = obs.iter().map(|o| o.at).chain([at]).collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    let target = times.iter().position(|t| *t == at).unwrap();
    let stationary = s.connected / (s.connected + s.disconnected);
    let mut total = 0.;
    let mut present = 0.;
    for path in 0..(1_usize << times.len()) {
        let first = path & 1;
        let mut weight = if first == 1 { s.prior } else { 1. - s.prior };
        for (i, time) in times.iter().enumerate() {
            let state = (path >> i) & 1;
            if i > 0 {
                let previous = (path >> (i - 1)) & 1;
                let persistence =
                    (-(1. / s.connected + 1. / s.disconnected) * (time - times[i - 1])).exp();
                let p1 = stationary + (previous as f64 - stationary) * persistence;
                weight *= if state == 1 { p1 } else { 1. - p1 };
            }
            for o in obs.iter().filter(|o| o.at == *time) {
                let detect = (s.detection * o.k / o.n).clamp(1e-6, 1. - 1e-6);
                let positive = if state == 1 { detect } else { detect * s.ratio };
                weight *= if o.positive { positive } else { 1. - positive };
            }
        }
        total += weight;
        if (path >> target) & 1 == 1 {
            present += weight;
        }
    }
    present / total
}

#[test]
fn independent_exhaustive_oracle_covers_patterns_irregular_counts_and_parameter_corners() {
    let mut cases = scenarios(parameters());
    cases.push(Scenario {
        connected: 300.,
        disconnected: 604800.,
        detection: 0.05,
        ratio: 0.0001,
        prior: 0.1,
        lag: 0.,
    });
    for scenario in cases {
        for pattern in 0..16 {
            let obs: Vec<_> = [
                (0., 1., 10000.),
                (0.001, 90., 100.),
                (137., 8., 500.),
                (790., 100., 100.),
            ]
            .into_iter()
            .enumerate()
            .map(|(i, (at, k, n))| Observation {
                at,
                positive: pattern & (1 << i) != 0,
                k,
                n,
            })
            .collect();
            let smoother = Smoother::new(&obs, scenario, &control()).unwrap();
            for at in [0., 0.0005, 0.001, 10., 137., 500., 790.] {
                let expected = independent_oracle(&obs, scenario, at);
                let actual = smoother.at(&obs, scenario, at).unwrap();
                assert!(
                    (expected - actual).abs() < 2e-10,
                    "pattern {pattern} at {at}: {expected} != {actual}"
                );
            }
        }
    }
}

#[test]
fn production_grid_central_probabilities_match_independent_oracle_after_lag_and_censoring() {
    let mut rows = vec![
        sample(0, false),
        sample(1, true),
        sample(2, false),
        sample(3, true),
    ];
    rows[2].reported = Some(700);
    rows[3].viewer_only = 1;
    let out = analyze(response(0., 240.), input(rows.clone()), control());
    assert_eq!(out.status, PresenceStatus::Ok);
    assert!(out.left_censored);
    let obs: Vec<_> = rows[1..]
        .iter()
        .map(|s| eligibility(s, BASE - 1., BASE + 241.).unwrap())
        .collect();
    let central = scenarios(parameters())[0];
    let mut checked = 0;
    for cell in &out.cells {
        if let Some(actual) = cell.presence_probability {
            let at = parse_time(&cell.evaluated_at).unwrap() + central.lag;
            let expected = independent_oracle(&obs, central, at);
            assert!((expected - actual).abs() < 2e-10);
            checked += 1;
        }
    }
    assert!(checked > 100);
    assert!(out
        .cells
        .iter()
        .any(|c| c.classification == PresenceClassification::Unknown));
}

#[test]
fn forward_backward_and_between_observation_estimates_match_exhaustive_paths() {
    let scenario = scenarios(parameters())[0];
    let obs = [
        Observation {
            at: 0.,
            positive: true,
            k: 30.,
            n: 100.,
        },
        Observation {
            at: 40.,
            positive: false,
            k: 20.,
            n: 100.,
        },
        Observation {
            at: 130.,
            positive: true,
            k: 10.,
            n: 200.,
        },
    ];
    let smoother = Smoother::new(&obs, scenario, &control()).unwrap();
    for at in [0., 10., 40., 100., 130.] {
        assert!(
            (smoother.at(&obs, scenario, at).unwrap() - exhaustive(&obs, scenario, at)).abs()
                < 1e-12,
            "{at}"
        );
    }
}

#[test]
fn synthetic_stay_exit_return_and_variable_population_have_expected_direction() {
    let scenario = Scenario {
        connected: 300.,
        disconnected: 1800.,
        detection: 1.,
        ratio: 0.01,
        prior: 0.5,
        lag: 0.,
    };
    let obs: Vec<_> = (0..60)
        .map(|i| Observation {
            at: i as f64 * 30.,
            positive: !(10..50).contains(&i),
            k: 75.,
            n: 100.,
        })
        .collect();
    let smoother = Smoother::new(&obs, scenario, &control()).unwrap();
    assert!(smoother.at(&obs, scenario, 150.).unwrap() > 0.99);
    assert!(smoother.at(&obs, scenario, 900.).unwrap() < 0.01);
    assert!(smoother.at(&obs, scenario, 1650.).unwrap() > 0.99);
    let mut pair = vec![
        obs[0],
        Observation {
            at: 60.,
            positive: false,
            k: 75.,
            n: 100.,
        },
    ];
    let low = Smoother::new(&pair, scenario, &control())
        .unwrap()
        .at(&pair, scenario, 60.)
        .unwrap();
    pair[1].n = 10000.;
    let high = Smoother::new(&pair, scenario, &control())
        .unwrap()
        .at(&pair, scenario, 60.)
        .unwrap();
    assert!(
        high > low,
        "larger assumed population must make a miss less informative"
    );
    assert_eq!(transition(scenario, 0.), [[1., 0.], [0., 1.]]);
    assert!((transition(scenario, 1e8)[0][1] - 1. / 7.).abs() < 1e-12);
}

#[test]
fn normalization_remains_finite_under_long_low_likelihood_sequences() {
    let scenario = Scenario {
        connected: 300.,
        disconnected: 604800.,
        detection: 0.05,
        ratio: 0.0001,
        prior: 0.1,
        lag: 0.,
    };
    let observations: Vec<_> = (0_usize..2000)
        .map(|i| Observation {
            at: i as f64,
            positive: i.is_multiple_of(7),
            k: 1.,
            n: 1e12,
        })
        .collect();
    let result = Smoother::new(&observations, scenario, &control()).unwrap();
    for at in [0., 200., 1000., 1999.] {
        let p = result.at(&observations, scenario, at).unwrap();
        assert!(p.is_finite() && (0.0..=1.0).contains(&p));
    }
}

#[test]
fn eligibility_distinguishes_deduplicated_viewers_role_changes_and_unknown_provenance() {
    let good = sample(0, true);
    assert!(eligibility(&good, BASE - 1., BASE + 1.).is_ok());
    let mut duplicates = good.clone();
    duplicates.categories[5] = Some(3);
    assert_eq!(
        eligibility(&duplicates, BASE - 1., BASE + 1.).unwrap().k,
        2.
    );
    for edit in 0..10 {
        let mut row = good.clone();
        match edit {
            0 => row.cache[0] = Some(true),
            1 => row.cache[1] = None,
            2 => row.encoded = None,
            3 => row.transport = Some("unknown".into()),
            4 => row.post_trigger = Some(false),
            5 => row.timing[2] = Some(f64::INFINITY),
            6 => row.reported = Some(1),
            7 => row.viewer_only = 0,
            8 => row.categories[5] = Some(1),
            _ => row.completed = None,
        }
        assert!(eligibility(&row, BASE - 1., BASE + 1.).is_err(), "{edit}");
    }
    let mut privileged = sample(1, true);
    privileged.target_roles = Some(36);
    let result = analyze(
        response(-1., 181.),
        input(vec![good, privileged, sample(2, true)]),
        control(),
    );
    assert_eq!(result.status, PresenceStatus::RoleUnsupported);
    assert!(result.complete && result.cells.is_empty());
}

#[test]
fn segment_support_never_crosses_failures_runs_backwards_times_or_large_gaps() {
    for boundary in 0..4 {
        let mut samples: Vec<_> = (0..7).map(|i| sample(i, true)).collect();
        match boundary {
            0 => {
                samples[3].outcome = "failed".into();
                samples[3].received = None;
            }
            1 => {
                for row in &mut samples[3..] {
                    row.run_id = 2;
                }
            }
            2 => {
                samples[3].received = samples[2].received.clone();
                samples[3].completed = samples[2].completed.clone();
                samples[3].started = samples[2].started.clone();
            }
            _ => {
                for (i, row) in samples[3..].iter_mut().enumerate() {
                    let at = format_time(BASE + 7200. + i as f64 * 60.).unwrap();
                    row.started = at.clone();
                    row.completed = Some(at.clone());
                    row.received = Some(at);
                }
            }
        }
        let mut result = response(-1., 8000.);
        let split = segments(&input(samples), &mut result, &control()).unwrap();
        assert_eq!(split.len(), 2, "{boundary}");
        assert_eq!(result.sample_counts.modeled_segments, 2);
    }
    let samples = (0..5).map(|i| sample(i, i >= 3)).collect();
    let result = analyze(response(-1., 301.), input(samples), control());
    assert_eq!(result.status, PresenceStatus::InsufficientData);
    assert!(
        result.cells.is_empty(),
        "two post-anchor samples cannot provide support"
    );
}

#[test]
fn sensitivity_grid_is_bounded_pointwise_and_never_extrapolates() {
    let result = analyze(
        response(-60., 360.),
        input((0..6).map(|i| sample(i, true)).collect()),
        control(),
    );
    assert_eq!(result.status, PresenceStatus::Ok);
    assert!(result.complete);
    assert_eq!(result.cells.len(), MAX_CELLS);
    assert_eq!(result.sensitivity_scenarios, 65);
    assert!(result.left_censored);
    assert!(result.cells.first().unwrap().presence_probability.is_none());
    assert!(result.cells.last().unwrap().presence_probability.is_none());
    let mut supported = 0;
    for cell in &result.cells {
        assert!(parse_time(&cell.from).unwrap() < parse_time(&cell.evaluated_at).unwrap());
        assert!(parse_time(&cell.evaluated_at).unwrap() < parse_time(&cell.to).unwrap());
        if let Some(center) = cell.presence_probability {
            supported += 1;
            assert!(
                cell.sensitivity_min.unwrap() <= center && center <= cell.sensitivity_max.unwrap()
            );
            if cell.classification == PresenceClassification::EstimatedPresent {
                assert!(cell.sensitivity_min.unwrap() >= 0.99);
            }
            if cell.classification == PresenceClassification::EstimatedAbsent {
                assert!(cell.sensitivity_max.unwrap() <= 0.01);
            }
        } else {
            assert_eq!(cell.classification, PresenceClassification::Unknown);
            assert!(cell.sensitivity_min.is_none() && cell.sensitivity_max.is_none());
        }
    }
    assert!(supported > 0);
    let mut p = parameters();
    p.lag_seconds = 0.;
    assert_eq!(scenarios(p).len(), 33);
}

#[test]
fn shortest_viewport_still_reports_a_distinct_midpoint_and_unknown_support() {
    let result = analyze(
        response(-30., -29.999),
        input((-3..0).map(|i| sample(i, true)).collect()),
        control(),
    );
    assert_eq!(result.status, PresenceStatus::Ok);
    assert_eq!(result.cells.len(), 1);
    let cell = &result.cells[0];
    assert!(parse_time(&cell.from).unwrap() < parse_time(&cell.evaluated_at).unwrap());
    assert!(parse_time(&cell.evaluated_at).unwrap() < parse_time(&cell.to).unwrap());
    assert_eq!(cell.classification, PresenceClassification::Unknown);
}

#[test]
fn limits_cancellation_and_deadlines_never_return_partial_probability_cells() {
    let mut capped = input(vec![sample(0, true)]);
    capped.limit = true;
    let result = analyze(response(0., 100.), capped, control());
    assert_eq!(result.status, PresenceStatus::InputLimit);
    assert!(!result.complete && result.cells.is_empty());
    for cancelled in [true, false] {
        let stop = Control {
            cancelled: Arc::new(AtomicBool::new(cancelled)),
            deadline: Instant::now(),
        };
        let result = analyze(
            response(0., 200.),
            input((0..3).map(|i| sample(i, true)).collect()),
            stop,
        );
        assert_eq!(result.status, PresenceStatus::AnalysisTimeout);
        assert!(!result.complete && result.cells.is_empty());
    }
}

#[test]
fn parameter_bounds_and_unknown_fields_are_rejected() {
    assert!(serde_json::from_str::<PresenceModelQuery>(
        r#"{"account":"alice","channel":"room","extra":1}"#
    )
    .is_err());
    for bad in [f64::NAN, f64::INFINITY, -1., 0.01, 1.01] {
        let query = PresenceModelQuery {
            detection_scale: Some(bad),
            ..PresenceModelQuery::default()
        };
        assert!(query.parameters().is_err());
    }
    for p in [
        PresenceModelQuery {
            connected_minutes: Some(4.99),
            ..Default::default()
        },
        PresenceModelQuery {
            disconnected_minutes: Some(10081.),
            ..Default::default()
        },
        PresenceModelQuery {
            false_report_ratio: Some(0.),
            ..Default::default()
        },
        PresenceModelQuery {
            lag_seconds: Some(301.),
            ..Default::default()
        },
        PresenceModelQuery {
            max_gap_seconds: Some(59.),
            ..Default::default()
        },
    ] {
        assert!(p.parameters().is_err());
    }
}

#[tokio::test]
async fn admission_remains_held_by_actual_work_after_request_cancellation() {
    let _serial = test_serial_guard().await;
    let permit = try_permit().unwrap();
    let held_by_job = permit.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancellationGuard(cancelled.clone());
    drop(permit);
    drop(guard);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(try_permit().is_none());
    drop(held_by_job);
    assert!(try_permit().is_some());
}

fn assert_lag_reuse_equivalent(expected: &PresenceModelResponse, actual: &PresenceModelResponse) {
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.complete, expected.complete);
    assert_eq!(actual.sensitivity_scenarios, expected.sensitivity_scenarios);
    assert_eq!(
        actual.cell_width_seconds.to_bits(),
        expected.cell_width_seconds.to_bits()
    );
    assert_eq!(actual.cells.len(), expected.cells.len());
    for (index, (actual, expected)) in actual.cells.iter().zip(&expected.cells).enumerate() {
        assert_eq!(
            actual.classification, expected.classification,
            "cell {index}"
        );
        assert_eq!(
            actual.unknown_reason, expected.unknown_reason,
            "cell {index}"
        );
        for (actual, expected) in [
            (actual.presence_probability, expected.presence_probability),
            (actual.sensitivity_min, expected.sensitivity_min),
            (actual.sensitivity_max, expected.sensitivity_max),
        ] {
            assert_eq!(
                actual.map(f64::to_bits),
                expected.map(f64::to_bits),
                "cell {index}"
            );
        }
    }
    assert!(
        serde_json::to_value(actual).unwrap() == serde_json::to_value(expected).unwrap(),
        "full response differs from the frozen scenario-by-scenario algorithm"
    );
}

#[test]
fn lag_reuse_matches_frozen_passes_bit_for_bit_across_counts_gaps_and_clock_overlap() {
    for complex in [false, true] {
        let times: Vec<(f64, i64)> = if complex {
            vec![
                (-180., 1),
                (-120., 1),
                (-60., 1),
                (0., 1),
                (60., 1),
                (120., 1),
                (180., 1),
                (240., 1),
                (300., 1),
                (360., 1),
                (420., 1),
                (300., 1),
                (360., 1),
                (420., 1),
                (480., 1),
                (5000., 1),
                (5060., 1),
                (5120., 1),
                (5180., 2),
                (5240., 2),
                (5300., 2),
            ]
        } else {
            (0..20).map(|i| (i as f64 * 60., 1)).collect()
        };
        let samples: Vec<_> = times
            .into_iter()
            .enumerate()
            .map(|(i, (at, run))| {
                let mut row = sample(i as i64, complex || !(4..14).contains(&i));
                let stamp = format_time(BASE + at + 0.125).unwrap();
                row.started = stamp.clone();
                row.completed = Some(stamp.clone());
                row.received = Some(stamp);
                row.run_id = run;
                row.reported = Some(50 + i as i64 * 37);
                let viewers = 1 + (i % 5) as i64;
                row.returned = Some(viewers);
                row.unique_members = viewers;
                row.viewer_only = viewers;
                row.categories[5] = Some(viewers);
                if complex && i == 6 {
                    row.outcome = "failed".into();
                    row.received = None;
                    row.target_roles = None;
                }
                row
            })
            .collect();
        for lag in [0., 30., 17.125, 300.] {
            let make_response = || {
                let mut result = response(-240.25, if complex { 5400.125 } else { 1200.125 });
                result.parameters.lag_seconds = lag;
                if complex {
                    // Clipped sensitivity corners still retain every distinct scenario.
                    result.parameters.detection_scale = 0.05;
                    result.parameters.false_report_ratio = 0.1;
                }
                result
            };
            let expected =
                analyze_before_lag_reuse(make_response(), input(samples.clone()), control());
            let actual = analyze(make_response(), input(samples.clone()), control());
            assert_eq!(actual.status, PresenceStatus::Ok);
            if !complex || lag <= 30. {
                assert!(actual
                    .cells
                    .iter()
                    .any(|cell| cell.presence_probability.is_some()));
            }
            assert!(actual
                .cells
                .iter()
                .any(|cell| cell.classification == PresenceClassification::Unknown));
            assert_lag_reuse_equivalent(&expected, &actual);
        }
    }
}

#[test]
fn lag_reuse_preserves_early_outcomes_boundary_cells_and_numerical_non_support() {
    for case in 0..7 {
        let make_response = || {
            let mut result = if case == 4 {
                response(-30., -29.999)
            } else {
                response(-60., 360.)
            };
            if case == 5 {
                // The public query rejects this value; exercise the internal numerical failure path.
                result.parameters.false_report_ratio = f64::MAX;
            }
            result
        };
        let make_input = || {
            let mut rows: Vec<_> = (0..6).map(|i| sample(i, i != 2)).collect();
            match case {
                0 => rows.clear(),
                1 => rows.truncate(2),
                2 => rows[1].target_roles = Some(36),
                4 => rows = (-3..0).map(|i| sample(i, true)).collect(),
                _ => {}
            }
            let mut result = input(rows);
            result.limit = case == 3;
            result.cancelled = case == 6;
            result
        };
        let expected = analyze_before_lag_reuse(make_response(), make_input(), control());
        let actual = analyze(make_response(), make_input(), control());
        if case == 5 {
            assert_eq!(actual.status, PresenceStatus::Ok);
            assert!(actual
                .cells
                .iter()
                .all(|cell| cell.classification == PresenceClassification::Unknown));
        }
        assert_lag_reuse_equivalent(&expected, &actual);
    }
}

#[test]
fn lag_reuse_builds_one_smoother_per_model_without_dropping_scenarios() {
    for lag in [0., 30., 17.125] {
        let make_response = || {
            let mut result = response(60., 241.);
            result.parameters.lag_seconds = lag;
            result
        };
        let make_input = || input((0..5).map(|i| sample(i, true)).collect());
        SMOOTHER_BUILDS.with(|count| count.set(0));
        let expected = analyze_before_lag_reuse(make_response(), make_input(), control());
        let old_builds = SMOOTHER_BUILDS.with(|count| count.get());
        SMOOTHER_BUILDS.with(|count| count.set(0));
        let actual = analyze(make_response(), make_input(), control());
        let new_builds = SMOOTHER_BUILDS.with(|count| count.get());
        assert_eq!(old_builds, if lag == 0. { 33 } else { 65 });
        assert_eq!(new_builds, 33);
        assert_eq!(actual.sensitivity_scenarios, old_builds);
        assert_lag_reuse_equivalent(&expected, &actual);
    }
    let stop = Control {
        cancelled: Arc::new(AtomicBool::new(true)),
        deadline: Instant::now() + StdDuration::from_secs(2),
    };
    assert!(group_scenarios(&scenarios(parameters()), &stop).is_err());
}

// Frozen pre-optimization algorithm from dd81ebea: keep scenario passes independent here.
fn analyze_before_lag_reuse(
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
