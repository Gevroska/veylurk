use super::*;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../fixtures/presence-validation/invented-reference.json"
    ))
    .unwrap()
}
fn evaluate(value: Value) -> Result<Value, String> {
    evaluate_reference(serde_json::from_value(value).map_err(|e| e.to_string())?)
}

#[test]
fn invented_reference_scores_boundaries_unknown_no_support_and_groups() {
    let report = evaluate(fixture()).unwrap();
    assert_eq!(report["classification"], "reference_evaluator_verified");
    assert_eq!(report["calibration_status"], "uncalibrated");
    assert_eq!(report["network_used"], false);
    assert_eq!(report["requested_reference_points"], 21);
    assert_eq!(report["exclusions"]["clock_or_transition_boundary"], 2);
    assert_eq!(report["exclusions"]["viewer_role_ineligible"], 4);
    assert_eq!(report["development"]["eligible_labeled_points"], 7);
    assert_eq!(report["development"]["counts"]["no_result"], 4);
    assert_eq!(report["development"]["counts"]["unknown"], 1);
    assert_eq!(report["development"]["scored_points"], 2);
    let brier = report["development"]["metrics"]["brier_score"]
        .as_f64()
        .unwrap();
    assert!((brier - (0.01_f64.powi(2) + 0.001_f64.powi(2)) / 2.).abs() < 1e-12);
    assert_eq!(report["holdout"]["independent_units"], 2); // Three trajectories, two dependency groups.
    assert_eq!(report["holdout"]["counts"]["strong_present_errors"], 1);
    assert_eq!(
        report["holdout"]["metrics"]["strong_present_error_rate"],
        1.0
    );
    assert!(report["holdout"]["metrics"]["strong_absent_error_rate"].is_null());
    assert_eq!(report["claims"]["empirically_calibrated"], false);
}

#[test]
fn reference_rejects_schema_identity_order_split_and_circular_labels() {
    let edits: Vec<(&str, Value)> = vec![
        ("/schema_version", json!(2)),
        ("/model_version", json!("another")),
        ("/parameters/detection_scale", json!(0.9)),
        ("/threshold", json!(0.98)),
        ("/as_of", json!("2024-01-01T00:02:00+00:00")),
        ("/labels_unblinded_at", json!("2024-01-01T00:00:00Z")),
        (
            "/trajectories/0/provenance/independent_of_community",
            json!(false),
        ),
        (
            "/trajectories/0/provenance/all_sessions_controlled",
            json!(false),
        ),
        (
            "/trajectories/0/provenance/clock_uncertainty_seconds",
            json!(-1),
        ),
        (
            "/trajectories/0/intervals/1/from",
            json!("2024-01-01T00:00:01Z"),
        ),
        (
            "/trajectories/0/evaluated_at/1",
            json!("2024-01-01T00:00:01Z"),
        ),
        (
            "/trajectories/0/prediction/source_eligibility_verified",
            json!(false),
        ),
        (
            "/trajectories/0/prediction/first_positive_support_preserved",
            json!(false),
        ),
        (
            "/trajectories/0/prediction/parameters/lag_seconds",
            json!(60),
        ),
        (
            "/trajectories/0/prediction/cells/1/classification",
            json!("estimated_absent"),
        ),
        (
            "/trajectories/0/prediction/cells/1/presence_probability",
            json!(1.1),
        ),
        (
            "/trajectories/0/prediction/cells/1/evaluated_at",
            json!("2024-01-01T00:00:44Z"),
        ),
        (
            "/trajectories/0/trajectory_id",
            json!("https://not-an-opaque-id"),
        ),
        (
            "/trajectories/2/dependency_group",
            json!("development_group_1"),
        ),
        ("/trajectories/2/account_id", json!("account_1")),
        ("/trajectories/1/account_id", json!("account_1")),
        ("/trajectories/1/channel_id", json!("channel_1")),
    ];
    for (path, value) in edits {
        let mut input = fixture();
        *input.pointer_mut(path).unwrap() = value;
        assert!(evaluate(input).is_err(), "accepted {path}");
    }
    let mut input = fixture();
    input["cookie"] = json!("forbidden_extra_field");
    assert!(evaluate(input).is_err());
    let mut input = fixture();
    input["trajectories"][0]["prediction"]["cells"][3]
        .as_object_mut()
        .unwrap()
        .remove("presence_probability");
    assert!(
        evaluate(input).is_err(),
        "nullable probability must be present explicitly"
    );
}

#[test]
fn proxy_absence_is_rejected_and_proxy_presence_cannot_become_calibration() {
    let mut input = fixture();
    input["artifact_kind"] = json!("client_session_proxy");
    for trajectory in input["trajectories"].as_array_mut().unwrap() {
        trajectory["provenance"]["kind"] = json!("client_session_proxy");
    }
    assert!(evaluate(input.clone()).is_err());
    for trajectory in input["trajectories"].as_array_mut().unwrap() {
        for interval in trajectory["intervals"].as_array_mut().unwrap() {
            interval["present"] = json!(true);
        }
    }
    let report = evaluate(input).unwrap();
    assert_eq!(
        report["classification"],
        "client_session_proxy_agreement_only"
    );
    assert_eq!(report["status"], "insufficient_reference_data");
    assert_eq!(report["calibration_status"], "uncalibrated");
}

#[test]
fn simulation_reuses_production_rules_and_seeds_without_probability_labels() {
    let deadline = Instant::now() + Duration::from_secs(10);
    let a = trajectory("on_model", 42, 12, deadline);
    let b = trajectory("on_model", 42, 12, deadline);
    assert_eq!(
        serde_json::to_value(&a.response).unwrap(),
        serde_json::to_value(&b.response).unwrap()
    );
    for time in [BASE, BASE + 155., BASE + 3000.] {
        assert_eq!(a.truth.at(time), b.truth.at(time));
    }
    let roles = trajectory("role_changes", 42, 40, deadline);
    assert!(roles
        .generated_exclusions
        .contains_key("viewer_role_ineligible"));
    let cached = trajectory("correlated_cached_draws", 42, 25, deadline);
    assert!(cached
        .response
        .excluded_reason_counts
        .contains_key("cache_or_unknown_transport"));
    assert!(simulate(SimulationConfig {
        trajectories: 201,
        observations: 12,
        max_seconds: 1
    })
    .is_err());
    assert_eq!(
        missing_reference()["status"],
        "not_run_missing_independent_reference"
    );
}

#[test]
fn incomplete_inference_keeps_all_points_as_no_result() {
    let t = trajectory("on_model", 42, 12, Instant::now() - Duration::from_secs(1));
    assert!(!t.response.complete);
    let (scores, _) = simulation_scores(&t, "on_model");
    assert_eq!(scores.points, 2048);
    assert_eq!(scores.no_result, 2048);
    assert_eq!(scores.scored, 0);
}

#[test]
fn proper_scores_clip_only_log_loss_and_preserve_separate_tails() {
    let mut s = Scores::default();
    s.point(
        false,
        Some((Some(1.), PresenceClassification::EstimatedPresent)),
    );
    s.point(
        true,
        Some((Some(0.), PresenceClassification::EstimatedAbsent)),
    );
    s.point(true, None);
    s.point(false, Some((None, PresenceClassification::Unknown)));
    assert_eq!(s.brier, 2.);
    assert!(s.log_loss.is_finite());
    assert_eq!(s.metrics()[2], Some(1.));
    assert_eq!(s.metrics()[3], Some(1.));
    assert_eq!(s.points, 4);
    assert_eq!(s.scored, 2);
    let report = summarize(&[s], Instant::now() + Duration::from_secs(1));
    assert!(report["uncertainty"]["intervals"]["brier_score"]["percentile_95_interval"].is_null());
}
