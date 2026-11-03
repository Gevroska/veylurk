# Autonomous Community worker recovery

Status: implemented and verified on the isolated test installation, November 2,
2026. Production was not changed.

This report covers the initial crash/browser-failure recovery revision. A later
[cleanup-only retirement correction](community-cleanup-recovery.md) extends it to
missing browser-exit notifications followed by exact physical containment. The
original fault-injection results below remain scoped to their recorded revision.

The owner requested automatic recovery after a Community worker crash without
restarting Veylurk or selecting Resume. This supersedes the manual recovery policy
of the initial [crash-containment revision](native-worker-crash.md). Production is
excluded; rollout and fault injection use the test installation on port 8796.

## Required behavior

- Keep the main application, IRC sessions and database running after a worker
  failure. Complete an interrupted sample once and retain completed samples.
- Treat current WebView2 subprocess failures as infrastructure faults. Retire the
  complete native host before replacement, including a matching known failed
  browser exit observed during controller retirement. Missing, mismatched, normal
  or unknown exit evidence does not establish that classification.
- Start a replacement only after the exact owned Job is empty and its retained
  worker-process handle has reported exit. Physical containment remains distinct
  from normal browser cleanup; a crash never becomes a fabricated normal exit.
  A proved launch failure before any process was created uses a separate result,
  without inventing Job or process-exit evidence. Ambiguous ownership stays blocked.
- Retain uncertain Job/process ownership in quarantine and recheck it cheaply.
  No replacement may overlap a possibly surviving old browser. Eventual proof may
  authorize recovery without requiring operator action.
- Retry worker startup after 5, 10, 20, 40 and then 60 seconds indefinitely.
  Existing global 30-second sample pacing and per-channel retry/cadence deadlines
  remain additional admission fences. Reset the failure streak only after a
  validated sample is saved durably.
- Preserve every parent-received authentication, integrity, challenge, HTTP denial,
  rate-limit or generic source barrier. A worker fault must not erase a denial
  received in diagnostics when its separate status/completion event was lost.
- Persist cleanup uncertainty during quarantine. Only the matching runtime ticket
  may clear a cleanup marker that it installed, after physical proof and while
  its collection generation remains authorized. Legacy ambiguous cleanup/source
  pauses must never be reclassified automatically.
- Disable, owner Login and shutdown cancel delayed recovery. Late completion or
  cleanup reports cannot resurrect a cancelled collection generation.
- Reuse the application's existing signed-in profile and saved metadata credentials.
  Expose recovery stage, failure streak, next retry and retained fault separately
  from the new worker's current status.

A child observation never transmitted before its crash cannot be reconstructed.
The guarantee concerns all source barriers received by the parent and fresh
validation on every replacement, not recovery of untransmitted information.

## Delivery and acceptance plan

1. Review admission, source-barrier ordering, exact containment and persistence.
2. Implement coordinator recovery, sticky source barriers and quarantined cleanup
   with targeted state-machine and actual Windows process regressions.
3. Independently review event ordering, cancellation, source protection and bounds.
   Run exact Windows native/application CI and relevant interface tests.
4. Normally stop the old test, preserve and verify its latest history, credentials
   and signed-in profile, then install at the existing approved executable path.
5. Inject worker and owned browser failures during authenticated test collection. Verify automatic
   replacement and fresh durable Community receipts with the same main instance,
   without a Resume/Login request or application restart. Verify exactly-once
   failure, retained successful evidence and IRC continuity across the gap.
6. Verify history prefixes, channel/settings/credential preservation, final health
   and unchanged production identity. Publish the qualified ZIP and exact results.

Repeated-fault cap behavior, delayed physical cleanup, protected denial ordering,
and disable/Login/shutdown cancellation are covered by targeted tests; a
long-duration reliability run is not part of this task.

## Verified source and checks

Qualified implementation: `3968a5be0861112f898d3674bcfac9c829c5d157`.
Independent critical review accepted the final change without remaining blocking
findings. The two exact-revision Windows workflows passed:

- [Native Windows checks](https://github.com/Gevroska/veylurk/actions/runs/41374024729):
  formatting, lint, 135 shared parsing/lifecycle/IPC/process tests and proof build.
- [Complete application checks and release package](https://github.com/Gevroska/veylurk/actions/runs/41374024738):
  UI and PowerShell checks, formatting, lint, 276 principal library tests, other
  binary tests, isolated buffered-storage recovery, all three scale workloads,
  release build, packaged startup/shutdown and offline backup/restore.

Supplementary local checks passed 134 interface tests and 98 real Windows
process/Job fixtures. The latter used a deliberate native-browser stub; they
do not substitute for the actual native CI or signed-in checks below. New
regressions cover source barriers before terminal/completion delivery, empty
diagnostics after a denial, cleanup quarantine and ownership tickets, missing
physical proof, cancellation, repeated retry caps, failed durable completion,
browser ProcessFailed reports and matching failed browser exits during retirement.

GitHub's normal application artifact is `13110409984` from the successful
application workflow. Its original archive SHA-256 is
`e95334767d95361502e024fe63f264263bf389211d47981fecfad578e867aa2f`.
The executable SHA-256 is
`bea233078fa572183770c5ba3412454029db88d946314a529f239d2f6b888cad`.
The delivered local ZIP includes these final documentation updates with the
identical qualified executable; its separate manifest records both provenance
and the delivered archive hash.

## Bounded signed-in fault proof

The test used the original database and signed-in WebView2 profile on port 8796.
History, encrypted metadata credentials, the old executable and the offline
profile were backed up and verified before replacement at the same approved
executable path. No runtime was installed and no production process was modified.

The old build had already persisted `cleanup_unconfirmed` before this upgrade.
The new build correctly preserved that legacy marker. One explicit initial
Resume cleared it before either fault was injected. There was no Resume, Login,
application restart or process replacement by the operator between either fault
and its successful recovery. Fixed 30-second sampling was temporary; the original
enabled adaptive/300-second settings were restored afterward.

| Injected fault | Verified containment and replacement | Fresh durable receipt |
| --- | --- | --- |
| Retained native worker handle, numeric exit `0xc0000005` | Native DLL loaded in worker, absent from main; exact old worker exited and Job emptied; automatic replacement after about 5.4 seconds | Sample 540, run 46, 01:59:01.576 UTC; 36.5 seconds after injection |
| Exact test-owned WebView2 browser main process, numeric exit `0xc0000005` | Matching browser PID and failed exit kind retained; entire worker retired with exit 0 while retaining `browser_process`; exact Job emptied; automatic replacement after about 11 seconds including cleanup and backoff | Sample 542, run 47, 02:00:18.577 UTC; 41.3 seconds after injection |

Both receipts contained 34 accounts with matching member-row counts, a verified
request after the trigger, HTTP 200 and a network response with every cache flag
false. Interrupted samples 539 and 541 each became one failed `host_failure`
record, with no fabricated member rows. Runs 45 and 46 ended at explicit
`host_failure` boundaries; replacement samples used new runs. Previously saved
sample 540 survived the subsequent browser failure.

The main process and runtime instance stayed unchanged throughout both faults.
IRC sessions 223 and 224 remained collecting across both boundaries. Status
exposed backoff, verification and recovered stages; browser retirement also
exposed cleanup waiting. Historical normal cleanup remained false, separate from
exact physical containment and the new worker's current diagnostics.

## Preservation and limitations

All eight pre-upgrade immutable history prefixes remained identical: 46,952
observations, 222 sessions, 230 session-channel rows, 37,038 visits, zero history
boundaries, 44 Community runs, 538 samples and 33,226 member receipts. SQLite
integrity passed, Community schema stayed at version 3, and the encrypted
metadata credential bytes were unchanged. The original 28 channel flags and
collection policy were retained. Final checks found both original channels at
zero failures: NormanAjari saved a receipt at 02:01:49.424 UTC and strano at
02:00:56.437 UTC. Main collection remained active, storage was available and
metadata was ready. The test was left running on port 8796 with the same main
instance used for both faults. Production retained its original process and
port 8795.

Private backup/proof evidence is under `veylurk-autorecovery-20261102` in the
local temporary directory. It includes the offline profile archive; that archive
contains sign-in data and must never be published. No raw IPC, page content,
cookies, credentials or full memory dump was added to public diagnostics.

The original native DLL defect remains unidentified. These tests establish
containment and autonomous recovery from two bounded injected failures, not a
causal fix for the DLL or a long-duration reliability guarantee. Unknown physical
ownership remains blocked until exact proof arrives. Authentication/access
denials and legacy ambiguous pauses remain protected; this change does not
attempt to bypass them or recreate observations never transmitted by a child.
