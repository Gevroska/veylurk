# Requirements and remaining work

Updated: 2026-11-02 (Europe/Paris).

The owner has authorized implementation of the remaining features. The earlier review-only restriction and Astra-only allocation are superseded. Current AGENTS.md governs coordination: Sol High/XHigh handles implementation, testing, integration and delivery; Astra XHigh receives bounded complex scoping or critical analysis when justified.

## Product scope and collection decision

The application collects received public Twitch IRC membership evidence autonomously, with optional official broadcast metadata. It records chat-connected accounts, including some accounts that never post. Chat membership is a different population from video viewers. A connected socket, a NAMES terminator or a viewer-count total does not establish complete membership.

Repeated Community sampling is now source-verified in an isolated native Rust
WebView2 proof; its application integration is now implemented and verified on isolated Windows data.
The owner approved the existing WebView2 runtime and signed in normally using
a dedicated application-owned profile. No software was installed and no cookies
were imported. Twitch website sign-in is separate from metadata API credentials.

Revision d947fc6c passed41 native Windows tests, formatting, strict lint, release
build and offline CLI checks in run41243918757. Its finite live test completed
two circuits over kamet0 and otplol_, accepting135,128,136,128 accounts. The last
three receipts followed verified automatic minimization. Normal browser exit
and absence of the proof/profile processes were checked independently. A prior
normal restart also retained sign-in. The denied chat-popout route is excluded.
These results do not establish sample independence, server freshness, complete
coverage, absence probabilities, watch time or indefinite authentication.

The application now includes a shared native host, separately versioned durable
Community samples/members, serial scheduling, explicit sign-in/resume controls,
and source-aware history/broadcast/timeline UI. It defaults to disabled and
preserves IRC records and metadata credentials. Independent review and exact
Windows checks passed at revision `43831ae`. A four-minute isolated test accepted
seven samples across two live channels and 737 member receipts, then confirmed
normal browser cleanup. Restart reused the website session and API credentials.
The original history/credentials and production installation remained untouched.
See [the integration plan](community-integration-plan.md).

Official documentation checked on 2026-10-25 states that IRC JOIN/PART delivery is unavailable above 1,000 chat-room users. Get Chatters requires authorized broadcaster/moderator access and the relevant scope; app client credentials alone do not authorize arbitrary third-party channels. See [IRC membership limits](https://dev.twitch.tv/docs/chat/irc/#part-command) and [Get Chatters](https://dev.twitch.tv/docs/api/reference/#get-chatters).

Therefore population-adaptive sampling, calibrated statistical absence, exact individual viewing periods and 100% viewer discovery cannot be claimed. The evidence model labels supported visits and observation-only ambiguous segments, with uncertain boundaries. Point-in-time candidates are either an exact positive receipt or an explicitly uncertain inference within source-supported boundaries. No result is a verified viewing duration.

For independent uniform samples of size k from a fixed population N, the conditional chance of missing a continuously present account in m samples is (1-k/N)^m. This is not a probability of departure. The implementation does not apply that formula to IRC events, video-viewer counts or unvalidated correlated samples.

## Requirement inventory

| Original requirement | Implementation / remaining acceptance |
| --- | --- |
| Rust application on Windows | Implemented; standalone executable and embedded assets, packaged by Windows CI. |
| Simple English localhost interface | Implemented; listener on 127.0.0.1 with Host/Origin/JSON checks. |
| Enter a channel list | Implemented; bulk normalization, deduplication, pause/resume/removal, up to 50 tracked channels. History survives removal. |
| Autonomous operation without Brave or extensions | IRC is implemented with one WebSocket. Optional Community integration uses the existing embedded WebView2 runtime; isolated application acceptance passed at `43831ae`. |
| Repeated Community-list samples | Finite autonomous full-channel rotation, sampling after minimization, normal cleanup and sign-in persistence verified. Shared-host/storage/UI integration is implemented; isolated application acceptance passed at `43831ae`. |
| Observe silent accounts | Partial source support: NAMES/JOIN/PART can expose nonposting chat-connected accounts; delivery and completeness are unknown. |
| Discover 100% of video viewers | Unsupported by the available evidence and access rights. |
| Population-adaptive request cadence | Implemented as an optional response-count scheduling heuristic, with Fixed mode, separate retry fences and per-channel target/actual status. Atomic v1-to-v2 migration and a four-minute isolated live run passed at `6e5a49f`. Population validity, independence, coverage and absence probabilities remain unvalidated separately. |
| Detect live state and retain titles/details | Implemented using optional Helix credentials; retains stream identity, start, titles/category/language, viewer-count checks and observed offline checks. Failures and stale checks remain Unknown. |
| Collect during live broadcasts | Implemented selectable All enabled channels / Live channels only policy, persisted in SQLite. Live-only pauses only after a fresh successful offline check. Unknown or stale metadata keeps observation enabled. |
| First/last positive observations | Implemented per account/channel evidence visit and broadcast summaries; PART never extends the last positive receipt. |
| Departure and later return | Evidence-supported visits split at PART and collection boundaries. Post-PART delayed NAMES become ambiguous observation-only segments until a JOIN supports a new visit. All boundaries remain uncertain. |
| Statistical absence estimates | Implemented experimental Community dynamic model with conditional pointwise scores and explicit finite assumption sensitivity. Candidate checks are recorded by Windows CI and finite application validation. Empirical calibration remains open; missing IRC receipts never become verified absence. |
| Search account to channels/broadcasts and reverse | Implemented exact-login filters, range queries and bounded pagination. Broadcast summaries distinguish positive receipts inside the window from uncertain earlier-connected candidates. |
| Suggest known account and channel logins while typing | Implemented at `b93c8078`, with bounded local prefix suggestions and mouse/keyboard selection. Windows checks and an owner-approved test rollout passed; existing production was preserved. Suggestions never query Twitch or change collection settings. |
| Date/time searches | Implemented inclusive From / exclusive Until filters, normalized to UTC with validation. Broadcast intersections cannot leak disjoint time ranges. |
| Who was present at an instant | Implemented evidence-only view: exact positive receipt / inferred candidate / unknown; future times are rejected, collection outages and departures are not bridged. |
| Readable timelines | Separate visits retain positive receipt bounds and uncertain ends. The account chart compares channels on a shared axis and clips an account's chart to a selected broadcast. Direct receipt points, patterned inference, departures and unknown periods remain distinct. Automated validation and live rollout are separate gates. See [timeline interpretation](timeline.md). |
| Historical titles on hover and drag zoom | Current increment adds local historical metadata lookup on evidence hover, with sampled/unknown titles distinguished, plus mouse selection and keyboard zoom/reset. Zoom preserves pinned search bounds and never invents evidence. Broadcast search presents the viewer first and makes the channel restriction optional. Native and live acceptance remain separate gates. |
| Account graph with 99% presence/absence certainty | Implemented experimental Community model for account/channel and selected-broadcast views. Strong classes require all tested scenarios to meet a pointwise99% threshold. This is conditional model output, not calibrated confidence or a simultaneous guarantee for a whole interval/broadcast. Original ticks and unknown periods are retained; empirical calibration is outstanding. |
| Per-broadcast statistics | Implemented unique associated accounts, observed accounts, inferred-only accounts, supported visit count and positive receipt count; ambiguous segments are excluded from supported visits. |
| Title/category/viewer-count history | Implemented paginated stored-check table and accessible unconnected viewer-count sample chart. No interpolation across missing checks. |
| Visible current channel live state and quality | Implemented live/offline/unknown, per-room joining/collecting/unavailable/gap state, last positive receipt and unknown coverage labels. |
| Individual unavailable-room recovery | Implemented paced retries with bounded backoff and a new evidence session per attempt. Healthy rooms retain their sessions; channel configuration changes do not require a global reconnect. |
| Retain history indefinitely, no CSV | Implemented without automatic expiry or CSV. Disk use remains finite and visible. |
| Lightweight dozens-of-channels operation | Architecture remains one socket, bounded queues and serialized SQLite. Synthetic 10/25/50-channel retained-history tests added; extended live scale acceptance remains pending. |
| Community query and chart performance | Combined account/channel receipt seeks, exact covering member counts, grouped SVG cell paths and lag-only smoother reuse are implemented. Existing schema/data, inference settings and limits remain unchanged. Native correctness, migration, numerical/geometry equivalence and same-host before/after measurements are separate candidate acceptance gates; see [performance](performance.md). |
| Continuous/background operation | Foreground plus optional hidden supervisor and opt-in per-user login task. Bounded crash retries are provided by Task Scheduler. No application startup task is installed merely by building or inspecting code. |
| Graceful stop | Ctrl+C, guarded local shutdown endpoint and verified Stop-Veylurk.ps1. No arbitrary process termination. |
| Safe backup/restore | Lock-protected offline backup, nonempty WAL refusal, hash manifest and fresh-directory restore. Existing history is never overwritten. Encrypted credentials remain encrypted. |
| Storage failure and low disk | Available-byte status and low-space warning; stopped storage causes graceful process failure so the optional task can apply bounded retries. Lost/uncommitted evidence remains an explicit collection gap. |
| Diagnose unattended failures | Bounded typed sidecar independent of SQLite, first storage failure retained separately from later cleanup, numeric error codes and actual progress-interruption flag. Persistence is best-effort and visible in Collection health. See [diagnostics](diagnostics.md). |
| Measure an unattended trial | Read-only finite localhost measurement, exact process identity, explicit replacement/API gaps, actual CPU denominator and bounded reports. See [measurement](measurement.md). Extended acceptance still requires elapsed real operation. |
| Auto/Light/Dark and accessibility | Preserved persistent theme choices, keyboard focus, horizontal table scrolling and responsive layouts. |
| Public repo, Unlicense, project name | Existing Gevroska/veylurk repository and Unlicense retained. No guarantee of exclusive name/trademark rights is asserted. |
| Automatic compilation/tests | Windows workflow extended for new UI/Rust/PowerShell tests, scale fixtures and packaged background-stop/backup/restore lifecycle. |

## Evidence model and preservation

- Raw observations remain intact. Derived model version 1 is stored separately and can be recomputed from retained observations and recorded collection boundaries.
- Positive NAMES/JOIN receipts support observations. PART is departure evidence and never a positive attendance extension.
- Delayed roster entries after PART do not create a supported return. Backwards receipt clocks are flagged; uncertain boundaries do not infer continuity.
- Collection sessions are per room/attempt. A room retry, removal, restart or connection outage cannot silently join visits across the gap. Older room gaps that were never recorded cannot be reconstructed retrospectively.
- Prior-connected broadcast accounts are explicitly inferred and have no fabricated in-window positive timestamps. Account-to-broadcast and broadcast-to-account filters use the same selected interval.
- SQL pages and storage jobs are bounded. The retained-history regression verifies an indexed lookup for new sessions rather than scanning all earlier room visits on every account receipt.

## Validation and completion gates

### Automated and fixture checks

- Existing UI/theme tests plus range validation, visits/ambiguous segments, point-in-time states, metadata chart/table, broadcast summaries, policy, storage warning and graceful-stop controls.
- Rust history tests for departure/return, PART-only evidence, repeated/delayed NAMES, backwards clocks, future instants, recorded gaps, recomputation, migration, ranges, pagination and broadcast association.
- Dynamic IRC fixture for isolated room retry and configuration changes, plus collector fixture preserving another room's evidence session.
- Windows PowerShell fixtures for DPAPI, locking/WAL refusal, hashes, restore preservation, startup task configuration and verified shutdown. Fixtures do not install tasks or contact Twitch.
- Deterministic SQLite progress interruption inside the real append transaction, raw/derived/totals rollback, fresh next-job budget, explicit collector failure boundary, bounded diagnostic persistence and signal-registration error handling.
- Measurement fixtures cover exact process ownership, replacement, API failure, actual CPU timing, redaction and finite output bounds. Packaged CI also checks a finite hidden liveness interval and actual local measurement without channels.
- Ignored synthetic workload tests run explicitly in CI for 10, 25 and 50 channels, 2,200 observations/channel, retained-history queries/reopen and metadata history. Synthetic dates are not elapsed live operation.
- GitHub Windows MSVC CI must pass formatting, lint, ordinary/scale tests, release compilation, no-channel startup and real packaged background shutdown/backup/restore before accepting the downloadable candidate.

### Pending machine/live acceptance

- Pause before first launch of the new Internet-capable executable, as requested by the owner. Retry the same binary after firewall approval.
- Preserve the existing database and DPAPI credentials, verify the copied history, then validate optional metadata on genuinely live channels.
- Check both search directions, historical migration, separate visits, range filters, themes and collection policy on the real package.
- Login startup/recovery needs an explicitly selected opt-in task on the intended Windows account. It is implemented but is not installed without that choice.
- The short performance increment passed Windows CI at `88a72ba0`; its owner-approved local rollout preserved history and credentials. Long reliability trials remain owner-managed. See [performance evidence](performance.md). Short trials and CI do not establish long-run reliability. The current interface increment adds historical title inspection and timeline zoom; validate it separately from the earlier releases.
- The autonomous Community source gate and isolated application integration passed. Fixed/Adaptive scheduling is implemented as a response-count heuristic. Validate freshness, caching, selection bias, population identity and correlation before introducing statistical estimates; the scheduling formula does not establish those properties.

## Current development evidence

The history/runtime increment at `4cb1c257` passed Windows CI, including 95 Rust tests counting the three explicit scale cases, 25 JavaScript tests and packaged operations. Its subsequent short live trial verified two genuinely live channels, both search directions, broadcast associations, themes, policy persistence and graceful stop. All 8,538 original raw observations and eight original session boundaries were verified unchanged; original rollback data and encrypted credentials were preserved.

Two initial starts of that revision stopped with generic storage errors; later foreground and hidden background runs were healthy. Their underlying error was not retained, so those earlier causes remain unresolved. Diagnostic revision `4df70187` subsequently passed 106 Rust tests, 27 JavaScript tests and packaged Windows checks. Its owner-authorized live starts then captured two real append cancellations by the all-job progress budget. The correction now makes every production job explicitly a cancellable query or a durable mutation, while preserving FULL durability, a single connection, transaction/batch limits, bounded receipt admission and fail-stop behavior. Separate deterministic tests cover durable success beyond the old budget, query cancellation, and exceptional SQLite write interruption. No partial flush is blindly retried. See [diagnostics and policy](diagnostics.md).

Local validation uses isolated fixtures. Rust execution is delegated to hosted Windows MSVC CI because the available local GNU toolchain lacks its native C build dependencies. Final CI/run/artifact identifiers and live validation belong to the candidate verification record; do not reuse the earlier two-channel acceptance as evidence for this revision.

Earlier UI visual fixtures were fictional and established rendering/interaction only. Community integration was subsequently checked in the real isolated application with two live channels, a retained sign-in and a separate test database. Existing firewall authorization is reused for the same approved executable filename and location; notify the owner before a genuinely new Internet-connected executable path. Short trials of an earlier revision are not blanket acceptance of later application changes.

## Further constraints

Windows, English source/UI/documentation, local credentials, indefinite retention and no CSV remain unchanged. Source limitations are outstanding original requirements rather than silently completed items. No routine stage-by-stage permission gate is imposed. The only planned interruption is the owner's new-executable firewall gate or a concrete product decision that cannot be resolved from existing authorization.

## Adaptive cadence acceptance

The application adds count-based Adaptive/Fixed scheduling and a real atomic
Community v1-to-v2 settings migration. Existing installations keep Fixed mode and
their saved preferences, pause and history; fresh installs remain disabled with
Adaptive/300 defaults. Independent source review is closed. Revision `6e5a49f`
passed Windows workflow [41253405010](https://github.com/Gevroska/veylurk/actions/runs/41253405010),
including 201 ordinary library tests, the isolated recovery fixture, binary tests,
three scale cases, 79 application UI tests, five theme tests and packaged operations.
A 240-second isolated run saved six samples and 628 member receipts over two live
channels, reused both website sign-in and metadata credentials, then exited normally
with no pending samples or profile processes. All pre-upgrade immutable IRC and
Community records compared identically, including 20,509 observations and 10,397
member receipts. Fixed settings migrated intact; legacy PATCH requests preserved
the current mode; explicit Adaptive targets matched the saved response counts.
The production installation was unchanged. Long reliability trials remain owner-managed.
See [the cadence contract](community-cadence.md).

## Community cleanup recovery candidate

The isolated test instance reported repeated source timeouts followed by
`cleanup_unconfirmed`. The retained generic `source_denied` pause is a conservative
safety latch, not evidence of Twitch authentication or integrity denial. A later
controlled rotation on the previous binary accepted one sample and reported
`navigation_ui_deadline` for the next channel with confirmed cleanup; a page
readiness timeout alone therefore does not establish a browser cleanup fault.

The recovery candidate preserves a browser exit already observed before retirement,
checks its controller epoch, expected browser PID and normal exit kind, and keeps
the same identity and deadline when recycling changes to closing. Confirmation
still requires successful handler detachment, controller Close and exit-observer
removal, plus the matching BrowserProcessExited event within the existing bounded
wait. There is no process-handle substitute, deadline extension, player-gate change
or automatic bypass of a denial pause.

Community status now separates the latest structural primary source failure from
cleanup diagnostics for the current application run. These bounded typed snapshots
remain in memory and the reserved terminal event; they are not persisted to disk.
Reporting does not perform disk I/O on the native thread or delay browser cleanup.
The generic pause still persists unchanged, and the runtime does not infer a past
primary failure from it after restart. Persistent primary/cleanup reporting remains deferred;
the auxiliary-window correction below adds bounded termination reporting only. Native and
coordinator regression tests cover identity, early/stale/abnormal exits, close during
recycling, cleanup operation failures and preservation of the primary failure.
Source review accepted the correction. Revision `223b0501` passed Windows application
workflow [41268954989](https://github.com/Gevroska/veylurk/actions/runs/41268954989);
the unchanged shared native source passed 59 tests in workflow
[41268713871](https://github.com/Gevroska/veylurk/actions/runs/41268713871).
A short test-only rotation saved a fresh 47-account sample. The other channel
reached its page-readiness deadline with signed-in chat and Community available
but no required Play control; normal browser cleanup was confirmed in 1,273 ms,
and scheduling remained active rather than entering a global pause.
All 24,858 pre-update IRC observations, 171 Community attempts and 15,742 Community
member receipts compared identically, and saved credentials were reused unchanged.
Production port 8795 and its executable were not modified or restarted. This short
check does not establish that an intermittent cleanup failure can never recur.

## Paused-player readiness correction

The test instance's `strano` page reached the unchanged 25-second navigation/UI
deadline with signed-in chat, Community and paused video present, but without the
ordinary Play control or decoded frames. The Play-label requirement was an
additional readiness gate even though the video elements directly reported pause.

The candidate treats `play_control` as diagnostic only in both the native
`PlayerGate` and the browser's immediate pre-click check. Initial readiness still
requires signed-in chat, an available Community control, at least one video and
every video paused for two seconds. Missing or changing Play controls and decode
readiness cannot reset that interval or withhold trigger/body/idle acceptance.
Missing video, unknown pause state or resumed playback still blocks initial
readiness and fails closed after verification. The source can issue only one
ordinary Pause click when actually playing and must then establish a fresh full
stability interval. Authentication, challenge, navigation/request provenance,
trigger deadlines and cleanup handling are unchanged.

Focused Rust regressions cover these readiness and pause-action boundaries. An
offline Node VM fixture executes the actual embedded `COMMUNITY_UI`: paused video
without Play or decoded frames produces one ordinary Community click; playback,
missing/unknown video, wrong pages, challenge pages, lost prerequisites and
expired deadlines produce none.

Revision `fa02cc66` passed Windows application workflow
[41270487906](https://github.com/Gevroska/veylurk/actions/runs/41270487906), including
108 Node tests, and the shared native source passed all 60 tests in workflow
[41270487924](https://github.com/Gevroska/veylurk/actions/runs/41270487924).
A bounded test-only rotation accepted `strano` samples with 32 and 31 accounts
in 7.070 and 7.800 seconds, with a successful `normanajari` navigation between them.
All four rotation attempts succeeded. A later attempt stopped for the cleanup
check remains recorded as cancelled. The temporary Fixed/30 cadence was restored to
the saved Adaptive/300 setting. Normal browser cleanup was confirmed in 518 ms,
with no remaining profile processes before collection was re-enabled.
The 24,974 original IRC observations, 182 Community attempts and 15,986 member
receipts compared identically; encrypted API credentials and website sign-in were
reused. Production port 8795 and its executable were not modified or restarted.

## Auxiliary Community window dismissal and stop evidence

The unattended test host later reported `browser_closed` with no primary source
failure and confirmed normal cleanup, while IRC and metadata stayed active. The
old service `WM_CLOSE` handler called the same genuine-close path as the standalone
proof. That path fits the observed terminal state, but no initiation reason was
recorded at the time, so the historical actor and exact stop time remain unknown.

The candidate makes service `WM_CLOSE` a presentation-only minimization. Accepted
and active work, admission, cancellation version, controller epoch, denial state
and coordinator scheduling remain intact. Closing the sign-in window preserves
the owner's pause; reopening sign-in and explicit Resume remain separate controls.
Disable and shutdown still close the host, and the standalone proof still closes.
No player, authentication, media, provenance, deadline or cadence gate changes.

A typed first-wins termination snapshot now distinguishes explicit shutdown,
command-channel loss, startup/source failure, uncertain cleanup, pump quit/error
and the exit guard. It survives cleanup through the reserved terminal slot.
The coordinator alone queues that allowlisted snapshot through the existing
bounded diagnostics writer, without occupying `first_storage_failure` or adding
a database schema or writer. The API also exposes a bounded service-window
dismissal count and latest timestamp. New host creation resets these in-memory
fields. Unexpected terminals close open Community runs once with `host_failure`;
normal retirement preserves its explicit boundary reason. Historical gaps are
not backdated.

Source regressions cover the actual shared window-close handler for service,
sign-in, standalone proof and pending shutdown; active admission and denial
preservation; first-reason retention through cleanup and the guard; pump/startup
paths; pending/idle run boundaries and retirement; and bounded durable diagnostic
records, redaction and first-storage-failure isolation. Independent source review
accepted the correction. Revision `08c1fa34` passed Windows application workflow
[41303153994](https://github.com/Gevroska/veylurk/actions/runs/41303153994), including
110 Node tests, and shared native workflow
[41303153992](https://github.com/Gevroska/veylurk/actions/runs/41303153992) passed 66 tests.

The test-only rollout reused the saved website session and API credentials and
saved fresh samples for both selected channels without changing Adaptive/300.
Explicit Disable confirmed real browser cleanup in 517 ms with no remaining
profile process. Its `shutdown_requested` snapshot was verified in the existing
on-disk journal with no first storage failure; the Community run ended as disabled.
Collection was then re-enabled with its original settings. All 25,579 pre-update
IRC observations, 201 Community attempts and 16,533 member receipts compared
identically. Production port 8795 and its executable were not changed or restarted.

After desktop access became available, a real native title-bar Close minimized
the service window and incremented its dismissal counter without stopping the
host or pausing collection. Fresh durable samples followed for NormanAjari at
13:02:44.680 UTC and strano at 13:03:20.695 UTC on November 1. A native Close in
the owner sign-in view also minimized the window and preserved its pause.

Explicit sign-in reopening restored a visible window without desktop activation,
but a subsequent navigation failure exposed another defect: the service recycled
its controller after consuming the login request, leaving a blank replacement
with the initial starting label and no API error. The bounded fault confirms
`navigation_failed`; the original underlying WebView2 error was not retained, so
neither a runtime update nor a particular network cause is established.

The correction keeps owner sign-in failures separate from collection failures:
it retains the browser, shows an explicit reopen instruction, and publishes one
bounded transient failure without recording a source denial or replaying login.
An allowlisted origin and numeric HRESULT or WebView2 error status accompany the
fault. Explicit reopening and Resume retain their existing cleanup boundaries.
Focused native and coordinator regressions cover stale callbacks, failure
latching, event saturation, denial preservation and recovery. Independent source
review accepted the correction. Revision `fb48e0e8` passed Windows application
workflow [41310526731](https://github.com/Gevroska/veylurk/actions/runs/41310526731):
110 Node tests, 233 ordinary library tests, the isolated storage-recovery check,
three synthetic scale workloads, and packaged startup, shutdown and backup
checks. Shared native workflow
[41310526730](https://github.com/Gevroska/veylurk/actions/runs/41310526730) passed
70 tests.

One explicit Resume restored collection, with fresh durable samples at
13:11:23.380 UTC for NormanAjari and 13:11:58.872 UTC for strano. Adaptive/300 was
restored, encrypted API credentials compared identically, and production port
8795 retained its original process identity. The original historical Close sender
is still unrecorded and cannot be identified retrospectively from the evidence.

The corrected build's native sign-in Close again minimized its window and kept
the owner pause. Explicit reopening produced a visible, rendered, signed-in
Twitch homepage without desktop activation. This run also captured a current
`NavigationCompleted` failure with numeric WebView2 status 9, `ConnectionAborted`.
The helper retained its browser and showed the transient error instead of
recycling into a blank window. This is successful reopening with an observed
navigation interruption, not error-free navigation or proof of why Twitch
interrupted loading. No automatic retry, error suppression or collection-gate
relaxation was introduced. Explicit Resume then saved fresh NormanAjari and
strano receipts at 13:34:25.416 and 13:35:03.954 UTC; Adaptive/300 was restored.

Final verification encountered a separate storage-access delay: cached status
remained available while durable work and independent report writes stalled.
Only the two identified read-only inspection helpers were cancelled. Storage
access and collection later advanced without restarting Veylurk; no app was
force-killed and no database sidecar was removed. An offline history read also
ran slowly, and a later disk measurement reported 20 queued operations on R:.
These observations do not establish the precise cause or a code fix for that
delay. After a verified clean application exit, the offline comparison passed
with integrity `ok`: all 25,792 pre-update observations, 59 sessions, 246 Community
attempts and 17,669 member receipts remained identical. The test was restarted
with its original settings and saved credentials. Production was not modified
or restarted throughout these checks.

That restart restored a persisted `source_denied` pause. Shutdown cleanup
uncertainty can currently produce this classification even without a Twitch
rejection: a terminal with unconfirmed cleanup passes `cleanup_unconfirmed`
through the generic persisted-pause fallback. The latest run ended as shutdown
and its last attempts succeeded, making this the leading explanation here; the
exact earlier cleanup result was not retained. After confirming the old test
process had exited and no process used its dedicated browser profile, one
explicit API Resume restored collection. No SQL edit or automatic retry was used.
A remaining classification improvement is to distinguish cleanup uncertainty
from source rejection while preserving the persisted restart barrier.

## Earlier four-item completion increment

The [Astra plan](completion-plan-2026-11-01.md) has a final evidence ledger; the
[completion report](completion-results-2026-11-01.md) records the four separate
outcomes. Cleanup classification/v3 barriers, memory-only storage activity,
bounded read responses and the offline numerical/simulation/reference tool are
implemented and verified at source `4ea5d9e`. Windows app/native CI, independent
review and relevant local checks passed. The test instance on 8796 retained its
history, credentials and sign-in; both original channels produced fresh durable
receipts after final restart. Production on 8795 was unchanged.

The measured page-retirement optimization reduced comparable idle private memory
from about 840 to 156 MiB (~81%) and stable idle CPU from 30.6% to 0.62% of one
core (~98%). These are idle-phase measurements, not overall throughput gains.
The same 1 GiB guard ended the candidate circuit after 231.663 seconds: four
successful channels, six initiated attempts and 24 configured. One attempt timed
out and one was cancelled during cleanup. Full many-channel Community breadth
therefore remains partial, and active-page memory remains a residual cost.

The storage observation and new activity endpoint improve diagnosis and bounded
read responsiveness. The historical and recurrent R: read-delay cause remains
unresolved; no disk, Storage Spaces or Syncthing setting was changed. See the
[storage investigation](storage-investigation-2026-11-01.md).

All nine 200-trajectory simulation scenarios completed. Correlated draws and
stale positives exposed model sensitivity; no settings were tuned after holdout
inspection. Strong-absence error was unmeasured because no strong-absence
classification occurred. The autonomous two-live-channel proxy pilot was
resource-truncated and yielded zero usable complete-window comparisons. It does
not supply independent membership or absence truth. Independent empirical 99%
calibration remains `not_run_missing_independent_reference`; the production model
retains `uncalibrated` and its existing defaults. No further owner-supplied
dataset question is pending.


## Chat-only source and additional RAM reduction

The owner authorized Popout Chat and on-demand browser creation on 2026-11-01.
Astra's [source/lifecycle plan](community-ram-optimization.md) defines strict
route-specific readiness, preserved source/authentication guards, dormant command
handling and a bounded isolated qualification. Implementation, independent review,
exact Windows CI, signed-out rejection and repeated signed-in collection passed
at source `3a685ed14897bc5e7247f0651a8f5147d5d95334`. The
[qualification report](community-ram-results-2026-11-01.md) records about 12 MiB
browser-free private memory and a 467 MiB observed peak in the separate C:
database trial. The browser is released between samples.

At that revision, full 24-channel resource qualification remained partial: the
R: run stopped after 12 accepted receipts on probe stalls, and the C: run stopped after 21 when
process metrics became unavailable. All recorded receipts were independently
verified; neither interrupted run was reclassified as passing. R: responsiveness
remained unresolved. The corrected C: circuit is recorded in the next section;
R: load responsiveness remains unqualified. These changes do not
establish empirical 99% presence calibration.

Both retained-history prefixes matched, the complete latest test database was
returned to its original location, and credentials and original settings were
preserved. The test instance on 8796 produced fresh durable samples from both
original channels after restart. Production on 8795 remained unchanged.

## Reliable measurements and hidden collection window

The owner requested a repair of process measurements and a fully hidden browser
except during explicit sign-in. The integrated window previously used minimize;
the correction uses a hidden native host while retaining WebView2 rendering.
Native visibility transitions have independent source review and exact Windows
CI: 99 native tests and the complete application workflow passed at source
`881ccdd8fd3c6b5d5280df308ede5dc643ab889a`. A fresh anonymous profile loaded
chat/Community while hidden and accepted no sample. After an initial retained
integrity denial and sign-in navigation abort, the owner confirmed the normal
Twitch window appeared and closed it. One deliberate normal Resume then cleared
the pause. Three hidden signed-in samples and a full 24-channel C: circuit passed
independent durable receipt/member/provenance checks. The circuit completed in
969.736 seconds with no collection errors and a 500.809 MiB observed combined
private-memory peak. These were 24 configured channels, not 24 confirmed live
channels. Explicit sign-in remains the only integrated flow that shows the window.

The measurement repair uses identity-bound process handles, bounded lifecycle
reconciliation, explicit missingness and conservative CPU/continuity rules.
Deterministic lifecycle and native fake-child exit tests, independent review and
five real-process measurements of the app plus seven WebViews passed. The full
circuit recorded 953 valid and five isolated unavailable process captures; one
valid reconciled capture retained a continuity break. Missing readings remained
unknown. Existing resource, source and timeout guards remained in force.
Historical measurement exceptions were not recorded, so their exact cause cannot
be retroactively established.

Strict eight-table recovery/schema checks, 44 deterministic verifier fixtures and
both immutable history prefixes passed. The complete latest database was returned
to its original R: test path after normal exit, with the original crash triplet
preserved separately and all hashes verified. The restarted test on 8796 retained
all 28 channel flags, policy/cadence, profile and saved API credentials. Both
original channels produced fresh durable samples; metadata and storage were
healthy with no current collection error. Production on 8795 remained unchanged.

The previously running build also exited with a native WebView2 access violation
before the new executable was installed. Its committed history and WAL are
preserved; recovery correctly marked its unfinished sample interrupted. No
source-supported explanation or repair of that older crash is claimed. R: storage
responsiveness under a 24-channel load also remains unqualified. See the
[current results](measurement-visibility-results-2026-11-01.md). Production remains
excluded.

## Native browser crash correction

The owner requested correction of the earlier WebView2 access violation on
November 2. Independent scoping found no source-supported causal repair from the
retained event metadata. A same-executable isolated native worker is implemented
and verified at `20ddb45f8b97843e57b0f7e43f8a4a35ece70561`. Exact native Windows
checks passed 121 tests, and the complete application workflow passed, including
storage recovery, scale workloads and normal release packaging. Independent review
has no remaining material findings; the supplementary local process suite passed
86 tests with its native browser body explicitly stubbed.

Isolated idle and active-sample worker failures preserved the main HTTP/IRC/database
process. The active sample failed exactly once, the worker tree exited with numeric
`0xc0000005`, and a new IRC session was durably created afterward. Community retains
its pause and unconfirmed normal cleanup; recovery requires restart and explicit
Resume. This contains the native crash without claiming its DLL cause is fixed.

The normal signed-in test on 8796 retained its settings, all 28 channel flags,
history and encrypted credentials. Both original channels produced fresh durable
samples; all eight pre-upgrade history prefixes and integrity checks passed.
Production on 8795 was unchanged. See the [plan and verified results](native-worker-crash.md).
