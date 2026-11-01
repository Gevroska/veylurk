# Requirements and remaining work

Updated: 2026-11-01 (Europe/Paris).

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
primary failure from it after restart. Persistent fault reporting is deferred. Native and
coordinator regression tests cover identity, early/stale/abnormal exits, close during
recycling, cleanup operation failures and preservation of the primary failure.
Source review, hosted Windows checks and a short isolated test rotation remain
pending for this candidate. Production port 8795 and its executable are outside
this recovery task.
