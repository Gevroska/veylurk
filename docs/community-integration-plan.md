# Community collector integration plan

Status: planned, not implemented. Updated 2026-10-31 (Europe/Paris).

Production integration begins only after the isolated finite autonomous popout
proof passes. Manual WebView2 source access and sign-in persistence have passed;
the first autonomous attempt stopped on a native timing-contract assumption.
The timing correction passed 26 Windows tests. Its subsequent popout attempt
received an integrity denial and stopped without accepting members. A normal
full-channel control with paused playback and the same observer then accepted
134 accounts over a correlated network response. Finite automatic rotation
using that normal route and autonomous live acceptance remain pending.

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
