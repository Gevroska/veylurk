# Native browser crash containment

Status: implemented and verified on the isolated test installation, November 2,
2026. Native faults are contained; the original DLL defect is not identified.

This report describes the initial containment revision and its manual recovery
policy. The subsequent [autonomous recovery change](community-autorecovery.md)
supersedes that policy. Its separate verification establishes automatic worker
replacement; the tests below must not be read as evidence of automatic recovery.

## Problem and scope

The November 1 test crash was Windows Application Error event 64657 at
21:11:42.561 UTC: access violation `0xc0000005` in
`EmbeddedBrowserWebView.dll` 156.0.4314.58, offset `0x448084`. WebView2 was loaded
inside the application process. Its native failure therefore stopped the HTTP
server, IRC collection and Community collection together. The unfinished sample
was correctly interrupted during subsequent recovery.

Event metadata does not identify the failing native call. No retained crash dump
or demonstrated Rust/COM lifetime error supplies a narrow causal correction.
The existing hidden-window change is not evidence that the DLL fault was fixed.

The correction moves the integrated native browser into a private child process
of the same executable. The server, IRC collector, metadata and SQLite writer
stay in the main process. Production is excluded from development and rollout;
only the isolated test installation on port 8796 may be replaced.

## Required invariants

- Dispatch worker mode before the application's asynchronous runtime, database,
  HTTP listener or metadata credential initialization.
- Assign the waiting child to a parent-owned Windows Job before allowing native
  initialization. The Job must cover subsequent browser descendants.
- Use inherited anonymous pipes, a versioned bounded protocol and validated
  identities. Do not log frames, accounts, credentials, cookies or tokens.
- Preserve the original admission deadlines, cancellation generations, source
  validation and fully delivered receipts. A crash cannot fabricate a successful
  or duplicate sample.
- Keep native window hiding and ordinary explicit sign-in behavior unchanged.
- Bound blocked transport and worker-tree retirement. Verify tree disappearance;
  do not replace a worker with uncertain cleanup.
- On abnormal worker exit, keep the main application alive and fail Community
  closed. Preserve source barriers. Record numeric exit and containment evidence
  separately from normal WebView2 cleanup; do not invent a normal browser exit.
- Do not automatically restart or resample after the first abnormal exit.

The first abnormal worker failure pauses Community until an explicit owner recovery.
The main application continues serving its interface and IRC history. An app
restart retains the durable pause; it does not silently clear source denials.

## Verification plan

1. Inspect the actual Windows event, native lifecycle and Microsoft's API
   requirements; independently review the process-boundary design.
2. Implement worker transport, Job ownership and early dispatch. Add offline
   regressions for malformed/partial/oversized frames, stale results, original
   deadlines, cancellation, full transport and exactly-once completion.
3. Verify actual abnormal child exit and descendant cleanup on Windows. Confirm
   the parent remains responsive, preserves delivered success and records a
   failed unfinished attempt without silently restarting.
4. Independently review the patch and run exact native/application CI. Publish
   only an artifact qualified by the relevant complete workflows.
5. Normally stop the existing test, back up and verify history and credentials,
   then install at the existing approved executable path. Verify authenticated
   hidden collection and normal exit using isolated test data.
6. Inject a bounded worker failure on an isolated test run. Verify main HTTP,
   IRC and SQLite health, persisted failure/barrier and zero worker processes.
   Preserve all history and settings, restart normally, and check both original
   channels. Recheck production identity without modifying it.

Fault injection establishes containment and finite live samples establish
compatibility. Neither identifies or proves elimination of the original native
DLL defect.

## Verified build and review

| Item | Evidence |
| --- | --- |
| Application code revision | `20ddb45f8b97843e57b0f7e43f8a4a35ece70561` |
| Native Windows checks | [Run 41365032907](https://github.com/Gevroska/veylurk/actions/runs/41365032907), job `125023273061`: 121 tests, formatting, strict lint, proof build and offline CLI checks passed |
| Complete Windows application checks | [Run 41365032917](https://github.com/Gevroska/veylurk/actions/runs/41365032917), job `125023273169`: tests, isolated storage recovery, 10/25/50-channel scale checks, release build, packaging and lifecycle checks passed |
| UI regressions | 129 tests passed, including separate containment/normal-cleanup presentation and recovery guidance |
| Supplementary local process tests | 86 passed against a copied tree with the native browser body explicitly stubbed; real Windows Job, pipe, process and fault fixtures ran |
| Normal release artifact | `13107565623`, `veylurk-windows-x64` |
| ZIP SHA-256 | `6a7286ffc0709d2866174bacb320ed90f335fd696ca4025259a0dbee00f9b25a` |
| Executable SHA-256 | `71d216a2ce872cffe093b2961647bbb8cf3b7087d3fe80eda548c2bc12cd7ef9` |

Independent review corrected a ticket/dequeue race, unrestricted inherited
handles, lost final diagnostics, post-terminal publications and override-refusal
semantics. An actual Windows fixture exposed Job accounting reaching zero before
the retained process handle signalled; containment now requires both proofs and
retains the numeric exit reason. No material static-review finding remained.

Worker retirement has a shared 13-second wait budget. The tested maximum combines
six seconds for requested shutdown, 500 ms exit grace, five seconds for Job
retirement, one second for the retained process and 100 ms polling/publication
margin. That 12.6-second calculation fits the existing 14-second control reply and
15-second coordinator deadline. The ordinary native cleanup allowance is retained.

## Isolated application fault acceptance

The approved test executable path was reused. Fault tests used a separate empty
database and anonymous browser profile, with metadata credentials omitted. The
normal signed-in profile and original history stayed offline throughout injection.

Both idle-worker and active-sample cases injected numeric exit `0xc0000005` into
an exact, retained worker-process handle. Before injection, the native DLL was
loaded in the worker and absent from the parent. The private role, executable,
parent and creation identity were verified; no main application was terminated.
This injected termination is not a reproduction of the original DLL fault.

For the active case, the final pre-injection status confirmed a `strano` sample.
Sample 3 then became `failed/host_failure` exactly once, with no unfinished sample.
The server kept its PID and instance, reported numeric exit `3221225477`, confirmed
the worker tree exited and preserved `cleanup_confirmed=false` plus the durable
`cleanup_unconfirmed` pause. No replacement worker or sample appeared during the
bounded observation. Adding `normanajari` afterward established a new durable IRC
session while the database stayed available and collection remained healthy.

Earlier anonymous UI-readiness timeouts remain separate failed rows. They are not
signed-in compatibility results or successful samples. Both fault-test application
lifetimes ended normally with code 0 and no remaining profile owners. The stopped
fault database had integrity `ok` and zero pending samples.

## History preservation and final authenticated acceptance

The previous test ended normally before replacement. Its complete latest database,
encrypted credentials and executable were backed up, flushed and hash-verified
under the offline ownership lock; a nonempty WAL would have prevented copying.
The executable was replaced atomically at the existing approved path. Original
data and profiles were not overwritten by fault-test data.

After returning to the original database/profile, the new test retained all 28
channel flags, All collection policy and enabled Adaptive/300 Community settings.
Saved API credentials were unchanged; metadata was configured and ready. Both
original channels collected successfully, without a new sign-in:

| Channel | Durable sample | UTC receipt | Committed accounts |
| --- | --- | --- | --- |
| `normanajari` | 522 | November 2, 00:23:46.551 | 7 |
| `strano` | 523 | November 2, 00:24:26.402 | 68 |

Independent SQLite checks matched every member count and verified HTTP 200,
`network_response`, request-after-trigger provenance and all four cache flags
false. Current source status was unpaused, with no channel failures or collection
error. The test runs on port 8796; production on port 8795 retained its original
process, executable, creation identity and listener.

All eight retained-history prefixes matched exactly: 46,819 observations, 220
sessions, 228 session-channel rows, 36,947 visits, zero boundaries, 43 Community
runs, 521 samples and 32,805 member rows. Integrity was `ok`; the preservation check
also found zero pending samples at its snapshot. New rows remain outside those
immutable prefix bounds.

Private evidence is under `veylurk-crash-isolation-20261102` in the local temporary
area: pre-upgrade backups/baseline, `fault` and `fault-active` injection reports,
and `final/health.json`, `final/durable-receipts.json`, `final/history-prefix.json`
and `final/preserved-identities.json`. Databases, profiles, encrypted credentials
and pipe contents are not published.

## Remaining limits and recovery

A future native DLL fault can still terminate the worker. The correction prevents
that failure from taking down the server, IRC and database process; it does not
patch Microsoft's DLL or establish long-duration reliability. After an abnormal
worker exit, Community pauses. Once all worker processes are confirmed gone,
restart Veylurk and explicitly choose Resume. Normal cleanup is never fabricated,
and source-denial barriers remain intact.

R: storage responsiveness, statistical calibration and historical sampling limits
are unchanged by this correction.

Microsoft references: [process events](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/process-related-events),
[threading model](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/threading-model)
and [controller Close](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#close).
