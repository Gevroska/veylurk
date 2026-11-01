# Isolated completion results — 2026-11-01

This report follows the [Astra execution plan](completion-plan-2026-11-01.md). Source implementation, automated acceptance, live measurements and empirical probability calibration are separate results. Production on port 8795 was excluded from all controls and executable/database changes.

## Verified candidate

- Source: `4ea5d9e36ea704a72cf8bab5136bf7bc76638d75`.
- [Windows application CI](https://github.com/Gevroska/veylurk/actions/runs/41321346204) passed formatting, strict linting, Rust tests, the isolated two-stall fifty-room recovery fixture, 10/25/50 synthetic workloads, UI/PowerShell checks, the offline validator smoke checks, and packaged startup/shutdown/backup checks.
- [Native source CI](https://github.com/Gevroska/veylurk/actions/runs/41320991143) passed 76 tests. Its native source is identical to the application candidate; the subsequent correction only adds unit types to application test channels.
- Local UI acceptance: 114 Node tests. Private operator helpers passed their offline configuration/parser/cancellation fixtures before live execution.
- Application artifact: `13089802220`, ZIP SHA-256 `cd31174dc7f32ea9abb06172a9d2e75ae8a77bd11f660f2229763e107ecf87eb`.
- Installed test executable SHA-256: `3d359ceb095a5c4ebe476d2a18e3a759d8eb64b3285bf66756139e5640727bc4`.
- Offline validator artifact: `13089712892`, ZIP SHA-256 `4fd33af1dc5c76392f7db42997ab9d2272c517c8a220f38bb5b574f70ccec262`; bundled file checksums and source revision verified.

## Cleanup classification and preservation

Cleanup uncertainty now persists as `cleanup_unconfirmed` in Community schema v3. Existing source/authentication/integrity/rate-limit barriers retain precedence. The coordinator saves the cleanup barrier before terminal history writes, including retirement-timeout and pending-cancellation paths. Enable, sign-in and restart cannot silently clear it. Migration/rollback, barrier ordering and UI regressions passed; adverse cleanup was tested with controlled fixtures rather than by damaging the live profile.

The old test process exited normally with code 0, and no process retained its dedicated browser profile. A fresh offline backup passed integrity checking with no pending sample. The pre-upgrade immutable baseline contains 27,602 observations, 89 sessions, 306 Community attempts and 19,319 member receipts. Its offline SQLite backup SHA-256 is `fe289610af292025c102db274674ef6d43fdec00272b2c27ddfe8e14c62f1ddc`.

Only the existing approved test package was updated. It restarted on port 8796 as instance `21500-2026-11-01T15:44:44.989Z`, loaded its existing encrypted credentials, migrated to v3 and obtained a fresh Community response without another login or API setup. Final history/restoration verification is recorded below after all trials.

## Storage responsiveness and historical cause

The new memory-only `/api/storage/activity` route exposes bounded admission, queue, active-job and completion evidence without SQLite or filesystem work. Reads have a ten-second total response wait and retain the separate five-second SQL execution budget. Abandoned queued reads are skipped; admitted durable writes still execute. FULL durability, actor/receive bounds and explicit overflow gaps remain intact.

The updated test route responded in approximately 1–2 ms during initial checks. Controlled Windows tests establish independence from a held database worker. These checks do not prove a physical storage speedup; synchronous disk-free inspection can still delay aggregate `/api/status`.

The independent five-minute storage observation completed with 29 usable samples out of 30; sampled R: queue reached 11. P: and R: share three physical devices, providing a possible contention path. Syncthing scans overlapped the earlier incident, but process transfer counters do not attribute physical disk traffic. The historical cause remains unresolved. See the [storage investigation](storage-investigation-2026-11-01.md); no Windows, disk, Storage Spaces or Syncthing setting was changed.

## Short Community resource comparison

Environment: Windows 11 Education build 26200, Ryzen 7 5700X (8 cores/16 threads), 31.92 GiB RAM, R: NTFS on Storage Spaces. Resource snapshots include the app and its descendant WebView2 processes every five seconds. CPU deltas are unavailable across process churn; waiting-state CPU is not full-cycle CPU. Memory snapshots can miss transient peaks, and summed working sets can double-count shared pages; the comparison below uses private bytes.

Both runs used the same frozen set of 24 channels, All-channel policy, Fixed/30-second target and existing global/post-completion pacing. All 24 were independently confirmed live at selection. At the later official check (`15:51:46Z`), 19 remained live; no channel was substituted. This is a channel-page sampling workload, not proof that all 24 stayed live throughout either run.

| Result | Baseline | Candidate |
| --- | ---: | ---: |
| Collection window | 60.711 seconds | 231.663 seconds |
| Configured channels | 24 | 24 |
| Durable initiated attempts | 2 | 6 |
| Successful distinct channels | 2 | 4 |
| Other attempts | None | One navigation timeout, one cancellation during resource-stop cleanup |
| Sampled group private-memory peak | 1,418.18 MiB | 1,025.84 MiB |
| Termination | Predeclared 1 GiB investigation trigger | Same unchanged trigger |
| Original settings/tracking restoration | Verified | Verified |

The candidate releases the Twitch page immediately after an accepted success through the existing confirmed-cleanup and same-profile recycle path. It starts the next attempt with fresh readiness checks. Signed-in chat, Community, video and the two-second paused-player gates remain unchanged.

Comparable waiting snapshots 25–46 seconds into the two runs, after the first `alexelcapo` success, show median private memory about **840 → 156 MiB**, approximately **81% lower**, and stable-process CPU about **30.6% → 0.62% of one core**, approximately **98% lower**. These are descriptive idle-phase improvements. Different page/network timing and process churn prevent a claim of overall throughput or full-cycle CPU improvement.

The active page still approached 1 GiB. Its allocation cause is unproven; this is neither a diagnosed leak nor proved irreducible browser overhead. Astra's post-measurement review recommended keeping the measured fix and stopping speculative resource changes. Neither run completed a 24-channel circuit, so many-channel Community breadth remains **partial**. The final cancelled sixth attempt is retained rather than omitted from the count.

Candidate accepted samples 308, 310, 311 and 312 each retained 100 Viewer-category accounts. All four record a request verified after the trigger, network-response transport and negative observed browser-cache/service-worker/prefetch/cache-event flags. `server_freshness` remains `unknown`; network provenance cannot prove uncached independent server draws.

## Numerical and constructed-truth validation

The offline validator ran all nine predeclared scenarios with 200 trajectories and 120 observations each: **1,800 trajectories completed in 6.663 seconds**, with no runtime-cap truncation. It used the unchanged production inference and defaults, opened no network or database, and retained development/holdout separation. Independent exhaustive-path oracle and production-midpoint comparisons passed in CI. The invented reference verifies the evaluator only; missing real reference is recorded as `not_run_missing_independent_reference`.

| Holdout scenario | Strong-present erroneous / classified points | Strong-present coverage |
| --- | ---: | ---: |
| Central transition/emission law | 0 / 233 | 0.114% |
| Correlated repeated draws | 540 / 2,612 (20.67%) | 1.275% |
| Stale positives | 15 / 135 (11.11%) | 0.066% |

These are correlated evaluation cells across 100 holdout trajectories per scenario, not independent decisions. Central numerical-probability coverage was 55.16%; the rest was unknown or no-result. **No strong-absent decision occurred in either split of any scenario**, so its error rate is unmeasured. Zero errors and a degenerate descriptive bootstrap interval cannot certify a 1% bound.

Astra reviewed the actual reports: the stress cases demonstrate assumption sensitivity and selective coverage, not a numerical defect established by these results. Even the central-law case retains first-positive selection and prior resetting, so it is not an exact calibration null for the entire selected procedure. Stress errors are constructed-truth results, not measured Twitch error rates. Settings were not tuned after seeing the holdout; a future revision requires a fresh untouched holdout.

## Live proxy check and final state

Both preselected pilot channels, `alexelcapo` and `altair`, were officially confirmed live before the run. The unchanged memory trigger ended the planned five-minute pilot after 65.835 seconds: one successful Community sample and one cancelled attempt. The public-chat observer captured 28 events from 20 distinct speakers during 63.395 seconds of actual overlap. Twenty-four events passed the unverified role proxy. The sole sample had a boundary-censored ±15-second comparison window: **zero fully observed usable comparisons** and no repeated successful sample pair. These results cannot measure dependence, membership error, absence or video viewing, and are not calibration evidence. No replacement channel, extension or favorable rerun was used.

The private pilot helper initially reported partial restoration because it issued 24 redundant DELETE requests against already-untracked historical channel rows; those requests returned 404. Separate identity-checked reads verified all 28 retained channel states and original settings were exactly restored. The `not_found` diagnostics came from these helper requests, not SQLite failures. The helper was corrected and regression-tested offline; the original partial report and supplemental verification are both retained. The experiment was not repeated to replace its result.

The test then exited normally with code 0; shutdown exceeded the initial thirty-second observation window. A later direct read-only history inspection on R: again stalled with little CPU use. Only that identified inspection helper was cancelled. The stopped database was copied sequentially to C: in 2.486 seconds, and full source/copy SHA-256 equality was checked. Verification of that byte-identical copy passed integrity checking, schema v3, zero pending samples and exact equality of every pre-upgrade immutable row. The preserved candidate snapshot contains 28,955 observations, 121 sessions, 321 Community attempts and 20,068 member receipts. The storage responsiveness issue remains unresolved despite successful verification.

The same approved executable restarted on port 8796 as `35824-2026-11-01T16:02:39.292Z`. Fresh durable samples 322 (`normanajari`, `16:02:49.879Z`) and 323 (`strano`, `16:03:25.951Z`) verify both original channels after restart. Original channel tracking, All policy and Adaptive/300 settings are restored; credentials are unchanged. The final point check found no pause, current Community error or first storage failure, metadata ready and storage available. Production remained PID 30924 with its unchanged executable and `2026-10-28T22:11:40.939248+01:00` start identity.

Local report copies and their SHA-256 manifest are in `outputs/completion-20261101/evidence/` relative to the parent workspace. Raw account-level proxy events, databases, browser profiles and credentials are excluded. All model results remain `uncalibrated`; independent empirical reference evaluation is `not_run_missing_independent_reference`.

| Authorized item | Final status | Remaining limitation |
| --- | --- | --- |
| Cleanup classification and persisted restart barrier | Verified implementation, automated and normal-runtime checks | Adverse cleanup is covered by fixtures, not deliberate live damage. |
| Storage observability and bounded read responsiveness | Verified implementation and automated/runtime checks | Historical and recurrent R: delay cause remains unresolved; OS I/O cannot be forcibly preempted. |
| Short many-channel Community performance | Measured idle improvement; partial live breadth | 4 successes / 6 initiated / 24 configured; active-page memory stopped the circuit. |
| Numerical/simulation/reference validation | Numerical checks and all simulations completed; invented evaluator verified | Zero usable live proxy comparisons; independent 99% membership calibration unavailable. |
