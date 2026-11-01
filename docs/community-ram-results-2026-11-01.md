# Popout Chat and dormant-browser qualification, 2026-11-01

Popout Chat sampling and the dormant WebView2 lifecycle are implemented. The
corrected build passed the anonymous negative check and three durable signed-in
samples from `otplol_`. The short trial peaked at **428.238 MiB** combined private
memory, with **10.848 MiB** median browser-free application private memory and
repeated roughly 30-second browser-free gaps.

Full 24-channel qualification remains **partial and failed**: the R: circuit
stopped on API probe stalls after 12 verified channels; the separate C: database
isolation circuit stopped on unavailable process metrics after 21 verified
channels, peaking at **467.215 MiB** private memory. Neither completed all 24.
Both history-prefix checks passed, credentials and production identity were
preserved, and the latest database was returned to its original R: path with
verified hashes and a retained backup. Final R: restart health and fresh durable
receipts from both original channels passed. The historical R: responsiveness
finding remains unresolved; no production rollout or complete breadth acceptance
is claimed.

## Source and lifecycle changes

Automatic sampling uses Twitch's ordinary
`https://www.twitch.tv/popout/{channel}/chat?popout=` page. Route, root-frame,
channel, generation, request-after-trigger and response checks retain explicit
source-mode matching. Popout readiness requires positive signed-in controls,
usable chat and Community controls, no video, and two seconds of stable readiness.
A full-channel redirect or unexpected video cannot satisfy a popout attempt.
The manual full-channel proof retains its separate paused-video contract.

The integrated host accepts work while dormant and creates WebView2 only for a
sample or owner sign-in. Creation consumes the accepted command's existing
deadline. Cancellation, shutdown, stale callbacks and controller epochs remain
enforced before and during creation. Successful sampling retires the browser and
confirms normal process exit, then leaves the host dormant instead of creating
an empty browser. The finite autonomous proof shares the popout source contract
but does not prove integrated-host dormancy.

Opening Community hides the positive chat identity controls. Authentication is
unobservable during that bounded panel phase, rather than cached as true. A
validated response is frozen, observation is disabled, the normal Close control
is used, and fresh positive authentication must remain visible for two seconds
before the original receipt can be published. Explicit denial, route, media,
epoch and original-deadline checks remain active.

Relevant source and regression coverage are in
[`native.rs`](../crates/community-source/src/native.rs),
[`sampling.rs`](../crates/community-source/src/sampling.rs),
[`host.rs`](../crates/community-source/src/host.rs) and
[`community-ui.test.cjs`](../crates/community-source/tests/community-ui.test.cjs).
The [design and source contract](community-ram-optimization.md) records the full
scope. No browser trimming API, disk-swapping policy, engine installation,
profile import or sampling-frequency reduction is part of this change.

## First failure and correction

The first signed-in candidate trial produced two receipts, then stopped with
`source_paused`, `signed_in_ui_lost` and a retained authentication barrier.
Restoration was partial and Community remained disabled. Its evidence remains in
`solo-otplol`; it is a failed qualification, not an accepted measurement of the
corrected build.

The actual injected JavaScript reproduced a false-denial-compatible opening
race: after an acknowledged Community click, the composer could disappear before
the panel mounted. The corrected transition may wait only while the expected
composer structure remains, both positive controls are hidden, the opener is
usable, and no visible malformed panel, Login, challenge or video is present.
That state cannot advance the phase, accept a body or extend the original
deadline. A confirmed panel and fresh two-second post-close authentication are
still mandatory. Before/after reproduction and independent Astra review covered
this bounded correction; the corrected build received new exact-revision CI.

The `solo-corrected` setup attempt failed before collection because of the order
of Resume and disabled-state controls. It supplies no accepted measurements and
is excluded from qualification. The original barrier was subsequently recovered
deliberately using the corrected, verified build. Failed evidence was retained.

## Build identity and automated checks

| Item | Exact evidence |
| --- | --- |
| Source revision | `3a685ed14897bc5e7247f0651a8f5147d5d95334` |
| Native Windows CI | [Run 41343709470](https://github.com/Gevroska/veylurk/actions/runs/41343709470), job `124958940130`: 96 tests passed |
| Application Windows CI | [Run 41343709431](https://github.com/Gevroska/veylurk/actions/runs/41343709431), job `124958939935`: all checks passed |
| Storage recovery | 85.21 seconds; existing acceptance gates retained |
| Scale checks | 10 channels: 3.123 s; 25: 18.411 s; 50: 82.022 s |
| Application artifact | `13098523961` |
| ZIP SHA-256 | `ae701893679f1ada279dcb7cc51c1c53589c8e93242d910fe3829df0ec932f6e` |
| Executable SHA-256 | `607e3c1ab73d2948e86dd49b21acda779d43457f322cecb91caf03c655b84a05` |

No performance threshold was increased. Automated test and CI success are
separate from the native source, memory and preservation checks below.

## Corrected native checks

The fresh anonymous check reached loaded chat and Community controls with
`signed_in=false` and no video. Its failure observation was
`navigation_ui_deadline`, with navigation ready and no pending script, network,
response or trigger. Durable verification found zero successes, zero member rows
and zero pending samples, with database integrity `ok`. Normal shutdown exited
with code 0. This is a loaded-source negative check; an incomplete page load
would have been inconclusive.

The corrected signed-in trial used `otplol_`, Fixed/30 cadence and unchanged
30-second global pacing. It stopped after three distinct receipt cycles in
**123.432 seconds**, below the 180-second cap. Settings restoration was
`verified_exact`, with no retained barrier and no restoration errors.

Separate durable verification, rather than the resource summarizer, confirmed
sample IDs **368, 369 and 370**, each with 110 returned members. Each receipt had
`request_after_trigger_verified=1`, all four cache indicators false,
`transport=network_response` and HTTP 200. The receipt timestamps were
20:04:09.872Z, 20:04:51.512Z and 20:05:31.789Z on 2026-11-01.

## Short-trial resource measurements

| Measurement | Corrected signed-in result |
| --- | ---: |
| Process sample records | 122: 120 valid, 2 unavailable |
| Actual sampling interval, median / maximum | 1.008 / 2.224 s |
| Combined private memory, peak | 449,040,384 bytes / 428.238 MiB |
| Combined resident working-set sum, peak | 639,860,736 bytes / 610.219 MiB |
| Combined private memory, all-valid-sample median | 11,464,704 bytes / 10.934 MiB |
| Browser-free application private memory, median | 11,374,592 bytes / 10.848 MiB |
| Browser-free application working set, median | 37,646,336 bytes / 35.902 MiB |
| Browser-free samples | 93 |
| Sustained browser-free observed spans | 29.289 s / 30 samples; 30.251 s / 31; 30.252 s / 31 |
| Final browser-free observation | One sample; no duration inferred |

The browser-free spans require valid complete root-only PID/start identities and
zero browser count, private bytes and working set. Missing metrics and gaps over
2.5 seconds break contiguous runs. Their durations cover first-to-last observed
elapsed time only. Initial waiting is included; no unobserved duration is added.
The descendant inventory does not itself exclude detached profile-owned
processes; final shutdown verification remains a separate gate.

API state is taken only from a successful same-record Community response.
Samples without such a response remain `unknown`; state is never inferred from
zero browser count. Process measurement precedes its associated API observation,
so a boundary record can show zero browsers with API state `sampling`.

| Recorded API probe | Median / p95 / maximum duration, ms |
| --- | ---: |
| Runtime | 1.022 / 1.380 / 3.360 |
| Community status | 1.145 / 1.484 / 15.361 |
| Application status | 1.348 / 3.247 / 2025.161 |
| Storage activity | 0.894 / 1.465 / 39.361 |

One application-status probe timed out at 2025.161 ms. It did not trigger the
three-consecutive-failure guard; the trial completed its receipt and restoration
checks. Setup and restoration requests have separate aggregate timing fields in
the evidence, including a 4990.624 ms Community-settings maximum.

## R: circuit interruption and C: storage isolation

The first corrected 24-channel circuit used the existing R: database and stopped
at **518.170 seconds** with `probe_stall_trigger`, after three consecutive API
probe timeouts with two-second budgets. It observed fresh receipts from **12 of
24 channels**, with zero recorded channel source failures. Combined private
memory peaked at **484,446,208 bytes / 462.004 MiB**, below the unchanged 1 GiB
guard. The resource report retains **failed qualification**; half a circuit does
not establish 24-channel completion.

Separate verification from the stopped C: pre-copy confirmed all 12 receipts,
sample IDs **373 through 384**, with committed members, verified
request-after-trigger provenance, all four cache indicators false,
`network_response` and HTTP 200. Successful receipts do not erase the probe-stall
stop or establish responsiveness for the unfinished circuit.

The helper could not confirm disabling, identity or cleanup during restoration:
it recorded `restoration=partial` and
`disable_identity_or_cleanup_not_confirmed`. A later independent manual
restoration verified all **28 channel-row flags** equal to their originals,
collection mode `all`, Community enabled, Adaptive/300 cadence and no source
pause barrier. The failed helper report remains unchanged alongside that later
verification.

Independent probes after stopping the circuit recovered to approximately 7 ms
for Community status and 3 ms for application status. The persisted
post-restoration storage-activity snapshot recorded a 17,340 ms maximum job
execution, a 15,451 ms maximum queue wait, two read-response timeouts, 989
cumulative successful jobs and zero failed jobs. These counters cover the
process lifetime rather than just the stall. They do not prove its cause.
**R: responsiveness remains unresolved.**

The test application, PID 36228, then stopped normally with exit code 0; no WAL
or SHM remained. The complete stopped R: database and its C: clone had matching
SHA-256:
`301335af5cae6950c104aee655b4ed7df7e969c390ac69a689cf1221f16a3275`.
The clone passed integrity checking, had zero pending samples, and retained
every verified original-baseline history prefix identically. A second baseline
immediately before C: isolation records **34,911 observations, 159 sessions,
390 Community samples and 22,974 members**.

Independent Astra review recommended one explicit database storage-isolation
run. It uses only the fixed C: clone database path, the same verified executable
hash above, the existing R: browser profile, port 8796, the same 24-channel
selection, pacing and all original guards. The private helper's `isolated-c`
mode passed PowerShell 5.1 fixtures. This is a test-tooling mode, with no product
change or hosted-CI revision.

The C: circuit stopped after **845.095 seconds** with
`process_metrics_unavailable`. It observed and separately verified committed
receipts, members and provenance for **21 of 24 channels**, with zero recorded
source failures. `pressea`, `zerost_s` and `altair` had no observed receipt before
the stop. The report retains **failed qualification** and `label=partial`; it is
not a full circuit. Helper restoration was `verified_exact`, with no barrier or
restoration errors. Independent restoration also verified all 28 channel-row
flags, `all`, Community enabled, Adaptive/300, no source pause and no source error.

| C: database isolation measurement | Result |
| --- | ---: |
| Process records | 831: 818 valid, 13 unavailable |
| Actual sampling interval, median / maximum | 1.009 / 4.018 s |
| Combined private memory, peak | 489,910,272 bytes / 467.215 MiB |
| Combined working-set sum, peak | 679,059,456 bytes / 647.602 MiB |
| Browser-free application private memory, median | 12,558,336 bytes / 11.977 MiB |
| Browser-free application working set, median | 38,969,344 bytes / 37.164 MiB |
| Browser-free samples | 623 |
| Browser-free observed runs lasting at least 20 seconds | 21 |

After the stop, an independent Sol reviewer obtained five
successful read-only sampler probes, including observations with six WebView2
descendants. The earlier sampler catch discarded its failure cause, so these
later successes neither explain the historical failure nor prove a sampler bug.
Allowlisted diagnostic stages and codes were added only to the private helper
for future investigation; thresholds, missingness and CPU behavior were not
changed. No additional benchmark or checks of the remaining three channels were
used to turn this partial run into acceptance. The C: result does not qualify R:
deployment responsiveness or resolve the R: stall.

## Verified preservation and return to R:

The C: test application, PID 40212, exited normally with code 0. No profile-owned
processes or WAL/SHM remained at shutdown. The stopped database passed both
original-baseline and pre-C: baseline history-prefix comparisons, integrity
checks and zero-pending-sample checks. The untouched R: file still matched
`301335af5cae6950c104aee655b4ed7df7e969c390ac69a689cf1221f16a3275`
before transfer.

The final C: database backup is retained as `c-storage/final-c-storage.sqlite3`.
The latest database was returned to the original R: test database path, flushed
and read back with matching SHA-256
`b116d1c2c349319668b83be7c69dfd97045a4c9afb7593ae2c0d7a00c5842e3c`
and size **35,864,576 bytes**. `return-to-original.json` records the transfer,
normal exit, zero remaining profile owners and both history-prefix results.

The encrypted credential file remained unchanged at SHA-256
`2d1998fdee24e21194f2c492c80f51c202d5f1ac2e9b7e8d91a3831a66d41440`.
Production's PID 30924, executable path, creation identity
`2026-10-28T21:11:40.9392480Z` and port 8795 were independently verified
unchanged. The final test instance,
`38872-2026-11-01T20:42:51.132Z`, restarted normally on port 8796 with the
original R: database and browser profile, the same verified executable and
existing credentials; no new API key was entered.

Final health verification confirmed all 28 row flags equal to their originals,
`all`, Community enabled, Adaptive/300, two enabled and two connected channels,
metadata configured and `ready`, no metadata or source error, no source pause
and zero failed storage jobs. Separate durable verification for the current
instance confirmed sample **420** for `normanajari` at
**2026-11-01T20:42:59.703Z** with 15 members and sample **421** for `strano` at
**2026-11-01T20:43:38.814Z** with 109 members. Member counts matched; both receipts
had verified request-after-trigger provenance, all four cache indicators false,
HTTP 200 and `network_response`. This establishes current restored health and
fresh collection, not the cause or resolution of the earlier R: stall.

## Historical full-page reference and limits

| Historical implementation | Collection window | Samples | Peak private memory | Waiting private median | Waiting working-set median |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full-page baseline `fb48e0e` | 60.711 s | 13 | 1418.184 MiB | 843.863 MiB | 985.855 MiB |
| Full-page page-retirement `4ea5d9e` | 231.663 s | 47 | 1025.844 MiB | 154.480 MiB | 324.953 MiB |

Both historical runs configured 24 channels, sampled roughly every five seconds
and stopped at the predeclared 1 GiB private-memory guard. Neither completed a
circuit. The baseline had two durable successes; page retirement had four across
six attempted channels. The latter still had six browser descendants in 31 of
33 waiting snapshots; its isolated zero-browser snapshot was followed by browser
recreation at the next five-second observation.

The corrected short trial used one channel, a different time window and finer
sampling. These are descriptive comparisons, not a matched benchmark or a
measured percentage speedup. The historical heavy baseline was not repeated.
The unchanged 1 GiB guard is an investigation stop, not a certified memory cap.

Private allocation and resident RAM are different quantities. Summed working
sets can double-count shared pages, and sampled peaks can miss transients.
Available stable-identity CPU intervals exclude process churn and do not measure
total-cycle CPU. No overall CPU reduction or throughput gain is claimed.
Accepted network responses do not establish server freshness, independent random
sampling, viewing coverage or calibrated probabilities. This work changes no
99% confidence or presence-model calibration claim.

## Verification ledger

| Gate | Status and evidence |
| --- | --- |
| Corrected source and independent review | Passed; bounded opening transition, explicit denial priority and post-close fresh authentication retained |
| Exact native and application CI | Passed for source `3a685ed14897bc5e7247f0651a8f5147d5d95334`; run/job IDs and artifact hashes above |
| Corrected anonymous negative | Passed; loaded chat/Community, signed-in false, no video, zero durable successes/members/pending, normal exit 0 |
| Corrected signed-in short trial | Passed; three durable receipts, exact settings restoration, no barrier |
| Short-trial memory and browser-free gaps | Measured; scope and missingness above |
| R: bounded 24-channel first circuit | **Failed qualification** at 518.170 s on `probe_stall_trigger`; 12/24 receipts separately verified durable; zero recorded channel source failures |
| R: post-interruption manual restoration | Verified 28 channel-row flags, `all`, Community enabled, Adaptive/300 and no source barrier; helper's earlier partial result remains retained |
| C: database storage-isolation circuit | **Failed qualification / partial** at 845.095 s on `process_metrics_unavailable`; 21/24 receipts separately verified committed with members/provenance; zero source failures; no claim for the remaining three channels |
| R: responsiveness | **Unresolved**; recovered later probes do not establish cause or sustained deployment health |
| Immutable retained-history prefixes | **Passed** on the stopped C: database against both original and pre-C: baselines; integrity `ok`, pending samples zero |
| Credentials, all channel flags, policy and cadence after C: trial | **Verified** unchanged credentials, 28 row flags equal, `all`, enabled Community, Adaptive/300, no source barrier/error; helper exact restoration independently confirmed |
| Latest database return to R: | **Verified** normal stop, both history-prefix checks, retained final C: backup and matching flushed/read-back hash; 35,864,576 bytes |
| Original two-channel post-restore collection | **Passed** current-instance durable IDs 420 (`normanajari`, 15 members) and 421 (`strano`, 109); member counts and provenance verified |
| Interim test shutdown before C: isolation | PID 36228 exited normally with code 0; stopped database had no WAL/SHM and matched its C: clone hash |
| C: test shutdown and profile-owned process cleanup | **Verified** PID 40212 normal exit 0, zero profile-owned processes, no WAL/SHM at shutdown |
| Final test restart on original R: database | **Passed** instance `38872-2026-11-01T20:42:51.132Z`; original profile/credentials, 28 flags equal, `all`, enabled Adaptive/300, 2 enabled/2 connected, metadata ready, no metadata/source error or source pause, zero failed storage jobs |
| Production | **Verified unchanged** port 8795, PID 30924, executable path `R:\Programmes\Outils\veylurk\veylurk.exe` and creation identity; excluded from replacement and measurement |

## Evidence locations

Private evidence root:
`C:\Users\Geoffrey\AppData\Local\Temp\veylurk-ram-20261101-1730`.

- `anonymous-corrected/terminal-status.json` and `durable-negative.json`: loaded
  anonymous-source observation and durable negative result.
- `solo-corrected-live/ram-candidate.jsonl` and
  `ram-candidate-summary.json`: measurements, receipt progress and restoration.
- `solo-corrected-live/resource-summary.json`: offline resource calculations;
  it deliberately does not infer source acceptance or mark durable proof true.
- `solo-corrected-live/durable-receipts.json`: separate durable verification of
  IDs 368, 369 and 370.
- `solo-otplol/`: retained failed first trial; its `resource-summary.json`
  explicitly records failed qualification.
- `solo-corrected/`: excluded setup-only failure before collection.
- `retained-history-baseline.json`: retained-history baseline after clean stop.
- `circuit-24-corrected/ram-candidate-summary.json` and
  `resource-summary.json`: retained R: probe-stall stop and resource results.
- `circuit-24-corrected/durable-receipts.json`: independent committed receipt,
  member and provenance verification for IDs 373 through 384.
- `circuit-24-corrected/manual-restoration.json` and
  `activity-after-restoration.json`: later exact flag/settings restoration and
  the cumulative storage-activity snapshot.
- `c-storage/clone-manifest.json` and `retained-history-before.json`: clean-stop
  clone hash, original retained-history prefix equality, integrity and no pending
  samples.
- `c-storage/pre-c-storage-history-baseline.json`: second retained-history
  baseline immediately before database storage isolation.
- `c-storage/trial/resource-summary.json` and `durable-receipts.json`: retained
  partial C: circuit, process-metrics stop and 21 committed channel receipts.
- `c-storage/independent-restoration.json`: independently verified 28 flags,
  policy, cadence and source state after the C: circuit.
- `c-storage/original-prefix-after.json` and `pre-c-prefix-after.json`: both
  stopped-database history-prefix comparisons, integrity and no pending samples.
- `c-storage/final-c-storage.sqlite3`: retained final database backup; its
  contents are not embedded in this report.
- `c-storage/return-to-original.json`: verified return hash, size, original R:
  preservation before transfer, normal exit and zero remaining profile owners.
- `final-restored/preserved-identities.json`: independent credential,
  executable and production-identity preservation record.
- `final-restored/health.json` and `durable-receipts.json`: final original-R:
  instance health, exact settings/row restoration and fresh durable IDs 420/421.

Historical references are in
[`outputs/completion-20261101/evidence`](../../../outputs/completion-20261101/evidence),
especially `community-performance.json`, each phase's JSONL and summary,
`durable-attempts.json`, `environment.json` and `manifest.json`. All 28 report
hashes listed by that historical manifest matched during the reference review.
No database, credential file, profile content, account-name list or chat content
is embedded in this report.
