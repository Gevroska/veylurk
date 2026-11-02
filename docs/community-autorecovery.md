# Autonomous Community worker recovery

Status: implementation and verification in progress, November 2, 2026.

The owner requested automatic recovery after a Community worker crash without
restarting Veylurk or selecting Resume. This supersedes the manual recovery policy
of the initial [crash-containment revision](native-worker-crash.md). Production is
excluded; rollout and fault injection use the test installation on port 8796.

## Required behavior

- Keep the main application, IRC sessions and database running after a worker
  failure. Complete an interrupted sample once and retain completed samples.
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
5. Inject a worker failure during authenticated test collection. Verify automatic
   replacement and fresh durable Community receipts with the same main instance,
   without a Resume/Login request or application restart. Verify exactly-once
   failure, retained successful evidence and IRC continuity across the gap.
6. Verify history prefixes, channel/settings/credential preservation, final health
   and unchanged production identity. Publish the qualified ZIP and exact results.

Repeated-fault cap behavior, delayed physical cleanup, protected denial ordering,
and disable/Login/shutdown cancellation require targeted tests; a long-duration
reliability run is not part of this task. Results will be added after verification.
