# Standalone Community WebView2 source proof

This small Rust Windows application tests whether Twitch's normal Community tab
works in an app-owned WebView2 host. It is separate from the production Veylurk
application, collector, database, credentials, and running process. A successful
parser test or Windows build does **not** prove that Twitch accepts this host.
The source remains unproved until the live acceptance steps below succeed.

It uses the existing Microsoft WebView2 runtime (the Edge rendering engine). It
does not open the Edge browser or depend on Codex, Brave, Playwright, or another
browser. It contains no installer, runtime bootstrapper, or download mechanism.
If an existing runtime cannot be used, it shows a friendly failure and stops.
The MSVC build links the crate's supplied WebView2 loader statically; the runtime
itself is not bundled.

## Build and offline checks

The crate is an independent workspace. Run these commands from the Veylurk
repository with an existing Rust MSVC toolchain on Windows:

```powershell
cargo fmt --manifest-path probes/community-webview2/Cargo.toml --check
cargo clippy --manifest-path probes/community-webview2/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path probes/community-webview2/Cargo.toml --locked
cargo build --manifest-path probes/community-webview2/Cargo.toml --locked --release
```

The binary is `probes/community-webview2/target/release/veylurk-community-webview2-proof.exe`
unless CI supplies a different `CARGO_TARGET_DIR`. An explicitly selected
`--target x86_64-pc-windows-msvc` adds that target directory below the target root.
Package the executable and this README in a separate writable folder. Do not
replace the production executable or copy production data into this folder.

`veylurk-community-webview2-proof.exe --help` exits before COM initialization,
window creation, profile creation, or network access. Parser tests are portable
and contain only synthetic source fixtures. They exercise batch correlation,
channel case normalization, exact operation selection, role duplicates, source
denials, malformed data, size bounds, aggregate redaction, and three-sample bounds.

If `Cargo.lock` has not yet been generated, the first CI preparation run must
generate and upload it. Commit that exact lock and rerun all acceptance checks
with `--locked` before accepting the executable. No local dependency download or
tool installation is needed for this preparation workflow.

## Owner-operated live proof

If the browser area remains blank, **Check rendering** opens a built-in static
page that needs no network access. Its visible text separates local rendering
from Twitch page loading. **Open login** returns to Twitch. Browser diagnostics
in the aggregate JSON record only native action/navigation counts, navigation
IDs, success flags, WebView2 error numbers, document-loaded counts, and process
failure kinds. They contain no page text, URLs, credentials, or cookies, and
successful navigation never establishes account login or Community access.

Launch of this new executable that accesses the Internet remains a separate
owner approval gate. Nothing here launches the executable automatically.

1. Start the proof in its separate folder. It opens the normal
   `https://www.twitch.tv/login` page. Sign in manually using the owner's account.
   The app never reads login fields or supplies credentials.
2. Confirm that normal Twitch rendering and login work. Enter the live channel
   login and press **Open channel**. Wait for the page and chat to finish loading.
3. Close Twitch's Community panel if it is already open. Press the native
   **Sample Community** button. This explicitly arms a 25-second observation
   window and makes one ordinary Community button click in the current page.
   If Twitch's button selector has changed, click its Community tab manually
   during the same observation window. There is no automatic retry.
4. Read the native status line and `aggregate-evidence.json`. An accepted sample
   requires HTTP 200, no source errors, and the expected chatters shape for the
   selected channel. Each later attempt requires another native app button click
   and at least 30 seconds since the prior attempt. There are at most three
   manual attempts per process, and at most three successful samples.
5. Close the native window normally. Confirm the JSON records browser cleanup,
   and independently verify that this proof's WebView2 subprocesses have exited.
   If the event was not received within five seconds, the JSON says cleanup was
   unconfirmed; the app never force-kills a process.
6. Restart the same executable in the same folder and confirm the normal Twitch
   session persists. Rendering, successful account login, a valid source sample,
   normal restart persistence, and process cleanup are separate live gates.

Do not automate CAPTCHA, challenge, or account recovery. A matching source
integrity/authentication/challenge error, or HTTP 401/403/429, stops sampling for
the remainder of the run. The app makes no bypass or retry attempt.

## Observation and evidence boundaries

The native `WebResourceResponseReceived` event observes requests that Twitch's
page already made. The app only examines POST bodies at
`https://gql.twitch.tv/gql` while a sample is explicitly armed. It accepts the
exact operation `CommunityTab` with `variables.login`, or the older
`ChatViewers` with `variables.channelLogin`; the selected channel comparison is
ASCII case insensitive. A batched response must match its originating batch
length and index. Multiple matching operations in one batch are rejected as
ambiguous. The current page must also match the selected channel.

The observer reads no request or response headers. It restores the native
request stream position, reads at most 64 KiB plus a one-byte overflow sentinel,
and drops the request bytes before reading the response. Response reads are
bounded at 1 MiB plus the sentinel. There are at most 64 matching endpoint events
per attempt, one outstanding response-content callback, one Community UI
callback, and a five-second native shutdown flush. Over-budget or malformed
data stops that attempt. There is no request replay, private HTTP client,
integrity generation, browser automation stealth, user-agent modification,
security flag change, or imported browser session.

The normal app-owned profile is `data/browser-profile` next to the executable.
The WebView2 engine manages its normal persistent login/session storage. The
application never reads or exports cookies, tokens, or other browser profiles.
It preserves this profile on close. Environment variables that override the
runtime, profile, release channel, or browser arguments cause a safe startup
failure; the proof does not rewrite them.

`aggregate-evidence.json`, also next to the executable, is replaced for each
run and after each state change. Only phase/status codes, the selected public
channel, attempt/sample counts, HTTP status, classified source errors, reported
chatters count, category lengths, unique-account counts, pairwise overlap, union
counts, and cleanup state are stored. Source account sets exist only in RAM;
raw batches, error text, page DOM, headers, credentials, and cookies are never
written by the application. If aggregate evidence cannot be written, sampling
is disabled. Archive a previous run's aggregate file separately if needed.

The reported count and captured account sets describe the native chat-community
source. They do not establish all video viewers, watch time, uniform sampling,
independent rounds, exhaustive coverage, or any percentage guarantee. The older
`ChatViewers` shape is supported by synthetic tests; only live results can
validate whichever operation Twitch actually uses in this host.

## Documentation checked

- [webview2-rs APIs and sample host](https://github.com/wravery/webview2-rs)
  (`webview2-com` pinned to 0.39.1, `windows` pinned to 0.62.2).
- [Microsoft: observing requests and responses](https://learn.microsoft.com/en-us/microsoft-edge/webview2/how-to/webresourcerequested#monitoring-the-requests-and-responses-via-the-webresourceresponsereceived-event).
- [Microsoft: WebView2 user data folders](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/user-data-folder).
- [Rust Windows IStream methods](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/System/Com/struct.IStream.html).
