# Offline Community presence validation

This work separates numerical correctness, constructed-truth behavior and independent empirical calibration. The production model remains `community_presence_v1`, `calibration_status: uncalibrated`, `probability_kind: model_based_pointwise`, with a historical smoother and unchanged defaults, eligibility, first-positive support and 0.99/0.01 sensitivity thresholds. No evidence here establishes video viewing or watch time.

## Executable and interfaces

`veylurk-presence-validate` is a synchronous offline executable. It calls the actual production `presence_model::analyze` implementation for simulation. It opens no socket, application database, profile or credential source; reports explicitly contain `network_used: false` and `database_used: false`. Its only filesystem inputs are an explicitly named reference JSON file and its fresh output destination. It refuses to overwrite existing output files. A malformed input can leave an empty reserved destination; a completed JSON report is required evidence of an evaluation.

Hosted Windows CI builds the existing package, including this additional bin target. Do not build Cargo locally in the current GNU environment. From the repository root, the hosted build/test commands are:

```powershell
cargo test --lib presence_model
cargo test --lib presence_validation
cargo test --bin veylurk-presence-validate
cargo build --release --bin veylurk-presence-validate
```

Run a short CI smoke with a fresh output path:

```powershell
target/release/veylurk-presence-validate.exe simulate --trajectories 2 --observations 12 --max-seconds 20 --output presence-smoke.json
target/release/veylurk-presence-validate.exe reference --input fixtures/presence-validation/invented-reference.json --output presence-fixture.json
```

Run the full bounded analysis and record the actual completion status:

```powershell
target/release/veylurk-presence-validate.exe simulate --trajectories 200 --observations 120 --max-seconds 120 --output presence-simulation.json
target/release/veylurk-presence-validate.exe reference --output presence-reference.json
```

The second command records `not_run_missing_independent_reference`; it does not solicit user data. If a qualifying authorized reference exists, pass its local JSON with `reference --input <reference.json> --output <fresh-report.json>`. JSON is also printed to standard output. Missing-reference status exits successfully because it was recorded as requested. A runtime cap exits unsuccessfully after writing an incomplete JSON report. Invalid JSON/schema/provenance exits unsuccessfully and is not a valid report. Bounds are 2–200 total trajectories per scenario, 3–120 observations each and 1–120 seconds; reference input is at most 8 MiB, 2,000 trajectories and 20,000 aggregate target points/cells each. Each trajectory has at most 2,048 points/cells and 1,000 reference intervals.

Public Rust interfaces are `presence_validation::{SimulationConfig, simulate, ReferenceInput, evaluate_reference, missing_reference}`. Both evaluators return `Result<serde_json::Value, String>`. `simulate` takes the bounded configuration; `evaluate_reference` takes strictly deserialized `ReferenceInput`. No application endpoint, database schema or native instrumentation is added.

## Numerical verification

Existing model tests cover segmentation, gaps/failures, unsupported roles, provenance, finite viewport resolution, cancellation and the pre-optimization algorithm comparison. Additional tests enumerate every hidden binary state path for all sixteen four-observation patterns across the production sensitivity scenarios and one extreme scenario. Irregular spacing includes a one-millisecond step; counts and populations vary. The independent oracle uses its own continuous-time transition formula, likelihood multiplication and path summation; it never calls the production transition, likelihood, normalization or forward/backward functions. A separate integration test compares actual production midpoint probabilities, including lag and first-positive censoring, against that oracle.

These tests can detect implementation defects. They cannot establish that real inclusion probabilities, session durations, lag or sample independence follow the model. No numerical defect has been assumed or used to change settings. Formatting and fixture checks are separate from hosted Rust test acceptance; record actual CI results with the candidate revision.

## Frozen simulation protocol

Protocol `presence_validation_v1` uses SplitMix64 with an open-interval 52-bit uniform draw. The decimal development and holdout seed roots are emitted in the report. Within each scenario, alternate whole independent trajectories between development and holdout, with separate seed roots and a fixed scenario/trajectory derivation. A 200-trajectory run therefore has 100 development and 100 untouched holdout trajectories per scenario. No fitting, setting selection or tuning occurs. Once read, this fixed holdout is analysis evidence and cannot serve as a fresh holdout for a tuned future candidate.

Truth is generated first as a continuous-time binary membership path, independently of posterior inference. Central-law truth starts with membership probability 0.5 at 300 seconds before the viewport. Exponential connected/disconnected means are 3,600/43,200 seconds. Receipt times are spaced 300 seconds apart with independent 0–30 second jitter; true population is 100–500, the deduplicated Viewer sample is `min(floor(N/2),150)`, detection scale is 0.5, false-report ratio is 0.01 and response lag is 30 seconds. Positive observations are independent Bernoulli draws conditional on truth in the central-law case. This draws one target account's inclusion, not a complete server population.

The central-law case is named `on_model` for its transition/emission law. It intentionally preserves production first-positive selection and the model's prior reset at that selected receipt; it is not an assertion that the entire selected-data conditioning distribution matches the production prior. Selection effects can therefore be measured even in this case. Every trajectory, including never-detected accounts, remains in the coverage denominator. The 2,048 midpoint grid is fixed by the production viewport rule; the evaluator recreates that same grid when production returns no cells.

Eight declared stress cases cover the requested departures:

| Scenario | Deliberate departure |
| --- | --- |
| `correlated_cached_draws` | Positive/miss value reused in five-draw blocks; an explicit cache flag every seventeenth draw at offset eight is rejected, while unflagged repeated blocks represent possible upstream reuse. Counts can still vary. |
| `unequal_inclusion_wrong_n` | A trajectory-level inclusion multiplier of 0.12 or 1.8, chosen equally, plus reported population three times true population. These mechanisms are intentionally combined rather than claimed separately identifiable. |
| `time_varying_detection` | Detection alternates between 0.05 and 0.95 every eight observations. |
| `stale_positives` | Additional positives can reflect membership 900 seconds earlier. |
| `non_exponential_sessions` | Uniform dwell durations from half to one-and-a-half times each central mean. |
| `brief_visits` | Present spells 8–33 seconds and absent spells 80–280 seconds, including visits between receipts and between midpoint evaluations. |
| `role_changes` | Viewer-role ineligible middle block of ten observations in each thirty-observation cycle; detected privileged roles retain the production unsupported-role behavior. |
| `informative_dropout` | An 80% failure probability conditional on absent state; production segmentation at failed observations remains active. |

Viewer-role ineligible truth points are excluded and counted; the rest retain production unknown/no-result behavior. The source gates are never relaxed to turn an explicit cached observation into evidence. Role changes can cause an entire response to be unsupported, which is a meaningful loss of coverage. Scenarios are interleaved across trajectories so a cap does not preferentially discard the final scenario. The deadline is passed into production inference. Unfinished inference is retained as no-result at every predeclared eligible midpoint; unattempted trajectories and incomplete counts are reported separately. No failing trajectory is rerun or silently removed. The cooperative computation cap stops new inference/bootstrap work; bounded report serialization/filesystem completion follows. It is not a hard filesystem deadline.

## Scores, coverage and uncertainty

Reports give per-scenario and per-split independent trajectory/group counts, requested/attempted/unattempted trajectories, incomplete inference, response statuses, generated observation counts, generator events and production/truth exclusions. Production exclusion reasons count sample/boundary events; `truth_viewer_role_ineligible` counts midpoint exclusions. These have different units and must not be added to estimate a sample failure rate.

For central probabilities, report Brier score, log loss and ten equal-width reliability bins. Brier uses the unmodified probability. Log loss clips probabilities to `[1e-12,1-1e-12]` solely for numerical scoring. The final reliability bin includes 1. Scores are conditional on a numerical probability being returned; counts and coverage disclose this selection.

Strong-present error counts false presence among `estimated_present` decisions. Strong-absent error counts false absence among `estimated_absent` decisions. Their denominators and rates remain separate; a missing class has a null rate, never zero. Coverage counts strong, uncertain, unknown and no-result among eligible labeled target points. Returning unknown/no-result everywhere supplies no evidence for either strong class.

The predeclared target weighting pools the fixed evaluation points. Descriptive 95% percentile intervals use 200 whole-trajectory/dependency-group bootstrap resamples; adjacent cells are never independent binomial trials. The report includes requested/completed/undefined bootstrap replicates, group counts and groups with eligible points. No interval is emitted with fewer than two groups, incomplete bootstrap work or undefined ratio replicates. Sparse/zero-error bootstrap tails cannot certify a 1% error bound; a degenerate interval at zero is descriptive only. No simultaneous tail or multiplicity guarantee is claimed. The full 200-trajectory run has only 100 holdout independent trajectories per scenario, before selective class support, and is not designed to certify either empirical 1% tail.

Stress undercoverage or high error is a finding, not a test failure to hide by changing assumptions. All results remain `simulation_only` and cannot remove the production `uncalibrated` label. The cap status is distinct from a high measured error: the latter is a completed analysis result.

## Independent reference format and invented fixture

`fixtures/presence-validation/reference.schema.json` documents the strict JSON Schema; Rust `deny_unknown_fields` structures enforce the structural schema and the evaluator adds semantic checks. `invented-reference.json` is tiny invented data, with no real account identities or evidence. It exercises independent-group aggregation, development/holdout separation, interval and clock exclusions, strong errors, unknown cells, unsupported roles and episodes with no support. The fixture result is `reference_evaluator_verified`, which identifies the evaluator path and never real calibration. Tests reject malformed ordering, settings, cutoff/blinding, probabilities, class thresholds, circular provenance, unsupported absence and split leakage.

The top level fixes model identity/settings/threshold, one UTC `as_of`, protocol freeze, prediction freeze and label-unblinding times, plus `whole_account_session_and_dependency_group` split provenance. Each trajectory contains opaque alphanumeric/underscore/hyphen IDs, account/channel/session/dependency identity, split, evidence kind/ID, Viewer-role eligibility, independent-source declaration, complete-session-control declaration, a clock bound, a viewport, stable membership/nonmembership intervals with entry/exit uncertainty, predeclared evaluation timestamps and a frozen prediction projection. Every object rejects extra fields. No cookies, tokens, URLs, native account lists or raw browser/chat records are required or accepted as extra properties.

For a real dataset, export the frozen production response's model/settings/threshold/as_of, status/completeness and cells into `prediction`. Explicit `source_eligibility_verified` and `first_positive_support_preserved` assert that its cells came from the unchanged production path, including all unknowns and failures. The evaluator cannot authenticate these assertions or an opaque evidence ID; retain the original local frozen response and provider evidence separately. Do not manufacture labels from the response, a Community positive/miss, returned totals, IRC NAMES/JOIN/PART, player state or viewer-count metadata.

All timestamps must explicitly end in UTC `Z`. Freeze times must satisfy protocol ≤ predictions ≤ label unblinding, and `as_of` ≤ prediction freeze. Intervals are ordered, nonoverlapping and half-open. A target is scored only if its entire clock-error neighborhood lies inside a stable interval after the entry/exit uncertainty margins. Unlabeled and transition/clock exclusions are counted. This excludes ambiguous entry/exit/return spans. Pointwise classifications do not define estimated transition times: the report explicitly marks lead/lag errors unidentified, instead of inventing exact transition estimates. No-result episodes and never-detected accounts still contribute to eligible coverage. Targets without a matching frozen midpoint are no-result.

An account/session/dependency group cannot cross splits. Repeated accounts must use the same dependency group even within one split; overlapping viewports on the same channel must share a dependency group. This is deliberately conservative. Entire declared groups are pooled before resampling. Actual independence, representative sampling, natural-session priors and provenance cannot be proved by JSON validation. Keep later time blocks held out where possible; this bounded evaluator does not enforce a global chronological split because channels can have different blocks.

`independent_membership` requires independently evidenced Viewer-role server membership and, for negative labels, complete simultaneous-session/device control plus a defensible independent absence source. `client_session_proxy` supports descriptive proxy agreement only; it rejects negative global-membership labels even if a client was closed. A client socket opening alone is insufficient positive reference evidence. It cannot become `independent_membership` by changing a field without new independent evidence. Invented fixtures, proxy results and even accepted small reference datasets always retain `uncalibrated`; the latter report `insufficient_reference_data`. No automated empirical tail certification is implemented in this bounded evaluator.

Without admissible independent labels, the required factual result is `not_run_missing_independent_reference`. Future real calibration needs independent membership/absence interval evidence, clock and transition bounds, blinded frozen predictions, representative independent groups, adequate nonzero support for both strong classes and a prespecified uncertainty/multiplicity analysis. No broadcaster/moderator rights, account acquisition, OAuth flow or native instrumentation is introduced to manufacture this prerequisite.

## Acceptance record

Implementation, source formatting, fixture structure, hosted numerical/tests, full bounded simulation and real reference availability are separate gates. Record the actual revision, executable hash, CLI options, output hashes, runtime and completion status in the coordinating evidence report. Until those executions are recorded, this document describes a protocol and implementation, not passed runtime results. Independent empirical 99% membership calibration remains outstanding unless independently defensible evidence and a qualifying analysis establish it.
