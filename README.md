# Veylurk

Veylurk is a Rust application for Windows that collects observable Twitch chat membership into a local SQLite database. Its embedded English interface searches accounts, channels, connection history, and optional broadcast metadata at `http://127.0.0.1:8794`.

The application runs continuously while its process is open. Collection needs no browser, extension, Twitch login, or user token. You can open the local interface in an ordinary browser and close that page without stopping collection. This release is a console application, not an installed Windows Service; service installation and automatic startup are not implemented.

## Run the application

Download the `veylurk-windows-x64` artifact from a successful Windows workflow run and extract `veylurk.exe`. It contains the interface and needs no Node.js or browser runtime for collection.

```powershell
.\veylurk.exe --database .\veylurk-data\observations.sqlite3 --port 8794 --channel examplechannel
```

Open `http://127.0.0.1:8794`. Add channels in the interface, pause or resume collection for a channel, and search exact Twitch account or channel logins. Up to 50 channels can be tracked. Repeat `--channel` to seed several channels; otherwise saved configuration is reused. Choose a stable database location: a relative path is resolved from the directory in which you start the application. Press Ctrl+C to stop gracefully. For a bounded acceptance run, `--run-for-seconds` accepts 1–3600 seconds and requests the same graceful shutdown; omit it for continuous operation.

The HTTP listener binds only to `127.0.0.1`. Changes require requests from the local application's own origin. There is no remote-access mode, account system, or CSV export. The browser receives embedded local assets without third-party scripts or a CDN.

One chat connection serves all enabled channels, with paced JOIN requests, keepalives, bounded queues, and reconnect backoff. A room that fails its initial membership deadline is marked unavailable while healthy rooms continue. It is retried on the next connection; pausing then resuming it requests a configuration reconnect. Configuration changes restart the connection and create a new recorded session. A storage failure stops collection and is shown in status; a retry cannot recover observations that were never saved. A second instance cannot open the same database while the first holds its lock.

## What the history means

The database retains received NAMES, JOIN, PART, and NAMES-end observations, their receive times, channels, and connection session IDs. It does not save chat message bodies. Only the application's own generated guest nickname is excluded; other guest nicknames remain observations.

Account search shows channels where that login was observed. Channel search shows the corresponding accounts and events. Relationship summaries give first and last received observations and message counts, not continuous presence or watch duration. A Twitch login is not a permanent account ID: renames cannot be reliably linked using IRC nicknames alone.

Connection history records attempts, endings, and failures separately from account evidence. Time between sessions and periods without reliable delivery remain unknown. A process interruption is recorded on the next database open; the recovery time is not the exact instant the earlier process stopped.

History has no automatic expiry. Pausing or removing a channel from tracking retains its searchable history. The database and its SQLite write-ahead log consume increasing disk space. For a simple backup, stop Veylurk gracefully before copying its data directory; copying only the main database while it is running can omit recent records.

Twitch documents duplicate and reordered IRC messages, and says JOIN/PART membership messages are unavailable when a chat room contains more than 1,000 users. That threshold describes chat membership, not the video's displayed audience. Delayed JOIN messages can be initial roster delivery. A NAMES terminator, an initial zero count, or silence establishes neither completeness nor absence. Anonymous guest access is an experimentally verified public convention, not a guaranteed Twitch API contract. See [Twitch IRC concepts](https://dev.twitch.tv/docs/chat/irc/#parsing-messages) and the [PART reference](https://dev.twitch.tv/docs/chat/irc/#part-command).

Veylurk can describe an account only as **observed through Twitch chat membership**. It cannot establish video viewing, watch time, exact arrival or departure, anonymous video viewers, or complete channel coverage. The 50-channel configuration limit is not a guarantee that Twitch will provide useful observations for every channel.

## Optional broadcast metadata

Chat history works without Twitch application credentials. Live status and broadcast identity remain **Unknown** until the optional official metadata worker can resolve and check a channel. Failed, incomplete, or stale checks also remain unknown. An unresolved channel login is not classified as offline.

To enable this feature, register your own confidential application in the [Twitch developer console](https://dev.twitch.tv/docs/authentication/register-app/) and supply that application's client ID and secret to the Veylurk process. Twitch's registration requirements include a verified account and two-factor authentication. Veylurk does not register an application for you, include a shared client secret, or use credentials from Twitch's website.

The current setup uses process environment variables; there is no credentials form or persistent secret vault. For an interactive PowerShell launch, enter the secret at a masked prompt rather than including it literally in command history:

```powershell
$env:VEYLURK_TWITCH_CLIENT_ID = Read-Host 'Your Twitch application client ID'
$veylurkSecret = Read-Host 'Your Twitch application client secret' -AsSecureString
$env:VEYLURK_TWITCH_CLIENT_SECRET = [System.Net.NetworkCredential]::new('', $veylurkSecret).Password
try {
    .\veylurk.exe --database .\veylurk-data\observations.sqlite3 --port 8794
} finally {
    Remove-Item Env:\VEYLURK_TWITCH_CLIENT_SECRET -ErrorAction SilentlyContinue
    Remove-Item Env:\VEYLURK_TWITCH_CLIENT_ID -ErrorAction SilentlyContinue
    $veylurkSecret = $null
}
```

The account that launches Veylurk must have the variables in its own process environment. Do not place secrets in the repository, screenshots, logs, or support messages. Client secrets and access tokens are kept in process memory and are not saved in the history database or returned to the interface.

The worker uses the [client credentials grant](https://dev.twitch.tv/docs/authentication/getting-tokens-oauth/#client-credentials-grant-flow), validates new tokens and active tokens hourly, and obtains a new app token when needed. App tokens do not have refresh tokens. It checks enabled channels in filtered batches at approximately 60-second intervals, independently from chat collection, and respects rate-limit waits. See [token validation](https://dev.twitch.tv/docs/authentication/validate-tokens/), [Get Streams](https://dev.twitch.tv/docs/api/reference/#get-streams), and [rate limits](https://dev.twitch.tv/docs/api/guide/#twitch-rate-limits).

Broadcast records retain Twitch's stream ID, title changes, reported start, and successful check times. Account-to-broadcast results associate received chat evidence with the same channel between a reported broadcast start and the last confirmed live check. This is a time association, not proof the account watched that broadcast. Unknown periods are not assigned invented broadcasts, and collection gaps remain unknown.

## Build and offline checks

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --test web/app.test.cjs
cargo build --release --locked --bin veylurk
```

Node.js is used only for the interface's development tests. The Windows workflow runs these checks, builds release executables, and checks application startup, local APIs, embedded assets, and timed graceful shutdown using an empty temporary database without channels or metadata credentials. It also checks application and IRC-probe help, then uploads the standalone application and bounded probe artifacts. HTTP and WebSocket fixtures use local test servers; they do not need Twitch credentials. Build success and offline tests are distinct from live operation and human acceptance.

A prior Rust IRC probe completed a live two-channel, 30-second run successfully, observing nonself accounts in both rooms with roughly 14.4 MB peak working set in that run. This validates the bounded transport in that environment; it is not a performance result for the continuous application, long histories, or dozens of channels.

## Bounded and historical probes

`veylurk-irc-probe` remains available separately for a bounded aggregate-only experiment on up to three channels:

```text
veylurk-irc-probe --channel firstchannel --channel secondchannel --window 30 --timeout 60
```

It keeps account names only in memory and prints per-channel aggregate JSON after a successful run. `initial_observed_accounts` describes the set at the first NAMES terminator; `observed_accounts_at_end` describes the received-event set at the end, not a verified current population. JOIN/PART counts count received messages, including possible duplicates and delayed roster delivery. The requested window starts once all initial terminators arrive. `--window` accepts 0–300 seconds and `--timeout` 15–600 seconds, including bounded teardown. The probe does not reconnect or save history.

Registration sends CAP, PASS, and NICK as separate WebSocket text messages. Twitch rejected the combined registration form in a controlled check; incoming server messages may still contain several IRC lines. Offline fixtures assert this framing, acknowledgement ordering, keepalives, identity checks, reordering, limits, failures, and teardown.

`veylurk-probe` and `veylurk-browser-probe` preserve earlier HTTP and browser experiments. Their CommunityTab route encountered `IntegrityCheckFailed`; the current application does not use that route or obtain integrity tokens. The historical browser bundle can be built explicitly with the Windows workflow's `legacy_browser` manual option. Its Node.js, Playwright, and Chromium downloads are excluded from the normal application workflow. No browser companion or extension is required by Veylurk.

Veylurk's own source is released under the [Unlicense](LICENSE). Third-party dependencies retain their respective licenses.
