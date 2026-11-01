# Process measurements and hidden browser, 2026-11-01

The measurement repair and hidden-window change are implemented and independently
reviewed. Exact Windows checks passed. Hidden anonymous page loading passed, but
authenticated collection remains unqualified because Twitch returned an
integrity-related Community error. The normal sign-in check is waiting for an
owner desktop observation; no denied collection request was automatically retried.

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

## Runtime acceptance and current stop

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
old failure diagnostics can remain retained under the source barrier. Current
window state requires the pending owner observation.

There has been no automatic Resume after the integrity rejection and no new
three-sample or 24-channel collection benchmark. Those acceptance gates remain
pending a meaningful normal sign-in state change.

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

The working test database is currently the isolated C: recovery copy. The
original R: crash triplet and saved credentials remain preserved. Returning the
complete latest database to R: and final restored collection verification remain
pending completion of the owner sign-in step. R: responsiveness and the old
native crash remain separate unresolved findings.

Private evidence is under `veylurk-ram-20261101-1730/measurement-hidden-v2` in the
user's temporary evidence area: anonymous negative, crash snapshot, committed
recovery, recovery marker, first-source-failure and real-process probe reports.
Database archives, profile content and credentials are not published.
