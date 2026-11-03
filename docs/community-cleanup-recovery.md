# Community recovery after a missing browser-exit notification

Status: correction implemented and verified in the test installation,
November 3, 2026. Production was inspected read-only and was not changed.

## Observed failure

Production Community collection stopped after its last successful receipt at
07:39:52 UTC on November 2. Status retained `cleanup_unconfirmed`: controller
Close and event unsubscription returned success, but the expected browser-exit
notification was missing after the five-second cleanup deadline. The native
worker subsequently exited with code 0 and its owned process tree was confirmed
empty. No source-denial diagnostic was retained. IRC, metadata and SQLite remained
healthy.

The current recovery path recognizes worker crashes and reported browser-process
failures. This cleanup-only, exit-zero retirement has no worker-failure
classification, so it remains paused despite the later exact containment proof.

## Scope and acceptance

- Inspect production read-only. Do not Resume, reconfigure, restart or replace it.
- Correct newly observed cleanup-only native retirements in the test version,
  with exact process/Job containment required before replacement.
- Preserve received source-denial barriers, owner cancellation and legacy durable
  pauses. Do not report an absent normal-exit notification as normal cleanup.
- Retain completed samples, fail interrupted work once and keep collection-run
  boundaries. Preserve existing retry delays and sampling deadlines.
- Add deterministic native/process and coordinator regressions, including
  ambiguous cleanup, source-denial ordering and owner cancellation cases.
- Independently review the change and pass exact Windows build/checks. Use a
  bounded isolated test proof with fresh durable receipts; no hours-long soak run.
- Preserve test history, encrypted credentials and signed-in profile. Use the
  established approved test executable location and port 8796.

The original reason the native exit event was not delivered is not established
by the retained diagnostics. This correction targets autonomous recovery once
physical containment is proved, without claiming that the event source itself
has been repaired.

## Correction and exact checks

Implementation revision: `73e078f8514d914fe650bdfeb8d2727ae3b006f0`.
A fresh explicit native cleanup failure now receives the parent-authored worker
classification `cleanup_unconfirmed` before its pause/completion is forwarded.
A terminal-only unconfirmed stop qualifies only when it carries its own explicit
cleanup-failure termination reason. Progress-only, default and stale cleanup
snapshots do not establish a new failure.

The existing retirement, quarantine, physical-containment and runtime-ticket
checks then authorize replacement. Normal cleanup remains unconfirmed, including
with worker exit 0. A missing callback is never reported as a browser crash.
Received source barriers, legacy pauses, owner cancellation, successful receipts
and scheduling fences remain protected. Existing browser-process failure evidence
has precedence over a later generic cleanup failure.

Astra supplied the bounded design scope. A fresh independent Sol XHigh reviewer
accepted the implementation without blocking findings. Exact-revision checks:

- [Native Windows workflow](https://github.com/Gevroska/veylurk/actions/runs/41564491832):
  139 tests passed, plus formatting, lint and proof build. Tests exercise the native
  missing-exit cleanup timer and real Windows child-process/Job containment using
  deterministic fixture reports.
- [Complete application workflow](https://github.com/Gevroska/veylurk/actions/runs/41564491800):
  279 principal library tests and the binary tests passed, as did isolated storage
  recovery, all three scale workloads, release build, packaged startup/shutdown,
  backup and restore checks.
- Supplementary local checks: 110 interface tests, Windows operations fixtures
  and 102 worker/process tests passed. The local process suite used a deliberate
  native-body stub; it is supplementary to the actual native workflow.

The regressions cover missing exit PID and kind with Close/unsubscribe success
and worker exit 0; explicit and terminal-only delivery; failed Close, detach and
unsubscribe operations; source-denial ordering and coalescing; bounded hung
teardown; uncertain containment; stale snapshots; owner cancellation; legacy
markers; retained positive receipts and exactly-once pending failure. Virtual
test-clock scopes keep timed SQLite reads on real time.

## Bounded signed-in test

Only the established test executable on port 8796 was replaced, after verified
database, encrypted-credential, executable and closed-profile backups. Production
kept its existing process, executable, settings and pause throughout.

The test's existing legacy cleanup pause was initially preserved and then resumed
once to establish the live baseline. The first live attempt encountered a separate
Twitch `integrity_denied` response, followed by a contained native-worker access
violation. The access-denial pause remained protected and no automatic request
retried around it. The owner used the normal sign-in window and confirmed the
page worked; an explicit owner recovery then resumed this test instance.

The unchanged main test process subsequently saved these authenticated,
network-validated Community samples from the live channel OuaisEddy:

| Sample | UTC receipt on November 3 | Account rows |
| --- | --- | --- |
| 558 | 02:29:31.510 | 103 |
| 559 | 02:30:11.224 | 103 |
| 560 | 02:30:50.837 | 103 |
| 561 | 02:31:29.864 | 97 |

Every sample had a verified request after its trigger, HTTP 200, network-response
transport, all cache flags false and a matching member-row count. There was no
pause or further owner recovery between these receipts. The previous test channel
flags and adaptive/300-second target were restored; the temporary live channel's
new proof history remains retained but untracked.

All eight pre-update immutable history prefixes remained identical: 47,012
observations, 224 sessions, 232 session-channel rows, 37,085 visits, zero history
boundaries, 47 Community runs, 556 samples and 33,470 member receipts. Integrity
checks passed, Community schema remained 3, and saved metadata credentials were
preserved.

The exact missing-callback recovery path was reproduced deterministically by the
native/process and coordinator regressions. No forced missing callback was
injected into the signed-in browser; the live check separately proves authenticated
receipt continuity. No hours-long reliability run or causal repair of WebView2's
missing notification or native DLL defect is claimed.

Private local evidence is retained under `cleanup-recovery-test-20261103-015700`.
Its browser-profile backup contains sign-in data and must not be published.

## Qualified artifact

The normal application artifact is `13199805500` from the successful application
workflow. Original CI archive SHA-256:
`5594bcbce4f95a92d80e90f11fb272da9c478f2459b702a5f0dfb7f0a81418a6`.
Executable SHA-256:
`00a42e45b314a29020ab5926cabeaec02387b6c25510275b6622732d69cf045f`.
The delivered local ZIP adds final documentation and provenance to the identical
qualified executable; its external manifest records the delivered archive hash.
