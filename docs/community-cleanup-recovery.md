# Community recovery after a missing browser-exit notification

Status: diagnosis confirmed; test-only correction and verification in progress,
November 3, 2026.

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
has been repaired. Results will be recorded after verification.
