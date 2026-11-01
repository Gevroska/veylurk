# Completion plan: cleanup classification, storage stalls, Community scale and model validation

Date: 2026-11-01 (Europe/Paris). Execution is in progress; the ledger below records the current acceptance boundaries.

The owner authorized execution of these four items after this plan. The coordinating agent owns integration, runtime operations, CI, evidence and final acceptance. This document is an execution checklist, not evidence that the work has passed. Update the status ledger as results become available; keep failures and unavailable evidence visible.

## 1. Outcomes and boundaries

| Item | Required outcome | Honest completion boundary |
| --- | --- | --- |
| Cleanup classification | A newly observed unconfirmed browser cleanup persists an accurate pause reason across restart and still prevents autonomous restart. | Code, migration, coordinator/API/UI regressions and isolated restart acceptance; no claim that cleanup failures cannot recur. |
| R: storage stalls | Separate application queue waiting, active storage work and independent volume delays; implement the smallest justified improvement to responsiveness and diagnosis. | Observability and bounded query behavior can be verified. The historical storage cause remains unresolved without discriminating evidence. |
| Many-channel Community performance | Run a short, paced native Community trial over 24 channels and report actual attempts, accepted receipts, resource use and responsiveness. | Configuring 24 channels, IRC activity or synthetic 50-channel tests alone do not establish a completed Community circuit. |
| The 99% model | Validate the numerical implementation, evaluate simulations with known constructed truth, and make a reproducible held-out reference-data evaluator available. Evaluate real data only if independently defensible labels exist. | Real-world calibration stays outstanding if independent labels or adequate independent sample size are missing. Video viewing is outside the estimand. |

Non-goals: production deployment, a long soak test, changing Windows/Storage Spaces/Syncthing settings, moving the live database, installing software, acquiring broadcaster/moderator access, importing browser cookies, bypassing Twitch denials, relaxing player/authentication/provenance/deadline checks, or promising complete viewers/watch time. No automatic Resume, partial-write replay, reduced SQLite durability, extra database connection, enlarged collector buffer or destructive recovery is authorized by this plan.

All code, UI, reports and notes remain English. Retain the existing WebView2 runtime and dedicated website session. A Play control remains diagnostic; signed-in chat, Community availability, a video and every video paused for two seconds remain mandatory. Challenge, media, source identity, network provenance and pacing checks remain unchanged.

## 2. Factual baseline and preservation record

Repository baseline is documentation HEAD `3a224ba85c4c0ff85d25115aecd0e3a5d31f770e`; the current application code/build is `fb48e0e843bf0c50006494a96dcea1a2d3b64673`. Record the actual candidate commit and executable hash separately.

| Surface | Baseline / rule |
| --- | --- |
| Production | Port 8795, PID 30924, `R:\Programmes\Outils\veylurk\veylurk.exe`. Never modify, restart, stop or use its database for this work. Verify process identity without unnecessary database reads. |
| Isolated application | Port 8796; initial PID 26440 and instance `26440-2026-11-01T13:47:12.442Z`. Revalidate immediately before every control operation. |
| Approved test executable | `outputs/veylurk-app-autocomplete/veylurk.exe` relative to the parent task workspace, not the `work/veylurk` Git checkout. Replace only this approved path after clean stop; a genuinely new Internet-connected executable path still needs the existing firewall gate. |
| Test data | Package `data/community-integration-test.sqlite3`; encrypted metadata credentials remain DPAPI protected. Resolve the actual absolute database path from `/api/runtime` before use. |
| Website session | `outputs/veylurk-community-webview2-proof/data/browser-profile` relative to the parent task workspace. Keep this profile, sign-in and its ownership checks. |
| Settings | Snapshot exact tracked/enabled channels, collection policy, Community enabled state, pause reason, cadence mode and interval. Root reports four tracked channels with only `normanajari` and `strano` enabled. Recent intended cadence is Adaptive/300; restoration uses the snapshot, not an assumed default. |
| Recent health | Root reported fresh successful samples and healthy collection at 15:28 Paris; the older `verified_player_pause_lost` diagnostic is not a current failure. At 15:32, root again verified test and production identity. These are point-in-time observations. |

The existing Windows application workflow `41310526731` passed 110 Node tests, 233 ordinary library tests, isolated storage recovery, synthetic scale tests and packaged operations. Native workflow `41310526730` passed 70 tests. These establish the baseline, not acceptance of the next patch.

Existing live Community proof covers two channels. Existing synthetic 10/25/50-channel cases primarily exercise IRC/history storage and recovery. They do not measure navigation, browser resources, serial Community rotation or Twitch availability across dozens of channels.

The current model is `community_presence_v1`, with a historical smoother, at most 65 sensitivity scenarios and pointwise thresholds. API metadata explicitly says `uncalibrated` and `model_based_pointwise`. No independent reference-session calibration set has been established.

R: is an NTFS Storage Spaces volume of about 6 TB, with about 311 GB free at the earlier inspection. Severe delays around 14:35–14:46 Paris affected durable application work, independent report writes and an offline approximately 23 MB history read; a disk sample reported 20 queued operations. Later access recovered, a clean exit and offline history/hash comparison passed, and collection resumed. Read-only Syncthing evidence showed scans around 14:36 and 14:43–14:45 and cleaning around 14:39:45–14:40:22. Temporal overlap is not causation. No directly configured Syncthing root covered the test/production paths, but reparse-point resolution was incomplete. Root's 15:32 sample reported queue 4, about 188 KB/s reads, no writes and 48% disk activity; one sample is neither a recurrence nor an exoneration.

## 3. Ownership and execution order

The planning assignment ends after writing this document. Use three bounded implementation workers if useful; the root remains the sole operator of the shared runtime, CI and rollout. Do not share file ownership.

| Owner | Owned paths and deliverable | Interface with other owners |
| --- | --- | --- |
| Cleanup worker | `src/community.rs`, `src/community_tests.rs`, `src/community_cadence_tests.rs`, `src/store_community.rs`, `src/store_community_tests.rs`; pause enum, migration and coordinator behavior. Native source changes only if evidence makes them necessary, with ownership explicitly extended first. | Give root the exact additive status field and pause-code contract; do not edit shared UI or workflows. |
| Storage worker | `src/store.rs`, `src/store_diagnostics_tests.rs`, `src/diagnostics.rs`, `src/app.rs`, `scripts/Measure-Veylurk.ps1`, `scripts/Test-VeylurkMeasurement.ps1`; bounded in-memory storage activity and a read-only status surface, plus justified query-wait behavior. | Keep tests in existing modules to avoid `src/lib.rs` conflicts. Supply root UI field semantics; do not change Community storage/migration code. |
| Model-validation worker | `src/presence_model.rs`, `src/presence_model_tests.rs`, new `src/presence_validation.rs`, new `src/bin/presence_validate.rs`, `src/lib.rs`, `Cargo.toml` only if needed, `fixtures/presence-validation/`, and `docs/community-presence-validation.md`. | Reuse production inference, keep production defaults/claims unchanged, avoid new dependencies where practical. Supply root exact CI invocation and bounded runtime. |
| Root | `web/app.js`, `web/app.test.cjs`, optional `web/index.html`/`web/style.css`, `.github/workflows/windows.yml`, other docs, evidence reports and this checklist. | Integrate contracts once; review cross-module effects, run one combined candidate build, operate all live experiments and preserve data. |

Recommended sequence:

1. Freeze identities/settings/evidence destinations and make a verified isolated consistent backup before channel/configuration mutations. The preparatory snapshot used SQLite's online backup API from a read-only source into an offline-verified C: copy, avoiding an extra browser restart and its known cleanup issue. It passed integrity checks with no pending sample. A clean-stop offline backup remains mandatory before the schema migration or executable replacement. Start one bounded baseline many-channel run on the current release if the volume is responsive and it will improve comparison; this can overlap code work, not another runtime test.
2. Implement the cleanup correction, storage instrumentation/query behavior and model evaluator in independent owned paths. Root integrates the small UI and workflow changes.
3. Review the actual combined diff, concentrating on persistence ordering, migration rollback, abandoned reads, unchanged durability and statistical claims. Resolve findings with the same owners. Use a fresh reviewer for this substantial change as required by repository guidance.
4. Run available local Node/PowerShell checks and local formatting. Submit one integrated revision for hosted Windows MSVC checks/build. Rebuild only for a material correction or failed gate.
5. Root performs one same-path isolated replacement with a verified offline backup, then migration/restart acceptance and one candidate many-channel run. Run the offline model evaluator from the same revision.
6. Restore exact test settings/channel membership, verify fresh samples and preservation, and publish separate statuses for all four items. Do not introduce routine approval pauses between these stages.

No local Cargo builds/tests: the local GNU toolchain lacks its native dependencies. The available standalone formatter is `work/rust-toolchain-local/rustup/toolchains/stable-x86_64-pc-windows-gnu/bin/rustfmt.exe` relative to the parent workspace; use `--edition 2021 --config skip_children=true` on owned edited Rust files. Hosted CI remains the native Rust acceptance surface.

## 4. Item A: accurate persisted cleanup pause

### Implementation

1. Add `CommunityPauseReason::CleanupUnconfirmed`, serialized as `cleanup_unconfirmed`. Keep existing reason values readable. This is a persistence contract change: increment Community schema v2 to v3 and migrate the single-row pause table's CHECK constraint atomically. Do not merely edit released v1 DDL or assume `CREATE TABLE IF NOT EXISTS` changes an existing constraint.
2. Support fresh -> v1 -> v2 -> v3, v1 -> v2 -> v3, v2 -> v3 and idempotent v3 reopen. Preserve original settings, pause values, sample/member/run IDs, member rows, indexes and sequence state. Reject unknown/corrupt versions or invalid retained values before committing recovery/migration. Preserve released v1 and v2 definitions for real migration fixtures.
3. Route `cleanup_unconfirmed` explicitly before the generic `source_denied` fallback. Cover both `HostEvent::Stopped { cleanup_confirmed: false }` and the `retire()` timeout path. The latter currently sets only an in-memory pause: persist the latch there too before subsequent terminal writes can fail or the process exits.
4. Preserve an already persisted specific source-denial reason when a subsequent cleanup failure occurs; it remains the primary persisted stop cause. Current-run cleanup status must still say unconfirmed. With no prior source barrier, persist the new cleanup reason. Document and test this precedence rather than letting event order overwrite a known authentication/integrity/challenge/rate-limit denial.
5. Keep persistence ahead of cancelled/failed attempt completion and run closure wherever cleanup uncertainty is first known. A persistence failure follows existing fail-stop behavior; it must not clear the pause or start another host. Do not claim guaranteed persistence when the disk cannot commit.
6. Keep in-process `cleanup_confirmed == false` blocking Login, Resume and new host creation. Restart loads the persisted reason as paused and starts no browser. After the old test process/profile ownership is externally verified clear, explicit Resume follows the existing cleanup boundary and clears the persisted latch. Enabling settings, reopening sign-in and application restart do not implicitly Resume.
7. Add an additive `persisted_pause_reason` status field if needed to distinguish a retained restart barrier from transient `last_error`. Initial/restarted cleanup reason must not fabricate current-run primary failure, cleanup telemetry or host-termination timestamps. UI text should explain “Browser cleanup was not confirmed; automatic restart is paused.”
8. Keep old `source_denied` values unchanged: their historical cause is unknowable. Label them as a retained generic source pause whose original cause was not recorded; do not claim Twitch rejected the request and do not relabel them retrospectively as cleanup failures.

### Acceptance cases

- Idle and pending terminal cleanup failure; ordinary shutdown, Disable, Resume retirement and unexpected stop paths.
- Host disappears only after the retirement deadline, without a terminal event: persisted cleanup barrier survives reopen.
- Denial followed by cancellation/cleanup failure retains the denial; the primary diagnostic is not erased and cleanup remains separately unconfirmed.
- Successful cleanup does not create a pause; a permitted per-channel readiness timeout with confirmed cleanup retains its existing backoff behavior.
- New reason round trips through store serialization, API and UI. Restart remains paused with no host-start attempt. Enable/disable and sign-in controls do not erase it.
- Explicit Resume works only under the existing safe runtime conditions; current-process unconfirmed cleanup rejects it.
- v1/v2 fixtures containing every old pause, no pause, pending attempts and populated history migrate intact; reopening v3 is idempotent. Injected migration failure rolls back schema, version, settings, pause and recovery together. Unknown version/value fails without retained changes.
- A v2 executable cannot safely reopen v3 data. Rollback uses the preserved pre-upgrade isolated backup in a fresh recovery destination, never an older executable against the migrated database.

Use deterministic coordinator/host fixtures for adverse cleanup; do not deliberately hang or corrupt the live profile. The short live check verifies successful migration, sign-in retention, normal cleanup and safe restart, not a manufactured live cleanup fault.

## 5. Item B: distinguish storage waits and improve application behavior

### Implement bounded evidence first

Current `Store::call_named` measures execution only for failures; completion/commit aggregates in `store.rs` are test-only. `/api/status` itself waits for `store.stats()` and obtains disk free space, so it is not an independent heartbeat during an actor or filesystem stall.

Add a fixed-size, allowlisted in-memory activity snapshot shared by Store clones. Record monotonic timestamps/counters for admitted/enqueued jobs, dequeue/start, completion, class and operation, active-job age, queue/admission wait and queue depth. Distinguish admission backpressure from time already accepted into the actor queue. If exact depth cannot be maintained safely through send cancellation, report a precisely named conservative metric instead of a false exact count. Include success/failure/cancel-before-start totals, last completed operation and completed-duration buckets or count/sum/max; do not build an unbounded per-job log.

Expose this through a lightweight localhost read-only endpoint such as `/api/storage/activity`, with existing Host/Origin/security behavior. The handler must read only memory: no actor job, SQLite query, filesystem metadata, free-space call or journal flush. Include process/instance identity through the established runtime contract. A slow active operation is an observation, not a new storage-failure classification or proof of disk hardware failure.

Minimal responsiveness improvement: allow a client-abandoned queued **read** to be skipped before it starts, using the oneshot receiver state, with correct counter cleanup and no query callback left installed. Never skip an admitted mutation because its caller disconnected. If adding a bounded HTTP read wait, ensure timeout does not leave a growing queue of work; keep the existing five-second SQL execution budget distinct from queue wait and HTTP wait. The dedicated memory endpoint provides progress evidence without requiring a risky rewrite of aggregate-status caching. Root can show its bounded status in Collection health with freshness/age labels.

Keep the first actual storage failure, fault journal caps, existing five-second SQL query budget, FULL durability, single serialized connection, batch limits, bounded collector admission and explicit overflow gaps. Do not turn a wall-clock timeout into durable-write cancellation. A SQLite progress callback cannot preempt blocked filesystem I/O.

### Bounded diagnosis protocol

1. Root makes a five-minute read-only observation concurrently with a baseline or candidate run; no additional collection workload. Capture application process CPU/I/O, memory-only activity, selected localhost request timing, R: queue/read/write/latency counters and available Windows storage error events in a fixed time window. Resolve actual Storage Spaces logical/physical counter mapping; record unavailable counters explicitly. Do not infer cause from `% Disk Time` alone.
2. Write measurement output to a fresh local C: temporary directory outside app data, then copy and hash it into repository evidence after R: is responsive. This prevents an R: report write from being mistaken for application latency. The diagnostic script must itself have one outstanding probe per surface, finite output and finite duration.
3. If needed, issue at most a tiny independent read or fresh 4 KiB write/flush probe to a dedicated scratch file, at most once every ten seconds for five minutes. Never use the production/test database or a watched folder. Prefer counters first; a tiny probe still changes load and is not a throughput benchmark. Stop probes after two consecutive two-second timeouts; do not leave multiple blocked workers or retry the same I/O indefinitely. Retain any partial scratch file for explicit later cleanup.
4. Query existing Syncthing status/events read-only, without displaying its key, paths containing secrets or file names from unrelated personal folders. Resolve only relevant folder ancestry/reparse links. Temporal overlap with scans/cleaning is evidence of concurrence; claim causality only with discriminating measurements, not by disabling Syncthing under this authorization.
5. If a code optimization beyond abandoned reads is proposed, identify the measured application operation, compare the same closed synthetic fixture on the same host using fresh byte-identical copies, and preserve query results and logical fingerprints. Use at most two 120-second offline runs for a focused before/after check. A filesystem call may outlast the harness guard; stop launching new work and record that limitation. A baseline/candidate live run with different retained history and Twitch conditions is not a controlled speedup experiment.

Interpretation table:

| Observed evidence | Supported conclusion |
| --- | --- |
| High queue wait, one old active durable job, low completed throughput | The actor is blocked behind active work; the active duration includes filesystem work and does not alone isolate SQL from disk. |
| Independent R: probe/read/write delay and disk latency/queue rise coincide | Shared volume delay is supported; Veylurk is not the only affected operation. Root cause still needs attribution. |
| High application CPU/SQL execution with responsive independent volume probes | An application hot path is a candidate; profile/fixture evidence is needed before optimizing it. |
| Actor progress normal but browser attempt slow | Investigate Community/native/network phase evidence separately; do not call it a storage stall. |
| Syncthing scan overlap only | Correlation, not an established cause. |
| No recurrence during five minutes | The short observation did not reproduce the fault; historical cause remains open. |

Acceptance: a deterministic held storage job makes activity readable, reports increasing active age and queued waits, and does not turn the wait into a mutation interruption. Cancelled queued reads disappear safely; disconnected writes still commit; subsequent queries use a fresh budget. Snapshot size is bounded, secrets/SQL/account names are excluded, and worker counters recover after success, failure and shutdown. Existing two-stall collector recovery and overflow regressions still pass unchanged.

## 6. Item C: short 24-channel Community test

### Dataset and timing

The owner explicitly instructed root to choose channels and perform the tests. Root checked the original 22 unique names plus known `kamet0` and `otplol_` through the official metadata API, then filled a fixed set to 24 with currently live channels from the official stream listing. Four selected channels came from the original/known list; all 24 had fresh live confirmations at selection. Freeze this exact set before baseline and candidate runs, and report subsequent availability changes rather than silently substituting channels. Normalize/deduplicate through the application API and snapshot original tracked/enabled states first.

Use the existing optional official metadata integration to record live/offline/unknown status with its timestamp. Prefer currently live channels for interpreting a live-channel result, but do not silently replace the fixed list between baseline and candidate. Offline pages can still produce Community receipts; count them separately. Use All enabled channels for a declared page-sampling benchmark if that is necessary to exercise all 24, temporarily restoring the prior collection policy afterwards. A “24 live channels” claim requires 24 actual fresh live metadata confirmations, not 24 configured pages.

Use one shared browser and temporary Fixed/30 only within the isolated test. Keep both the 30-second global start spacing and the 30-second post-completion fence; retain failure backoff. A 24-channel circuit needs a budget of roughly twelve minutes **plus** the sum of navigation/sampling times. The exact first sweep has 23 intervening fences; a full recurring circuit has 24. Do not shorten fences to meet the test cap.

Each pass has an **18-minute collection window**, no extension or restart to hide a failure. Root may run one baseline pass during implementation and one candidate pass after rollout. These are two bounded comparisons, not a continuous soak. Capture a two-minute ordinary two-channel resource baseline on the candidate before expansion where useful. Setup/restoration and an in-flight durable I/O wait are reported separately from collection duration; the test cap does not authorize force termination. At most one run controls port 8796 at a time.

### Measurements

- Runtime identity before/after each control batch; original process cannot silently be replaced by a newly discovered PID.
- Configured, enabled, policy-selected, actually attempted, successful and failed distinct channels; exact start/completion/receipt times and safe failure categories for each attempt.
- Per-channel native source time, inter-start/post-completion spacing, first-pass completion latency, due lateness and achieved success intervals when a second receipt exists. One sample cannot establish that channel's recurring interval.
- Account/member counts by source/category and accepted sample IDs, without exporting account lists. Check durable receipt existence through bounded API pages and final offline verification after clean stop.
- Veylurk CPU and memory plus **only** the dedicated profile's WebView2 process group. Retain PID/start-time ownership; sum private bytes separately. A sum of working sets can double-count shared pages and must be labelled accordingly.
- Memory activity/runtime/Community status every five seconds; one store-backed lightweight query at most every ten seconds, with a two-second request budget and no overlapping retry. Record every timeout/missing value. Report median, observed range and sampled p95 with sample count; these are short-run summaries, not latency guarantees.
- IRC progress/gaps remain separate from Community sample success. Record browser/controller count and any cleanup failure, global pause, denial, source-gate loss or crash.

Store the initial candidate set and settings before starting. Select any extra representative repeat checks before inspecting results. Stop dispatch when the 18-minute window expires by the ordinary isolated configuration/Disable control, let the accepted attempt follow existing cancellation/cleanup, then restore original channels/settings. Do not delete observations produced by temporary channels; restore tracking state through normal APIs, which preserve history.

### Evidence levels

| Status | Required evidence |
| --- | --- |
| Full 24-channel first-pass proof | Every one of the 24 declared channels has at least one durable, source-eligible native Community success within the window; source guards/pacing preserved; exact identities and resource/latency results retained; normal cleanup/settings restoration verified. This proves breadth for one short sweep. |
| Additional cyclic evidence | At least one declared representative channel completes a second eligible receipt after the full sweep, with its actual interval reported. This still does not establish repeated all-channel rotation. |
| Full two-circuit proof | Every declared channel succeeds twice. Not required here and not pursued by extending the short window. |
| Partial many-channel proof | A strict subset succeeds, some are never attempted, a source gate pauses the run, or the window ends first. Report exact `successful/attempted/configured` counts and reasons; never title this as 24-channel acceptance. |
| Failed performance/safety gate | Identity mismatch, unexpected new hosts, a data/preservation discrepancy, pacing violation, crash, persistent loss of the memory heartbeat, or a cleanup/restart barrier violation. Retain evidence and stop expansion. |

Predeclare engineering investigation triggers: three consecutive memory-only probes exceeding two seconds, three consecutive store-query timeouts, active storage age over 30 seconds without progress, unexpected process accumulation, or sampled aggregate private memory above 1 GiB. These trigger diagnosis/stop of expansion, not a claim of a universal product SLA. Report observed CPU/memory even below triggers. A normal source denial stops the run immediately; do not Resume it to finish the channel count.

## 7. Item D: validate the 99% claim honestly

### Estimand and claims

Let `X_a,c(t)` be membership of account `a` in the channel's ordinary Viewer-role chat population at evaluated instant `t`. Let `E_<=A` be eligible receipts and recorded collection boundaries available by the fixed historical analysis cutoff `A = as_of`. The central model output is `p_theta(t) = P_theta(X_a,c(t)=1 | E_<=A)` under its stated parameters and observation assumptions.

The strong present decision is `min_theta p_theta(t) >= 0.99` over the finite implemented scenario set; strong absent is `max_theta p_theta(t) <= 0.01`. A sensitivity range is not a confidence interval. The scores are for cell midpoints, not every instant in a cell, a whole visit/broadcast, a natural-user population or video viewing. Later receipts are intentionally used by the historical smoother; evaluate with a fixed cutoff and do not describe it as an online forecast.

The empirical decision targets are separate conditional error rates:

```text
r_present = P(X(t)=0 | classification=estimated_present, supported target domain)
r_absent  = P(X(t)=1 | classification=estimated_absent, supported target domain)
```

Report calibration of the central probability separately from threshold-decision error. Also report the fraction of eligible reference points receiving strong, uncertain, unknown and no-result classifications. Returning unknown everywhere cannot pass a useful calibration claim by producing zero mistakes.

### Work achievable now

1. Extend numerical oracle tests, reusing exhaustive hidden-state enumeration for small cases and the actual production inference for integration. Cover irregular timing, variable counts, departure/return, response lag, state switches near boundaries, role changes, failures, gaps, left censoring and finite display resolution.
2. Add a deterministic, bounded offline evaluator with no network/database mutation. Its input is either simulated labeled trajectories plus generated observations or independent reference intervals plus frozen production model responses. Validate schema, UTC ordering, fixed `as_of`, model/settings identity, source eligibility, ground-truth provenance and split assignment. Use opaque IDs; never require cookies/tokens or personal account lists in public artifacts.
3. Run a reproducible on-model simulation with a fixed seed and declared independent trajectory count, then misspecification scenarios: repeated/cached sample blocks, unequal account inclusion, wrong `N`, time-varying detection, delayed/stale positives, non-exponential sessions, brief visits between midpoints, role changes and informative observation loss. Start with 200 independent trajectories of at most 120 observations per scenario and eight named scenarios; cap total evaluator time at 120 seconds. If the cap is reached, preserve an incomplete report and reduce future fixture size, not silently discard the failing scenario.
4. Keep calibration/development trajectories separate from an untouched test seed set. The simulated truth generator must not derive labels from the same posterior being tested. Apply the same production eligibility, first-positive support, censoring and cell-midpoint selection to generated observations; this can reveal selection/conditioning defects even in otherwise matching simulations. A simulation need not “pass 99%”; measured undercoverage is a result to investigate, not a reason to weaken a test.
5. Emit counts, exclusions, reliability bins, Brier score, log loss with a declared numeric clipping convention, both strong-class error rates, strong/unknown coverage, per-scenario results, Monte Carlo uncertainty and runtime. Keep simulation results explicitly labelled `simulation_only`; they cannot alter `calibration_status: uncalibrated`.
6. Implement reference ingestion/evaluation and tiny invented fixtures that prove interval boundaries, censoring, dependency groups, data split and score computation work. The fixture result is `reference_evaluator_verified`, not real calibration.

### Independent real reference requirement

First inventory only already authorized, relevant reference-session artifacts; do not scan unrelated credentials or browser data. Retained Community receipts, returned totals, IRC NAMES/JOIN/PART observations from the collector, successful browser pauses and viewer-count metadata are not independent membership ground truth. A Community positive cannot label itself and a miss cannot label absence.

An admissible reference record needs: pseudonymous account/channel/session identity; Viewer-role eligibility; independently evidenced membership or nonmembership intervals; entry/exit/return uncertainty bounds; clock error; observation provenance; all simultaneous sessions/account devices under control where absence is asserted; and a channel/time dependence-group identifier. Hold labels blinded to model predictions until decisions and splits are frozen.

A consenting participant's own authenticated chat-client acknowledgements and complete session logs can support a **controlled-session proxy pilot**, without broadcaster/moderator rights, if such access is already authorized. An open socket or a locally closed tab alone does not prove service membership or immediate server absence. Unknown server propagation, reconnects, other devices and list-role eligibility make those spans interval-censored or unlabeled. The dedicated collector login remains untouched; do not create accounts, obtain another login or send chat messages just to manufacture a dataset.

If no trustworthy server-membership reference exists, score the proxy as proxy agreement only. Do not reinterpret it as validation of the original estimand. In particular, true negative labels require an independent defensible absence reference; positive-only examples cannot validate the absent class. Scripted session durations do not validate natural-user arrival/departure priors. Nothing in this design establishes video viewing.

The owner has explicitly instructed the agent to choose channels and conduct the tests itself. Do not ask the owner to supply ground truth again. Select channels and complete every feasible test autonomously. If independently defensible membership labels remain unavailable, record that factual prerequisite and the exact evidence needed for future real calibration; continue all independent implementation/runtime work. Do not wait indefinitely or claim completion by redefining simulation as empirical evidence.

### Optional bounded self-reference pilot

Root may conduct one additional pilot of at most five minutes on one or two selected channels using the existing signed-in WebView2 profile, only if existing instrumentation can expose the necessary structural events safely and without changing source gates. No new account, OAuth authorization, browser/profile, installation, credential extraction or chat message is needed or permitted for this pilot. Do not add a new native instrumentation project or another build cycle merely to force this optional evidence into the current batch.

An incoming authenticated chat WebSocket `ROOMSTATE`/`USERSTATE` event matched to the current intended channel can timestamp the client's server-acknowledged join/session phase. Record only allowlisted command kinds, monotonic/UTC timestamps, an opaque session identity, matching-channel boolean and transport open/close state. Do not retain raw frames, tags, user identifiers, chat content, outbound frames, URLs or credentials. A transport open alone is not sufficient. Validate the extraction/parser offline before recording any real structural event; if safe access is unavailable, label the pilot unavailable and continue.

Classify this evidence as `client_session_proxy`. It establishes only an observed reference client's acknowledged connection interval, with transition/disconnection censoring. The same account may be connected through IAB or another client; closing this client cannot establish global absence. Navigation by the collector also changes the client's connection, making the experiment endogenous to collection. One account across a few channels cannot validate population inclusion or natural-session priors. Do not count these proxy intervals as exact labels for the membership estimand or use them to remove the uncalibrated label.

Freeze the first pilot segment for instrumentation checks and hold later segments out for descriptive proxy agreement. Report positive receipt overlap, time to first observed receipt, interval overlap and censored boundaries without treating a miss as absence. Use the broader short Community run for repeated-list overlap, miss-run or serial-correlation diagnostics only where repeated samples actually exist; one receipt per channel cannot estimate autocorrelation. Compare any observed overlap with an explicitly conditional uniform-sampling simulation, retaining unknown `N`, caching and selection assumptions. These are real source diagnostics and proxy observations, not empirical 99% membership calibration.

### Held-out evaluation and uncertainty

Freeze the model, defaults, thresholds, support rules, cutoff horizon and metrics before exposing held-out labels. Split at account/session and channel-time dependency-group level, with a later time block held out where possible; never split adjacent cells of one trajectory between training and test. Tuning after inspection creates a new candidate and needs a new untouched holdout.

Evaluate at predeclared timestamps or a fixed randomized sampling rule, not favorable receipt times. Score stable independently labeled spans; exclude a point whose clock/entry/exit uncertainty interval straddles the state boundary, and count every such exclusion. Report transition strata separately, including lead/lag errors with interval-censored boundaries. Include accounts never detected and episodes with no support in the coverage denominator. The present implementation's first-positive left censoring must not disappear from the report.

Stratify by channel/population-count range, cadence, role eligibility, session length and lag where data support it. Account/day/channel-time common shocks and repeated samples induce dependence. Use whole independent sessions/blocks as resampling units; no cell-level binomial calculation over thousands of correlated pixels. Report the number of genuinely independent groups and predeclare weighting for the intended target population. With few or unrepresentative groups, show descriptive uncertainty and retain `insufficient_reference_data`; bootstrap output alone does not certify a tail guarantee.

For perspective, with zero errors in `n` genuinely independent Bernoulli decisions, a one-sided exact upper bound is `1 - alpha^(1/n)`. Bounding an error rate at 1% requires at least 299 zero-error independent decisions for 95% confidence, or 459 for 99% confidence, for **one** predeclared class/domain. A simultaneous two-class 95% statement using equal Bonferroni allocation requires at least 368 zero-error independent decisions per class. These sample counts are planning illustrations, not permission to count correlated cells as independent or ignore selective coverage. Any errors increase required sample size. More predeclared strata/claims require a declared multiplicity budget or narrower exploratory claims.

An empirical release statement needs the chosen one-sided upper error bound below 1% separately for both strong classes in the prespecified domain, adequate nonzero class support, honest coverage, representative independent groups and frozen held-out analysis. A pointwise posterior threshold and the confidence level of this empirical error bound are different quantities. Neither gives simultaneous correctness of every point on an entire timeline; that would require a separately defined trajectory-level loss and independent trajectory-level validation.

Acceptance now: reproducible numerical/oracle checks, bounded simulation/stress report, tested reference evaluator and explicit missing-data status. API/UI remain uncalibrated. If a real dataset exists, additionally produce the held-out report; if it cannot support both tails or representative independence, mark the real claim partial/outstanding with exact missing evidence.

## 8. Integration, rollout and recovery

Use the existing Windows workflows rather than installing a local compiler. Required combined checks: formatting, strict Clippy, ordinary Rust tests, focused new migration/storage/model tests, unchanged isolated two-stall recovery, existing 10/25/50 synthetic cases, Node UI/native-script tests, PowerShell measurement/operations fixtures and packaged release startup/shutdown/backup checks. Run native WebView2 tests if its source changes; otherwise retain exact unchanged-source baseline evidence and explain its boundary.

Before replacement, root captures runtime/settings and hashes encrypted credentials without displaying content. Gracefully stop only the identified test instance; verify process exit, dedicated profile cleanup and database lock release. Make a verified offline backup using existing tools, including WAL handling and hashes. Do not copy an active SQLite main file alone or remove sidecars. Compare immutable historical rows by IDs/cutoffs and logical values; whole-file hashes are for offline backups/credentials, not an advancing live database.

Replace the approved executable path once. Verify candidate commit/hash, loopback/runtime identity, atomic migration, original settings, saved website login and fresh eligible samples. Test restart barrier behavior primarily in deterministic fixtures; do not insert an artificial pause with ad hoc SQL into the live history. A normal candidate restart verifies migration/session persistence without claiming a real cleanup-failure reproduction.

Stop expansion immediately on a source denial/challenge, unconfirmed cleanup, identity mismatch, invalid source provenance, preservation discrepancy or unexpected restart. Record the first failure; do not repeat a denial or relax a guard. If storage stalls, stop new measurement/collection expansion, keep the process and WAL intact, poll only bounded memory/process surfaces and allow ordinary cleanup to finish. A run cap is not a hard filesystem deadline.

If graceful shutdown or disk recovery remains unavailable, leave the exact state and recovery requirement recorded; do not force-kill the application or profile processes. If migration fails, retain the original backup and failed candidate data; do not repeatedly launch against suspect data. An older executable is usable only with a separate verified pre-migration restore. No rollback may overwrite history accumulated after the upgrade.

At the end, restore exact original channel tracking/enabled states, collection policy and Community settings, verify fresh receipts for the original two channels and retained credentials/sign-in, and recheck the production identity. Record attempted but unsuccessful restoration explicitly rather than claiming the task is cleanly finished.

## 9. Evidence artifacts and status ledger

Use a fresh `outputs/completion-2026-11-01-<UTC>/` directory. During a volume delay, stage reports in a fresh `C:\Users\Geoffrey\AppData\Local\Temp\veylurk-completion-<UTC>\` directory, then copy with hashes when R: is responsive. Do not overwrite prior reports. Keep raw sensitive account-level references local and restricted; published summaries use opaque IDs and no credentials, cookies, URLs or arbitrary page text.

| Artifact | Required contents |
| --- | --- |
| `manifest.json` | Baseline/candidate revisions, executable hashes, CI run/artifact IDs, tool versions, UTC/monotonic timing, test identity, protocol/seed hashes and artifact hashes. |
| `preservation-before.json`, `preservation-after.json` | Runtime identity, settings/channel snapshot references, backup/hash references, history cutoffs/counts/logical comparison, credential hash equality, profile cleanup and production identity checks. |
| `cleanup-validation.json` | New/legacy pause round trips, migration and rollback fixtures, retirement-timeout latch, denial precedence, API/UI/restart evidence and limitations. |
| `storage-samples.jsonl`, `storage-summary.json` | Activity snapshots, numeric process/volume/probe data, request outcomes, missing values, independent timing and bounded observation interval. |
| `storage-investigation.md` | Observations, competing explanations, discriminating tests, implemented behavior and remaining causal uncertainty. |
| `community-baseline.jsonl`, `community-candidate.jsonl`, `community-performance.json` | Fixed channel set, settings, actual attempted/successful/live counts, source timing, durable sample IDs, request/CPU/memory measurements, failures, cap/termination reason and full/partial classification. |
| `presence-simulation.json` | Constructed-truth provenance, seeds, scenarios, counts, scores, dependency units, Monte Carlo intervals, failures and `simulation_only` classification. |
| `presence-reference.json` | Real-data provenance/split/domain/label uncertainty and results if available; otherwise structured `not_run_missing_independent_reference` with exact prerequisites. Invented fixtures are identified separately. |
| `verification.md` | Integrated diff/review outcome, checks, CI/build/runtime boundaries, four separate final statuses and unresolved work. |

Allowed ledger statuses: `planned`, `in_progress`, `verified`, `partial`, `failed`, `not_run_missing_independent_reference`. A completed investigation may coexist with an unresolved historical cause. “Evaluator verified” must never be shortened to “99% calibrated.”

| Work / gate | Status | Evidence / next action |
| --- | --- | --- |
| Baseline identities/settings and preservation snapshot | verified | C: staging `veylurk-completion-20261101-1540`: exact configuration snapshot, consistent read-only-source backup, offline integrity `ok`, no pending sample, encrypted credential and executable hashes. Root revalidates runtime before mutations. |
| Optional bounded 24-channel baseline | partial | Stopped at the predeclared 1 GiB resource trigger after 60.7 seconds: 2/24 durable successes, 1,418 MiB sampled group private peak. Original settings/tracking restoration verified. |
| Cleanup enum, v3 migration and all retirement paths | in_progress | Implementation handed off; local Node tests and formatting passed. Independent review and Windows Rust checks remain required. |
| Cleanup restart barrier, denial precedence and UI | in_progress | Source and targeted UI regressions complete; independent review passed, Windows Rust/runtime evidence pending. |
| Memory-only storage activity and abandoned-read behavior | in_progress | Storage worker; unchanged mutation policy, proposed 10-second read-response wait separate from 5-second SQL execution budget. |
| Five-minute independent storage observation | in_progress | Read-only OS/process sampling to C:; no disk, Syncthing or production settings changes. |
| Numerical, simulation and reference-evaluator implementation | in_progress | Model-validation worker; bounded offline CLI and typed reference schema, no production inference/default changes. |
| Independent real reference available and admissible | not_run_missing_independent_reference | No dataset established at planning time; inventory relevant authorized artifacts. |
| Combined independent review and local checks | verified | Independent Astra source review; 114 Node tests and PowerShell measurement fixtures passed. Native ledger-test assertion and injected-stall job-class mismatch corrected without changing production budgets or recovery assertions. Compiled acceptance remains separate. |
| Hosted Windows MSVC checks/build and exact artifact | planned | One integrated candidate by default. |
| Verified backup, same-path isolated rollout and migration | planned | Root only. |
| Candidate 24-channel Community trial | planned | 18-minute cap; actual breadth and live status reported. |
| Simulated/reference evaluation reports | planned | Separate implementation/simulation/real statuses. |
| Original settings, credentials, history and production preserved | planned | Final restoration plus fresh original-channel receipts. |
| Four-item final report and remaining-work update | planned | Do not close empirical calibration without qualifying evidence. |

Completion report format: one row per authorized item with implementation status, automated evidence, actual runtime/reference evidence, and the precise unresolved limitation. This lets the owner distinguish a fixed bug, a measured short performance result, an unresolved storage cause and a statistical claim that current access cannot establish.
