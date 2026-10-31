# Standalone Community WebView2 source proof

This isolated Rust Windows proof uses the existing WebView2 runtime and its
executable-local `data/browser-profile`. It does not change the production
Veylurk service, SQLite history, credentials, or IRC evidence. Manual Community
access and profile persistence were demonstrated with an earlier revision;
the accepted finite autonomous run observed four correlated samples across two
channels and circuits, including three after verified minimization. This shared
host extraction still requires its own Windows checks and application acceptance.

Parsing, request correlation, player/deadline gates and native behavior now live
in `crates/community-source`; this proof is a thin aggregate-only adapter. Its
independent lockfile also records that local path dependency. Run the shared
crate's tests as well as the proof's CLI/build checks.

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
Timing fixtures cover a network finish preceding the response-event timestamp,
unknown transferred bytes, numeric-only failure diagnostics, and unchanged body
limits and freshness claims in those cases.
Typed player-state tests cover pause confirmation, stable signed-in chat
readiness, readiness loss, playback resuming, idle checks, and navigation reset.
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

## Finite autonomous paused-channel mode

Use the same executable folder and existing app-owned profile:

```powershell
.\veylurk-community-webview2-proof.exe --autonomous --channel first_channel --channel second_channel --circuits 2 --max-seconds 240
```

Replace the two placeholders with actual permitted channel logins. This command
is an example, not a launch performed by the build. A new Internet-capable
executable still needs the owner's launch approval.

The mode accepts exactly two or three distinct channels, one to three circuits
(default two), and a 60–600 second total limit (default 240). One controller
visits the normal `https://www.twitch.tv/CHANNEL` page in channel order, with all
attempt starts at least 30 seconds apart. It waits for the normal player and
uses its ordinary Pause control at most once per navigation when needed.
The attempt has one 25-second navigation/UI budget, then a 25-second
native-response budget. Total runtime includes startup and waits; closing gets
at most five additional seconds for browser-process exit confirmation.

The existing profile is required. No automatic login is attempted. The source
page must show the signed-in User Avatar, chat structure, Community control,
and a video whose paused state is corroborated by the normal Play
control. These prerequisites must remain ready for at least two seconds before
observation is enabled. Structural selectors are conservative candidates; the
runtime must validate them. No account identity, chat content, or media URL is
returned or retained by this UI check.
Video decoding/buffering readiness and ended state do not gate sampling. Their
combined `video_ready` flag is diagnostic only: the proof never loads or plays
media to make that flag true.
During initial page setup, a preliminary paused/Play observation does not latch
readiness. Changing controls reset the stability interval. If playback starts,
the one allowed normal Pause action is followed by fresh confirmation and a full
two-second stability interval; the proof does not assume the click took effect.

Pause is checked again immediately before the ordinary Community click and
after the response body arrives, before sample acceptance. The same checks
continue during response waits and between attempts. Once stable readiness is
established, actual resumed playback or a missing video stops the run. A missing
Play control while all videos remain paused instead withholds the trigger or
sample acceptance until two seconds of stable reverification. Waiting does not
reset the original deadline or repeat an actual Community click. Evidence
distinguishes a trigger deadline, player-reverification deadline, and native
response timeout. The next navigation gets a new pause/readiness gate. There is no
mute substitute, media URL request, direct player API call, ad skip, DOM removal,
or background/occlusion flag. A paused video does not prove absent video network
traffic or low resource usage. Native buttons cannot redirect the autonomous
run; close the window to stop it.
Minimization preserves the last normal controller bounds so the page keeps its
ordinary layout. Native background throttling and occlusion behavior are unchanged.
Autonomous mode minimizes its own native window exactly once, immediately after
the first accepted sample is saved. It uses ordinary `SW_MINIMIZE`, records the
monotonic `minimize_milestone_ms`, and verifies `IsIconic` in `minimized_verified`.
Failure to verify minimization stops the run. Later receipts must follow that
milestone to establish minimized collection. Manual mode stays visible; no
visibility, suspension, or browser flags are changed.

This route is explicit: autonomous mode never tries the chat popout and never
falls back or retries after a denial. A normal full-channel control succeeded
after a separate denied popout run; that does not establish the denial's cause.

There is at most one ordinary Community click per attempt. No failed attempt is retried:
a channel that reaches a response deadline is skipped in later circuits while
its paused page remains guarded. Navigation/UI failure stops the run because
pause cannot be assured during a scheduling gap. HTTP 401/403/429, integrity,
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

Every actual Community trigger passes a shared generation/monotonic-deadline
check immediately before script dispatch. The script also checks an immutable
expiry immediately before its click, using the same browser performance clock
as the request-trigger timestamp. A preceding read-only UI result supplies that
clock anchor in both manual and autonomous modes. Callback/queue delays shorten
the browser budget; they never renew it or convert system wall time into browser
time. An expired trigger records `community_trigger_deadline` without a click
or rearming.

The response event uses its reporting timestamp, while the finish event can
carry an earlier network-stack timestamp. Chromium's own frontend handles this
ordering. The proof preserves the raw matched timestamps, their difference, and
an explicit ordering flag; it does not adjust them or use their relative order
as an identity test. Both timestamps must still be finite and no earlier than
the matched request start. Blink's encoded-length sentinel `-1` means unknown:
`encoded_bytes` is then null and cannot establish a network-response provenance.
Other negative or nonfinite lengths fail closed. Decoded-body and protocol
envelope limits apply independently of the reported transferred length.

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
Each attempt also records video presence/paused state, whether pause was ever
verified and verified before the trigger, normal Pause click count, the last
pause-check time, and whether signed-in UI was observed. These are point-in-time
observations; they do not establish continuous absence of playback or traffic.
The last structural player observation is retained before applying the gate,
including on failure. Its eight fields contain only booleans or null, separating
video readiness, actual pause, Play-control presence, and channel UI readiness.
Matched-source timing failures retain at most one numeric diagnostic containing
the raw finite timestamps, differences, encoded length, and a specific reason
code. Nonfinite numbers become null. No request IDs, URLs, headers, or accounts
are included in that diagnostic.

A new native response can be cached or contain the same accounts. Explicit
cache/service-worker flags, missing cache metadata, and an observed network
response are distinct. `server_freshness` remains `unknown`, and
`fresh_server_samples_proven` remains false: network receipt and changing sets
cannot establish server freshness, independence, randomness, full coverage,
viewing time, or calibrated absence. No samples become IRC NAMES/JOIN events.

Live acceptance requires positive native samples from normal full-channel pages
with verified paused playback; multiple channel circuits and profile restart;
correct channel isolation and useful repeated-sample evidence; and measurements
of the whole browser/app process tree. Check minimized operation separately.
Build success and fixtures do not establish these conditions. Cache-only or
unknown evidence must not be reported as fresh sampling.

## Documentation

- [webview2-rs](https://github.com/wravery/webview2-rs), pinned `webview2-com`
  0.39.1 and `windows` 0.62.2.
- [WebView2 DevTools protocol](https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/chromium-devtools-protocol).
- [Network protocol events](https://chromedevtools.github.io/devtools-protocol/tot/Network/).
- [Chromium network event timestamps](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/core/inspector/inspector_network_agent.cc):
  `DidReceiveResourceResponse` and `DidFinishLoading` use event-reporting and
  network-finish times respectively.
- [DevTools request end-time handling](https://github.com/ChromeDevTools/devtools-frontend/blob/main/front_end/core/sdk/NetworkRequest.ts):
  the `endTime` setter explicitly handles finish before response-received time.
- [Blink unknown encoded length](https://github.com/chromium/chromium/blob/main/third_party/blink/renderer/platform/loader/fetch/url_loader/url_loader_client.h):
  `URLLoaderClient::kUnknownEncodedDataLength` is `-1`.
- [WebView2 user data folders](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/user-data-folder).
