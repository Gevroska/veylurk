# Requirements and remaining work

Updated: 2026-10-31 (Europe/Paris).

The owner has authorized implementation of the remaining features. The earlier review-only restriction and Astra-only allocation are superseded. Current AGENTS.md governs coordination: Sol High/XHigh handles implementation, testing, integration and delivery; Astra XHigh receives bounded complex scoping or critical analysis when justified.

## Product scope and collection decision

The application collects received public Twitch IRC membership evidence autonomously, with optional official broadcast metadata. It records chat-connected accounts, including some accounts that never post. Chat membership is a different population from video viewers. A connected socket, a NAMES terminator or a viewer-count total does not establish complete membership.

The original Community-panel sampling requirement remains unmet. Historical direct HTTP and autonomous browser experiments did not produce a maintainable working source; the recorded CommunityTab request failed an integrity check. These probes are not a production collector. No integrity or authorization control is bypassed by this implementation.

On 2026-10-31, the owner superseded the anonymous-only requirement and authorized using a dedicated Twitch account. The Chrome for Testing sign-in attempt displayed Twitch's unsupported-browser error; this message does not identify its cause. No replacement browser was installed. The owner then signed in through the in-app Browser. Two native CommunityTab responses on a verified live channel returned 149 and 153 distinct accounts, including 100 viewers each, with 254 accounts in their union and no source errors. These are observations from that browser session; they establish neither autonomous operation nor statistically independent samples.

The owner explicitly approved the existing WebView2 runtime after its Edge engine was explained. An isolated native Rust source proof is under `probes/community-webview2/`; it has its own profile, normal owner-operated sign-in, manually armed bounded samples, and aggregate-only evidence. Its Windows build and live rendering, login, session persistence, Community access, and cleanup are separate acceptance gates. Passwords and verification codes are entered directly into Twitch, never into chat or Veylurk's database. No cookies or other browser sessions are imported. The production collector, credentials, and history remain untouched; periodic Community sampling is not integrated into the application.

Official documentation checked on 2026-10-25 states that IRC JOIN/PART delivery is unavailable above 1,000 chat-room users. Get Chatters requires authorized broadcaster/moderator access and the relevant scope; app client credentials alone do not authorize arbitrary third-party channels. See [IRC membership limits](https://dev.twitch.tv/docs/chat/irc/#part-command) and [Get Chatters](https://dev.twitch.tv/docs/api/reference/#get-chatters).

Therefore population-adaptive sampling, calibrated statistical absence, exact individual viewing periods and 100% viewer discovery cannot be claimed. The evidence model labels supported visits and observation-only ambiguous segments, with uncertain boundaries. Point-in-time candidates are either an exact positive receipt or an explicitly uncertain inference within source-supported boundaries. No result is a verified viewing duration.

For independent uniform samples of size k from a fixed population N, the conditional chance of missing a continuously present account in m samples is (1-k/N)^m. This is not a probability of departure. The implementation does not apply that formula to IRC events, video-viewer counts or unvalidated correlated samples.

## Requirement inventory

| Original requirement | Implementation / remaining acceptance |
| --- | --- |
| Rust application on Windows | Implemented; standalone executable and embedded assets, packaged by Windows CI. |
| Simple English localhost interface | Implemented; listener on 127.0.0.1 with Host/Origin/JSON checks. |
| Enter a channel list | Implemented; bulk normalization, deduplication, pause/resume/removal, up to 50 tracked channels. History survives removal. |
| Autonomous operation without Brave or extensions | Implemented with one IRC WebSocket. No browser runtime is needed for collection. |
| Repeated Community-list samples | Two signed-in in-app Browser responses verified; independent WebView2 proof and production scheduling remain pending. Historical probes and account setup do not fulfill this feature. |
| Observe silent accounts | Partial source support: NAMES/JOIN/PART can expose nonposting chat-connected accounts; delivery and completeness are unknown. |
| Discover 100% of video viewers | Unsupported by the available evidence and access rights. |
| Population-adaptive request cadence | Source-blocked. Current IRC collection is event-driven; joins, retries, network backoff and metadata requests are paced. |
| Detect live state and retain titles/details | Implemented using optional Helix credentials; retains stream identity, start, titles/category/language, viewer-count checks and observed offline checks. Failures and stale checks remain Unknown. |
| Collect during live broadcasts | Implemented selectable All enabled channels / Live channels only policy, persisted in SQLite. Live-only pauses only after a fresh successful offline check. Unknown or stale metadata keeps observation enabled. |
| First/last positive observations | Implemented per account/channel evidence visit and broadcast summaries; PART never extends the last positive receipt. |
| Departure and later return | Evidence-supported visits split at PART and collection boundaries. Post-PART delayed NAMES become ambiguous observation-only segments until a JOIN supports a new visit. All boundaries remain uncertain. |
| Statistical absence estimates | Source-blocked. Silence alone does not close a visit or prove absence. |
| Search account to channels/broadcasts and reverse | Implemented exact-login filters, range queries and bounded pagination. Broadcast summaries distinguish positive receipts inside the window from uncertain earlier-connected candidates. |
| Suggest known account and channel logins while typing | Implemented at `b93c8078`, with bounded local prefix suggestions and mouse/keyboard selection. Windows checks and an owner-approved test rollout passed; existing production was preserved. Suggestions never query Twitch or change collection settings. |
| Date/time searches | Implemented inclusive From / exclusive Until filters, normalized to UTC with validation. Broadcast intersections cannot leak disjoint time ranges. |
| Who was present at an instant | Implemented evidence-only view: exact positive receipt / inferred candidate / unknown; future times are rejected, collection outages and departures are not bridged. |
| Readable timelines | Separate visits retain positive receipt bounds and uncertain ends. The account chart compares channels on a shared axis and clips an account's chart to a selected broadcast. Direct receipt points, patterned inference, departures and unknown periods remain distinct. Automated validation and live rollout are separate gates. See [timeline interpretation](timeline.md). |
| Historical titles on hover and drag zoom | Current increment adds local historical metadata lookup on evidence hover, with sampled/unknown titles distinguished, plus mouse selection and keyboard zoom/reset. Zoom preserves pinned search bounds and never invents evidence. Broadcast search presents the viewer first and makes the channel restriction optional. Native and live acceptance remain separate gates. |
| Account graph with 99% presence/absence certainty | The owner accepted patterned inferred intervals alongside unknown periods. A calibrated 99% presence/absence claim remains unsupported by the current source; missing observations never become verified absence. |
| Per-broadcast statistics | Implemented unique associated accounts, observed accounts, inferred-only accounts, supported visit count and positive receipt count; ambiguous segments are excluded from supported visits. |
| Title/category/viewer-count history | Implemented paginated stored-check table and accessible unconnected viewer-count sample chart. No interpolation across missing checks. |
| Visible current channel live state and quality | Implemented live/offline/unknown, per-room joining/collecting/unavailable/gap state, last positive receipt and unknown coverage labels. |
| Individual unavailable-room recovery | Implemented paced retries with bounded backoff and a new evidence session per attempt. Healthy rooms retain their sessions; channel configuration changes do not require a global reconnect. |
| Retain history indefinitely, no CSV | Implemented without automatic expiry or CSV. Disk use remains finite and visible. |
| Lightweight dozens-of-channels operation | Architecture remains one socket, bounded queues and serialized SQLite. Synthetic 10/25/50-channel retained-history tests added; extended live scale acceptance remains pending. |
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
- Reconsider Community sampling only if a reproducible, permitted autonomous source is demonstrated. Validate freshness, caching, selection bias, population identity and correlation before introducing statistical estimates or adapting sample cadence.

## Current development evidence

The history/runtime increment at `4cb1c257` passed Windows CI, including 95 Rust tests counting the three explicit scale cases, 25 JavaScript tests and packaged operations. Its subsequent short live trial verified two genuinely live channels, both search directions, broadcast associations, themes, policy persistence and graceful stop. All 8,538 original raw observations and eight original session boundaries were verified unchanged; original rollback data and encrypted credentials were preserved.

Two initial starts of that revision stopped with generic storage errors; later foreground and hidden background runs were healthy. Their underlying error was not retained, so those earlier causes remain unresolved. Diagnostic revision `4df70187` subsequently passed 106 Rust tests, 27 JavaScript tests and packaged Windows checks. Its owner-authorized live starts then captured two real append cancellations by the all-job progress budget. The correction now makes every production job explicitly a cancellable query or a durable mutation, while preserving FULL durability, a single connection, transaction/batch limits, bounded receipt admission and fail-stop behavior. Separate deterministic tests cover durable success beyond the old budget, query cancellation, and exceptional SQLite write interruption. No partial flush is blindly retried. See [diagnostics and policy](diagnostics.md).

Local validation uses isolated fixtures. Rust execution is delegated to hosted Windows MSVC CI because the available local GNU toolchain lacks its native C build dependencies. Final CI/run/artifact identifiers and live validation belong to the candidate verification record; do not reuse the earlier two-channel acceptance as evidence for this revision.

UI visual checks use a clearly marked fictional localhost fixture, without a collector or database. They establish interface rendering and interaction only. The already approved application may keep collecting during development; each newly built executable still requires its own firewall launch approval before live validation. Short trials of the earlier revision are not acceptance of the new binary.

## Further constraints

Windows, English source/UI/documentation, local credentials, indefinite retention and no CSV remain unchanged. Source limitations are outstanding original requirements rather than silently completed items. No routine stage-by-stage permission gate is imposed. The only planned interruption is the owner's new-executable firewall gate or a concrete product decision that cannot be resolved from existing authorization.
