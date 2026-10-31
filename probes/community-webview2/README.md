# Standalone Community WebView2 source proof

This isolated Rust Windows proof uses the existing WebView2 runtime and its
executable-local `data/browser-profile`. It does not change the production
Veylurk service, SQLite history, credentials, or IRC evidence. Manual Community
access and profile persistence were demonstrated with an earlier revision;
this revision still needs Windows checks and live autonomous-popout acceptance.

There is no installer, runtime download, private HTTP client, token/cookie API,
request replay, imported profile, browser identity modification, security flag,
remote debugging port, Brave dependency, extension, or Playwright dependency.
Normal owner login remains manual. The browser keeps its ordinary session in
this proof's existing profile. Environment overrides for the browser, profile,
release channel, or arguments cause startup failure instead of being changed.

## Offline checks

The crate is an independent workspace with pinned dependencies and a committed
lockfile. With an existing Windows MSVC toolchain:

```powershell
cargo fmt --manifest-path probes/community-webview2/Cargo.toml --all -- --check
cargo clippy --manifest-path probes/community-webview2/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path probes/community-webview2/Cargo.toml --locked --all-targets
cargo build --manifest-path probes/community-webview2/Cargo.toml --locked --release
```

`--help` and invalid-argument handling return before COM, browser/profile
creation, or network access. Synthetic tests cover exact source selection,
role deduplication, source errors, redaction, response/member bounds, scheduler
fairness and deadlines, stale request IDs/generations, delayed trigger callbacks,
frame mismatch, caching, and repeated samples. They do not establish live access.
The Windows workflow packages only the executable, documentation, manifests,
and any required loader; it never packages a browser profile or live evidence.

## Manual owner login and source check

Run without arguments in a separate writable proof folder. The native **Open
login** button navigates to Twitch's normal login page. Sign in personally;
the host never inspects login fields, credentials, headers, or cookie storage.
**Check rendering** opens a built-in local page if native rendering is uncertain.

Enter one channel and choose **Open channel**. When chat is ready, close an
already-open Community panel and choose **Sample Community**. At most three
manual attempts are allowed, with starts at least 30 seconds apart. The host
enables passive native observation, verifies the root frame, and performs one
ordinary Community click. It waits at most 25 seconds for that click's native
response after setup. A denial disables sampling for the remainder of the run.

Normal closure preserves the profile. Reopen this same folder to test session
persistence; neither successful navigation nor a saved profile proves login or
source access. Successful later navigation clears the prior native error status.

## Finite autonomous chat-only mode

Use the same executable folder and existing app-owned profile:

```powershell
.\veylurk-community-webview2-proof.exe --autonomous --channel first_channel --channel second_channel --circuits 2 --max-seconds 300
```

Replace the two placeholders with actual permitted channel logins. This command
is an example, not a launch performed by the build. A new Internet-capable
executable still needs the owner's launch approval.

The mode accepts exactly two or three distinct channels, one to three circuits
(default two), and a 60–600 second total limit (default 420). One controller
visits `https://www.twitch.tv/popout/CHANNEL/chat` in channel order, with all
attempt starts at least 30 seconds apart. It never navigates to the full video
player. The attempt has one 15-second navigation/UI budget, then a 25-second
native-response budget. Total runtime includes startup and waits; closing gets
at most five additional seconds for browser-process exit confirmation.

The existing profile is required. No automatic login is attempted. A video
DOM element invalidates the chat-only prerequisite. The host records only its
presence flag. Native buttons cannot redirect the autonomous run; close the
window to stop it. Minimized-window operation remains a separate live check.

There is one ordinary Community click per attempt. No failed attempt is retried:
a channel that reaches a navigation/UI/response deadline is skipped in later
circuits. Other channels may continue. HTTP 401/403/429, integrity,
authentication, rate-limit, or challenge denial stops the whole run. Unexpected
protocol data, identity failures, or exceeded safety bounds also stop the run.
No CAPTCHA, challenge, account-recovery flow, or detection workaround is tried.

## Native request identity and bounds

Only the WebView2 native DevTools observation interface is used. `Network.enable`
finishes before the trigger; `Page.getFrameTree` identifies the source root
frame. Observation is disabled before app navigation, and event payloads are
never read on login or outside the exact selected source page.

`Network.requestWillBeSent` supplies the request ID, root frame, browser request
start time, and matching `CommunityTab`/`ChatViewers` operation/channel. A
browser-clock timestamp captured immediately before the ordinary click rejects
pre-trigger requests, even if their events arrive late. A bounded candidate
buffer accommodates the click callback arriving after request/response events.
The ID, frame, page, and navigation/attempt generation must remain consistent.

`Network.responseReceived` and `Network.loadingFinished` identify the same
request. Only then does `Network.getResponseBody` read that existing response;
it never sends a Twitch request. Batch index and length are checked before
parsing the selected result. Headers, cookies, security details, and unrelated
fields are ignored by typed event deserialization and never retained.

Limits are 192 KiB per event, 2,048 events per attempt, four pending matching
candidates, a 64 KiB request body, a 1 MiB decoded response, and 4,096 member
rows across all roles. Protocol response-string escaping has a separate finite
sixfold envelope limit. Unsupported base64 responses fail closed. There is at
most one response-body request and one UI callback pending per attempt.
Expired or superseded callbacks cannot save a sample or clear a newer callback.

## Aggregate evidence and acceptance

`aggregate-evidence.json` next to the executable is replaced each run. It
contains only public configured channels, safe phase/reason codes, attempt
start/end times, native navigation diagnostics, category/count totals,
per-channel overlap/union and duplicate-set flags, cache provenance, and browser
cleanup state. Account sets exist only in memory. Raw bodies, errors, page DOM,
headers, credentials, cookies, and individual account logins are never written.
Evidence-write failure stops the autonomous proof.

A new native response can be cached or contain the same accounts. Explicit
cache/service-worker flags, missing cache metadata, and an observed network
response are distinct. `server_freshness` remains `unknown`, and
`fresh_server_samples_proven` remains false: network receipt and changing sets
cannot establish server freshness, independence, randomness, full coverage,
viewing time, or calibrated absence. No samples become IRC NAMES/JOIN events.

Live acceptance requires positive native samples from the actual popout route
without a video element; multiple channel circuits and profile restart;
correct channel isolation and useful repeated-sample evidence; and measurements
of the whole browser/app process tree. Check minimized operation separately.
Build success and fixtures do not establish these conditions. Cache-only or
unknown evidence must not be reported as fresh sampling.

## Documentation

- [webview2-rs](https://github.com/wravery/webview2-rs), pinned `webview2-com`
  0.39.1 and `windows` 0.62.2.
- [WebView2 DevTools protocol](https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/chromium-devtools-protocol).
- [Network protocol events](https://chromedevtools.github.io/devtools-protocol/tot/Network/).
- [WebView2 user data folders](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/user-data-folder).
