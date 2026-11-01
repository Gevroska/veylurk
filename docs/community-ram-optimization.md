# Chat-only Community sampling and browser dormancy

The owner requested Twitch's normal Popout Chat page and further RAM reductions on 2026-11-01. Astra recommended two coordinated changes: remove the full stream page from automatic sampling, and create the embedded browser only when a sample or owner sign-in needs it. Production is excluded from rollout and measurement.

## Source contract

Automatic collection uses `https://www.twitch.tv/popout/{channel}/chat?popout=`. The expected source mode follows the requested channel through navigation, UI checks, root-frame identity, request correlation and final response validation. A full-channel redirect cannot satisfy a popout attempt. There is no fallback to a heavier page after a denial or source mismatch.

Popout readiness requires the exact page, positive signed-in controls, usable chat and Community controls, and no video element, stable for two seconds. Unexpected video fails the attempt; it is not silently paused or accepted as a different source mode. The legacy manual full-channel proof retains its separate paused-video requirement. Popout evidence reports generic source readiness without claiming a verified player pause.

The ordinary Community click and native observed-response path remain in use. Generation/controller epoch, navigation/top-frame identity, operation/channel matching, trigger deadlines, cache checks, denial handling and response bounds retain their existing roles. Network receipt still does not establish server freshness, independent random samples, complete viewing coverage or calibrated probabilities.

After an acknowledged Community click, the composer can disappear briefly before the panel mounts. Only that opening phase can wait with authentication unobservable: the exact composer structure must remain, both positive controls must be hidden, the opener must remain usable, and no visible malformed panel, Login, challenge or video may appear. This waiting state cannot advance the phase, freeze or accept a body, or renew the original deadline. A confirmed panel and fresh positive post-close authentication remain mandatory.

The actual popout layout lacks the full page's User Avatar control. Its positive identity control is checked together with editable chat and absence of a login control. Opening Community hides these controls: authentication is explicitly unobservable during that bounded panel phase, never cached as true. The collector freezes a fully validated response, disables observation, closes the observed panel through its ordinary Close control, and re-establishes visible positive authentication for two seconds before publishing the original receipt. Route, media, denial, epoch and original deadline checks continue throughout; failed restoration produces no sample. Before qualifying that predicate, a fresh anonymous profile must reach the chat UI without satisfying authentication or producing a sample; incomplete page loading is inconclusive. The signed-in profile must subsequently produce successful samples. Passwords, cookies, tokens and account-name text are not extracted for this check.

## Dormant lifecycle

The integrated host can accept bounded commands while dormant, without a browser process. Admission-ready means the host can receive work; it does not assert browser, authentication or source readiness. A valid sample or owner sign-in creates the browser in the existing application-owned profile.

Initialization consumes the accepted sample's original queue deadline. Cancellation, shutdown, channel closure and stale publications must work before an engine exists. Only one creation can be in flight; a late controller cannot navigate for an expired or cancelled command.

After a successful sample, the host retains the existing immediate retirement and matching normal-process-exit confirmation. It then stays dormant instead of recreating an empty browser. Cleanup uncertainty and source denials remain barriers. Shutdown of an already dormant host requires no fabricated browser-exit event. The finite autonomous proof shares the popout source contract but keeps its existing finite controller schedule; it does not qualify the integrated host's dormancy.

No memory-trimming API, disk-swapping policy, browser flag, engine installation, website-profile import or sampling-frequency reduction is part of this change. Browser startup latency is the principal tradeoff.

## Bounded qualification

1. Verify source-mode, UI, provenance, lifecycle, queue-deadline and cancellation regressions in the existing Windows workflows.
2. Back up the latest isolated test history and credentials after normal exit; verify the existing production identity independently.
3. Run one anonymous-profile negative check with the approved executable on port 8796 and a separate empty database/profile. Preserve the normal signed-in profile.
4. Reuse the signed-in profile for three scheduled samples from a currently live channel, with the existing Fixed/30 settings and unchanged global pacing. `emmodem` was the initial choice; qualification used `otplol_` after the official live check found `emmodem` offline. Stop at a denial, unconfirmed cleanup or the existing 1 GiB investigation threshold.
5. Measure app and descendant browser private memory, working set, process counts, responsiveness and source outcomes. Verify process disappearance during waiting gaps and durable accepted receipts. CPU deltas across process churn remain unavailable.
6. Only after the short check succeeds, attempt a bounded first circuit of the existing 24-channel set. Restore all original settings/row flags and verify both original channels afterward.

Previously recorded full-page measurements are comparison evidence, not a new matched benchmark: roughly 156 MiB idle private memory included about 146 MiB in browser descendants; active-page private memory reached about 1 GiB. Private allocation and resident RAM are reported separately. Do not repeat a known heavy baseline simply to manufacture another resource-limit stop.

## Acceptance ledger

| Gate | Status | Evidence |
| --- | --- | --- |
| Astra design and scope | Verified | Explicit popout mode and dormant lifecycle; no further owner decision needed. |
| Implementation and independent review | Verified source | Strict route/readiness, unobservable opening/panel phases, frozen receipt, explicit-denial priority and dormant lifecycle independently reviewed. Opening-transition and restore-deadline classification regressions are covered. |
| Exact Windows CI and artifact | Passed | Source `3a685ed14897bc5e7247f0651a8f5147d5d95334`: native CI passed 96 tests; complete app CI passed tests, recovery, scale, build and packaging. The normal release artifact and executable hashes are recorded in the results report. |
| Anonymous negative and signed-in positive checks | Passed | Corrected fresh anonymous profile loaded chat/Community without positive authentication or accepted samples. Three signed-in `otplol_` receipts were independently verified in SQLite; original channels also produced fresh receipts after final restart. First-failure evidence remains retained. |
| Resource gains and breadth | Measured; full breadth partial | The corrected short trial measured 428 MiB peak private memory and about 11 MiB without browsers. The 24-channel R: run stopped after 12 receipts on probe stalls; the separate C: database run stopped after 21 receipts when process measurements became unavailable. That run recorded 467 MiB peak and about 12 MiB browser-free private memory. Neither interrupted circuit is a full 24-channel qualification. |
| History, credentials, settings and production preserved | Passed | Both immutable history prefixes matched; the complete latest database was returned to R: with hash verification. Saved credentials and all 28 channel flags/policy/cadence were preserved. The usual two-channel test instance is running on 8796; production 8795 remained unchanged. |

The [qualification report](community-ram-results-2026-11-01.md) preserves every
interruption and the measurement limits. R: storage responsiveness and the cause
of the transient process-measurement failure remain unresolved. C: results do not
qualify the R: deployment's responsiveness.


### Diagnostic build policy

The full Windows workflow retains recovery/performance failures and the normal
release artifact remains gated on complete success. When those two gates alone
fail, a distinctly named `veylurk-diagnostic-windows-x64` artifact may support
isolated investigation only after lint, unit/UI/operations, compilation and every
required offline smoke/package check pass. Its manifest records the exact source,
executable hash, failed gate outcomes and `release_qualified: false`. Building it
does not reclassify failed qualification or authorize production deployment.
