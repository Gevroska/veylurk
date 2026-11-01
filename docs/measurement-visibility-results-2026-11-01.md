# Process measurements and hidden browser, 2026-11-01

The measurement repair and hidden-window change are implemented and independently
reviewed. Exact Windows checks and hidden anonymous page loading passed. In the
November 2 continuation, the owner confirmed the Twitch window appeared and closed
it; one deliberate normal Resume then preceded three durably verified hidden
`strano` samples in 135.770 seconds. A subsequent bounded circuit durably verified
all 24 channels in 969.736 seconds, with a 500.809 MiB application-plus-WebView2
private-memory peak. The complete latest C: database was returned to the original
R: test location after strict recovery/history and preservation checks passed.
Fresh durable receipts from both original channels and current health passed on
the restarted R: instance. The earlier integrity denial and native crash remain
documented below.

## Process measurement repair

The private benchmark previously enumerated processes and obtained identity and
metrics through separate, unpinned calls. Normal process exit could invalidate
those observations. Its historical catch discarded the exact failure, so the
repair does not retroactively identify the earlier exception.

The corrected sampler opens native process handles and binds creation identity,
ancestry and counters to those handles. It validates inventories around capture
and permits at most one bounded reconciliation for a confirmed lifecycle change.
Unresolved descendants remain obligations across reconciliation and subsequent
polls. An unknown surviving child cannot become a zero-browser measurement.
Independent review found and corrected intra-call, cross-call and post-inventory
orphan undercounts before runtime use.

Missing metrics remain unavailable. All observed verified private-memory lower
bounds still enforce the 1 GiB guard, including an incomplete group. The existing
three-unavailable stop, request budgets and trial limits remain unchanged. CPU
baselines reset after missingness or reconciliation. Reconciled point metrics
cannot bridge a browser-free duration. New failure codes/stages are allowlisted.

The change is in the coordinator's private `work` benchmark helpers, including
`Invoke-RamTrial.ps1`, `RamTrial.Processes.ps1` and their fixtures. It does not change
the packaged root-only measurement script's contract.

Evidence obtained:

- Windows PowerShell 5.1 deterministic fixtures passed for exit, PID reuse,
  unknown ancestry, access failure, lower-bound guards, reconciliation, missingness
  and handle cleanup.
- Two native fake-child fixtures passed. An exited pinned handle rejected metrics;
  bounded reconciliation took two attempts in each case.
- Five read-only observations of the older running test app passed.
- Five later read-only observations of the updated test app and seven actual
  WebView2 descendants passed. Capture durations were 34.943–130.242 ms. CPU's
  first interval stayed null. These were coherent captures, not proof of an
  uninterrupted channel circuit.
- Summary continuity/null-missingness fixtures passed. Replaying the historical
  C: log preserved its previous resource values, CPU samples and browser-free
  spans. Historical interrupted runs remain interrupted.

## Hidden browser window

The integrated service window is created without `WS_VISIBLE` and is no longer
minimized during collection. Native `SetWindowPos` hides it before sampling and
during cancellation, dismissal, retirement and shutdown. Explicit owner sign-in
restores and shows it, including a launcher whose initial show preference is
hidden. Dismissing that window retains the existing owner pause semantics.

The controller remains rendered with its ordinary bounds and `IsVisible=true`.
The change does not suspend scripts or alter authentication, source provenance,
deadlines, normal browser-exit confirmation or the standalone proof behavior.
Three real-HWND regressions cover creation, sign-in close/reopen and lifecycle
hiding. Their success is separate from authenticated website acceptance.

## Exact build

| Item | Verified identity |
| --- | --- |
| Code revision | `881ccdd8fd3c6b5d5280df308ede5dc643ab889a` |
| Native checks | [Run 41350528738](https://github.com/Gevroska/veylurk/actions/runs/41350528738), job `124979505607`, 99 tests passed |
| Application checks | [Run 41350528795](https://github.com/Gevroska/veylurk/actions/runs/41350528795), job `124979506973`, all checks passed |
| Normal application artifact | `13101443033` |
| ZIP SHA-256 | `8574ced41f4af80dd2c30ecc3b3be2821f9fa079040454c074e6f23419d2564d` |
| Executable SHA-256 | `9f738c4416f4f38e9bebab6a87068ee44b2c7b910fe78a935af5ab6204a4a037` |

The same approved test executable path was used on port 8796. Production on 8795
was excluded from replacement and testing.

## November 1 runtime acceptance and stop

The fresh anonymous profile reached loaded chat and Community while hidden, with
`signed_in=false` and no video. No test window was returned by the desktop
inventory. Separate durable checks found zero successes, zero members, zero
pending samples and integrity `ok`; shutdown exited normally with code 0.

The signed-in test on a recovered C: database then received an integrity-related
error in the selected HTTP-200 GraphQL response. This is distinct from a DOM
readiness or navigation-error classification. The specific server explanation
was intentionally not retained. Neither expired credentials, profile corruption
nor hidden-window causation is established.

The collector retained `integrity_denied`. An explicit normal owner sign-in
request subsequently reported navigation error 9 (`ConnectionAborted`). The
computer tool listed the test HWND but could not activate it; a black capture
does not independently establish visibility, rendering or the abort's cause.
Reopening creates a new hidden host before asynchronous initialization/show, and
old failure diagnostics can remain retained under the source barrier. At that
stop, an owner desktop observation was still pending.

No automatic Resume followed the integrity rejection. At that stop, the new
three-sample and 24-channel collection gates remained pending a meaningful normal
owner sign-in flow. The subsequent continuation is recorded below.

## Older-build crash and preservation

Before the updated executable was installed, the older running test process
38872 exited with `0xc0000005` in `EmbeddedBrowserWebView.dll` 156.0.4314.58,
offset `0x448084`, at 21:11:42.561 UTC. The unfinished sample 453 had started
6.414 seconds earlier. No source-supported cause or repair was established;
the visibility change must not be presented as fixing that access violation.

The original main database, WAL and SHM were preserved and hash-verified. A private
archive retains the original triplet. A committed C: backup passed integrity
checking with 40,885 observations and 453 samples; one unfinished sample remained.
Normal updated-application startup marked sample 453 `interrupted` with
`process_interrupted`, which was verified independently.

The solo and full circuit used the isolated C: recovery copy. The complete latest
database has since returned to the original R: test location, with the original
crash triplet archived and saved credentials preserved. Fresh collection and
health verification on the restarted R: instance passed as recorded below. R:
responsiveness under the 24-channel load and the old native crash remain separate
unresolved findings.

Private evidence is under `veylurk-ram-20261101-1730/measurement-hidden-v2` in the
user's temporary evidence area: anonymous negative, crash snapshot, committed
recovery, recovery marker, first-source-failure and real-process probe reports.
Database archives, profile content and credentials are not published.

## November 2 continuation: owner window and completed hidden solo trial

Dates in this section use Europe/Paris; recorded receipt timestamps remain UTC.
The owner confirmed that the normal Twitch sign-in window appeared and then
closed it. The coordinator independently observed one `WindowDismissal` around
23:02 UTC on November 1 (00:02 on November 2 in Paris). That observation qualifies
the owner-window flow; the earlier navigation abort's cause remains unknown.

Following that owner interaction, the coordinator issued one deliberate normal
Resume. The source pause cleared. The same application instance
`26568-2026-11-01T21:23:39.470Z`, executable and isolated C: recovery database were
used with the existing R: browser profile. No profile was purged. Collection
remained hidden during the completed solo trial from 23:04:14.273 to 23:06:30.043
UTC on November 1 (00:04–00:06 on November 2 in Paris).

The harness observed three fresh `strano` receipt timestamps and stopped at its
requested target. Independent durable verification matched all three database
samples and their member rows:

| Sample ID | Received at UTC, November 1 | Returned accounts |
| --- | --- | --- |
| 455 | 23:04:51.756 | 106 |
| 456 | 23:05:48.678 | 106 |
| 457 | 23:06:26.702 | 106 |

Each durable receipt records HTTP 200, `network_response` transport and a verified
request after the trigger. Disk-cache, service-worker, prefetch-cache and
served-from-cache-event flags are all false. The harness's initial summary label
still says durable verification is pending; the separate completed
`durable-receipts.json` supplies that proof. The final recorded Community state
was `waiting`, with `paused=false`, no persisted source pause and no last error.
Harness restoration was `verified_exact`, with no restoration errors or retained
barrier. This restored the trial's requested settings/state; returning the full
latest database to R: was still pending at that point and is recorded below.

Completed resource results:

| Measure | Result |
| --- | --- |
| Collection window / total harness elapsed | 135.770 s / 138.379 s |
| Valid / unavailable process captures | 135 / 0 |
| Reconciled / noncontiguous captures | 0 / 0; every capture completed in one attempt |
| Application plus WebView2 private memory, median / p95 / peak | 11.137 / 488.520 / 528.027 MiB |
| Peak private bytes / unchanged resource guard | 553,676,800 / 1,073,741,824 bytes |
| Zero-browser process captures | 92 of 135 |
| Zero-browser application private memory, median / peak | 11.133 / 11.176 MiB |
| Observed browser-free spans | 30.402 s, 29.385 s, 29.322 s; one final isolated point |
| Capture duration, median / p95 / maximum | 33.166 / 61.291 / 115.511 ms |

These are sampled process-tree measurements; peaks between samples can be
missed. Zero-browser captures establish complete root-only observations, rather
than excluding detached processes. Browser-free durations use observed
first-to-last spans without extrapolation. Available stable-identity CPU
intervals do not establish whole-cycle CPU use or an overall CPU improvement.

The completed private evidence is under
`measurement-hidden-v2/20261102-resume/solo` within the same temporary evidence
area: `ram-candidate-summary.json`, `resource-summary.json`,
`durable-receipts.json` and the completed `ram-candidate.jsonl`. Resource summaries
and status progress are separate from the durable source/provenance checks.

## November 2 continuation: completed 24-channel circuit

The subsequent isolated C: circuit completed one fresh receipt for every one of
the 24 configured channels in 969.736 seconds, within the 1,080-second limit. The
collection window ran from 23:07:02.118 to 23:23:11.855 UTC on November 1
(00:07–00:23 on November 2 in Paris). Total harness elapsed time was 976.510
seconds. The same application instance and executable were used.

Independent SQLite, member-row and provenance checks verified all 24 receipts,
sample IDs 459–482. Every receipt has HTTP 200, `network_response` transport,
verified request-after-trigger provenance and all four cache flags false.
No collection errors were observed. The completed harness restored its settings
and state with `verified_exact`, no restoration errors and no retained source
barrier. The independent `durable-receipts.json` supplies durable acceptance;
the harness summary's original status-only label remains unchanged.

| Measure | Completed circuit result |
| --- | --- |
| Fresh durable receipts / configured channels | 24 / 24; one full cycle |
| Valid / unavailable process captures | 953 / 5, out of 958 captures |
| Valid reconciled / noncontiguous captures | 1 / 1, included within the 953 valid captures |
| Application plus WebView2 private memory, median / p95 / peak | 12.461 / 398.617 / 500.809 MiB |
| Peak private bytes / unchanged resource guard | 525,135,872 / 1,073,741,824 bytes |
| Application plus WebView2 working-set sum, median / p95 / peak | 41.285 / 585.445 / 674.547 MiB |
| Zero-browser process captures | 708 |
| Zero-browser application private memory / working-set medians | 12.129 / 41.125 MiB |
| Observed root-only runs of at least 20 seconds | 24; spans 27.314–29.386 s |
| Capture duration, median / p95 / maximum | 41.092 / 65.422 / 124.990 ms |

Four isolated captures remained unavailable with `process_name_failed`. A fifth
remained unavailable with `inventory_changed` at `post_identity` after two
attempts. These remained unknown measurements. Separately, one confirmed
`process_exited` at `process_identity` produced a valid second-attempt capture,
marked noncontiguous. Missing and reconciled observations break CPU and
browser-free duration continuity as specified. The guard against three
consecutive unavailable captures, 1 GiB limit, request budgets and source-denial
guards were unchanged.

The completed evidence is under
`measurement-hidden-v2/20261102-resume/circuit`: `ram-candidate-summary.json`,
`resource-summary.json`, `durable-receipts.json` and `ram-candidate.jsonl`.
Earlier failed, partial and interrupted trials retain their historical status.
This completed circuit does not resolve R: responsiveness or the old native
access violation. It establishes neither server freshness, a 99% coverage claim,
long-duration stability nor an overall CPU improvement. Working-set sums can
double-count shared pages, and sampling can miss intermediate peaks.

## November 2 continuation: database return and preservation

The C: test process 26568 exited normally with code 0, and no browser-profile
owners remained before the stopped-database checks. The strict recovery verifier
passed all eight history tables and their schemas against the trusted startup
time bounds. Its deterministic suite passed 44 fixtures and received independent
review. All 40,885 source observations and 28,195 source Community member rows
remained identical. Only the expected recovery closures changed within the
source history: two sessions, 543 visits, one Community run and one unfinished
sample. The original and pre-C history-prefix checks also passed with integrity
`ok` and zero pending samples.

The complete latest C: backup was retained as `final/latest-c-database.sqlite3`
under `measurement-hidden-v2/20261102-resume`. Its stopped-state size was
41,291,776 bytes and its SHA-256 was
`50BB51D047C8CF0B076824D4AABEBF2D1314B0FA3485D1EAE9729ACE0566E512`.
Under exclusive C:/R: database ownership locks, the complete database was staged,
flushed and hash-verified back to the original R: test path. The original R:
crash database, WAL and SHM were retired together to `crash-before-hidden-20261102`
in that test data directory, with their saved hashes matched. No stale WAL or SHM
remained at the returned target before startup.

Saved credentials and the production instance on port 8795, PID 30924, retained
their verified identities. The test executable still matched the exact build
hash above. Preservation evidence is in `final/recovery-history-stopped.json`,
`final/original-prefix.json`, `final/pre-c-prefix.json`,
`final/return-to-original.json` and `final/preserved-identities.json` under the
same dated evidence directory.

The R: test instance restarted on port 8796 as PID 40560, instance
`40560-2026-11-01T23:27:24.147Z`, with the original profile, saved API credentials
and the same executable. All 28 channel flags and the original All/adaptive/300
settings matched the pre-trial snapshot. Two channels were enabled and collecting;
metadata was configured and ready, storage was available, and no source pause,
channel failure or current collection/metadata error was reported.

Independent read-only SQLite checks verified new `normanajari` sample 488 at
23:30:50.340 UTC and `strano` sample 487 at 23:29:47.825 UTC, both after this
startup. Their nine and 107 member rows matched the recorded counts. Both retained
HTTP 200, `network_response`, verified request-after-trigger provenance and all
four cache flags false. Reports `final/restarted-r-health.json` and
`final/restarted-r-durable-receipts.json` complete this bounded acceptance.
Production on port 8795 was not changed. This is not a long-duration reliability
claim or a qualification of R: storage under the full 24-channel load.
