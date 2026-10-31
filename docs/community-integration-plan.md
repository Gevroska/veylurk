# Community collector integration plan

Status: implemented and accepted in an isolated Windows application test. Updated 2026-10-31 (Europe/Paris).

The finite autonomous full-channel source gate passed at revision d947fc6c.
Two circuits over kamet0 and otplol_ returned four accepted HTTP200 samples
(135,128,136,128 accounts). The last three arrived after the application
verified its own minimization. The process closed normally, browser-process
exit was recorded, and the proof process/profile processes were independently
absent. Exact Windows proof checks passed in run41243918757.

This establishes finite autonomous signed-in source operation. It does not
establish independent samples, server freshness, complete video-viewer coverage,
watch time, indefinite authentication or production application acceptance.
The denied popout route is not used as a fallback. Normal full-channel pages
with verified paused playback are the accepted route.

## Application acceptance

Revision `43831ae26982df0c3a9e719a5909e2463a5571d3` passed Windows workflow
[41248512390](https://github.com/Gevroska/veylurk/actions/runs/41248512390), including
strict lint, 179 library tests, binary tests, isolated buffered-storage recovery,
10/25/50-channel synthetic workloads, packaging and startup/backup checks.
The unchanged shared source passed 53 tests in workflow 41247581479; the UI passed 73.

A 240-second isolated application run on port 8796 reused saved metadata credentials
and the signed-in application-owned profile. It retained the copied legacy history,
accepted seven samples over normanajari and strano, saved 737 member receipts,
and passed account/channel search, independent pagination, broadcast association,
resolved timeline-range and historical-title checks. It exited normally with no
pending attempts, SQLite integrity `ok`, and zero profile processes remaining.
A normal restart obtained another sample without another sign-in or API entry.
Original test history/credentials and the production instance on 8795 were unchanged.

A separate short, offline release workload retained 110,000 synthetic IRC records
across 50 channels in 51.92 seconds with all correctness checks passing. This is
storage evidence, not live browser throughput. One 15.24-second waiting-state
resource point observed nine stable processes, 1022 MiB private memory and 2.94%
of 16 logical CPUs for the app and its descendants; it is not sustained usage.

## Backend and lifecycle

- Extract accepted parsing and correlation into shared code; do not maintain
  separate production and proof implementations.
- Run one optional Community collector beside IRC. A dedicated STA thread owns
  the WebView2 controller and message pump. Bounded typed commands and results
  carry job identity, normalized members, receipt time and safe metadata; no
  arbitrary URL, script or credentials cross the interface.
- Use the runtime's effective-channel selection, preserving collection when
  live status is unknown. Persist each result before scheduling another job.
- Default Community to disabled until normal owner sign-in and source checks.
  Sign-in pauses sampling. Authentication, integrity and challenge denials pause
  Community; ordinary channel failures back off without blocking other rooms.
  Keep IRC health independent.
- The proof's 64 retained navigation IDs and cumulative account unions are
  finite-run mechanisms. An indefinite collector needs controller epochs,
  generation checks and recycling at a quiet boundary before that cap. Detach
  handlers, close and confirm cleanup before replacement. Never retain an
  unbounded account union in memory.
- Default to a profile beside the database, owned by Veylurk. An explicit
  developer CLI override may reuse this proof's existing owned profile while
  the proof is closed. Do not copy cookies or import another browser profile.
  Twitch website sign-in stays separate from saved metadata API credentials.

## Durable records

Use the existing durable storage actor and an additive, component-versioned
transaction. Preserve all existing IRC observations, sessions, relationships,
history visits and their model version.

- `community_runs`: start/end, reason and build identity.
- `community_samples`: unique run/job identity, channel, start/receipt times,
  outcome, safe failure code, reported chatters, returned accounts, cache and
  timing provenance.
- `community_members`: unique sample/account membership and retained roles.

Commit a successful sample and all deduplicated members atomically. Failed or
cancelled attempts have no synthetic members. Mark unfinished work interrupted
on restart. Identical account sets from distinct requests remain distinct
receipts. Reject unknown Community schema versions without changing them.

Index channel/receipt/sample lookups and account/member/sample lookups. Queries
and pages remain bounded. Add explicit Community initialization/append diagnostic
operations; never pass Community records to IRC `append_sessions` or emit NAMES.

## Local controls and presentation

- Add Community-specific status, settings, login and resume endpoints under
  the existing localhost Host/Origin/JSON protections. The owner enters
  credentials directly into Twitch in the application-owned native window.
- Report last successful receipt, next due job, safe failure, delayed channels,
  requested interval and achieved cadence. One serial page cannot promise a
  short per-channel interval across dozens of channels. The proof's 30-second
  global spacing is an engineering choice, not a published Twitch allowance.
- Keep existing IRC API cursors intact. Give Community receipts their own
  bounded account/channel/time query and member cursor. An All/IRC/Community
  choice can show independently paged source sections.
- Reuse resolved and broadcast-clipped timeline ranges. Draw Community receipt
  ticks with source/sample identity and provenance, including Community-only
  accounts in suggestions. Do not manufacture inferred intervals or departures
  from missing sample membership.
- Tooltips describe inclusion in a response received at a recorded time.
  Server freshness, population coverage, independence, viewing duration and
  calibrated presence/absence probabilities remain unproved.

## Implementation and acceptance

Implement shared contracts and storage, then host/coordinator, local controls,
and source-aware history/timeline presentation. Preserve the current production
installation and test on an isolated database and port 8796. Existing firewall
approval can be reused only for its approved filename and location.

Verify additive migration/reopen/rollback, atomic members and interruption
recovery, fair scheduling and cancellation, controller recycling with stale
callbacks rejected, independent pagination and source-only timelines, localhost
control protections, finite live rotation, retained sign-in, measured browser
resources and graceful cleanup. Long reliability trials remain owner-managed.
