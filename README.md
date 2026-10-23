# Veylurk

Veylurk is being developed as a lightweight Windows application in Rust for observing Twitch chat presence. The intended application will retain evidence in SQLite and provide an English localhost interface for searching by channel, broadcast, or observed account.

The current implementation is a **bounded, browser-free IRC prototype** named `veylurk-irc-probe`. It uses one anonymous secure WebSocket connection for up to three channels. It needs no browser, extension, Twitch login, or user token. It does not yet provide continuous monitoring, stored history, broadcast identification, or a user interface.

## Current evidence

A separate, bounded .NET protocol experiment established that anonymous IRC could return nonself account observations on two tested third-party channels. One room initially returned only the probe's guest account, then delivered nonself JOIN messages. That sequence is why an initial zero count is treated as incomplete evidence rather than an empty room. This protocol experiment is separate from validation of the Rust executable.

The earlier HTTP and browser CommunityTab experiments encountered `IntegrityCheckFailed`. The IRC prototype uses a different Twitch interface and does not request, obtain, or replay an integrity token.

Twitch documents that IRC messages can be duplicated or reordered. Its membership documentation also states that JOIN/PART messages are unavailable when a chat room contains more than 1,000 users. That threshold concerns chat membership, not the video's displayed audience. These limitations prevent a completeness guarantee. See [Twitch IRC concepts](https://dev.twitch.tv/docs/chat/irc/#parsing-messages) and the [PART reference](https://dev.twitch.tv/docs/chat/irc/#part-command).

## Running the IRC prototype

```text
veylurk-irc-probe --channel examplechannel --window 30 --timeout 60
veylurk-irc-probe --channel firstchannel --channel secondchannel --window 30 --timeout 60
```

The probe requests the membership capability, registers a generated `justinfan` guest using the public guest convention, joins the requested rooms, and handles both IRC and WebSocket keepalives. It never sends a chat message. Anonymous guest access is an experimentally verified convention, not a guaranteed Twitch API contract.

Each outgoing IRC command uses its own WebSocket text message. Registration sends CAP, PASS, then NICK separately; combining those commands in one message caused Twitch to return CAP NAK during a bounded protocol check. Incoming server messages can still contain multiple IRC lines. The probe distinguishes an explicit CAP NAK from an unexpected capability response using fixed error text without logging the server payload.

`--window` accepts 0–300 seconds and begins once every requested channel has supplied its first NAMES terminator. `--timeout` accepts 15–600 seconds and bounds the whole run, including shutdown. Up to four seconds of that budget are reserved for attempting PART and WebSocket Close. The budget must leave enough time for connection, registration, initial terminators, the requested window, and shutdown. A zero-second window requests only the observations available when all initial terminators have arrived.

The probe prints one aggregate JSON line per channel only after the requested observation completes successfully. Account names stay in memory and are never printed or persisted. Its output fields mean:

| Field | Meaning |
| --- | --- |
| `initial_observed_accounts` | Unique nonself accounts observed by the first NAMES terminator for this channel. Later NAMES messages can add observations. |
| `observed_accounts_at_end` | Size of the in-memory observed set after applying the membership messages received during this run. This is not a verified current population. |
| `nonself_join_events` / `nonself_part_events` | Received nonself JOIN/PART message counts across bootstrap and the observation window. These can include duplicates and delayed roster delivery, so they are not counts of unique arrivals or departures. |
| `names_terminator_received` | A protocol terminator arrived. This establishes neither complete membership nor an empty room. |
| `membership_window_seconds` | Requested observation duration after every channel's first terminator. |
| `evidence_scope` | The limitations that apply to every observation. |

Only the probe's own guest nickname is excluded. Other guest accounts are observations too. Valid duplicate welcome, NAMES, and terminator messages are tolerated; duplicate terminators do not restart the window. A PART prevents a delayed NAMES response from restoring that account unless a subsequent JOIN is received. The server's ordering and delivery limitations still apply.

Channel identities, server prefixes, account prefixes, line framing, UTF-8, and capability acknowledgement are checked. The experiment caps each room's observed accounts and remembered departures at 10,000 entries each, IRC lines at 64 KiB, and buffered data and WebSocket messages at 128 KiB. Connection, registration, initial terminators, and individual writes have additional time limits. Rejections, reconnect requests, malformed data, exceeded bounds, a self PART, or a disconnect during collection fail the run without emitting a successful observation. There is no automatic reconnect or retry loop.

## Build and offline checks

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked --bin veylurk-irc-probe
```

The IRC executable uses Rust and the Windows trust store through rustls. It has no browser runtime or Node.js dependency. Unit fixtures cover fragmented input, identity validation, duplicate and reordered messages, delayed membership, per-channel isolation, account bounds, and exclusion of only the probe's own nickname. Local WebSocket fixtures require exact separate registration messages and cover acknowledgement ordering, rejection before JOIN, protocol keepalives, observation failure, deadlines, and shutdown without contacting Twitch.

The Windows workflow checks formatting, lints, runs offline tests, builds the executables, and publishes `veylurk-irc-probe-windows-x64` as a separate artifact. Its IRC smoke check uses `--help` only. Successful build and offline tests are distinct from a completed live run and from acceptance of the eventual application.

## Earlier experiments

`veylurk-probe` and `veylurk-browser-probe` remain in the repository as earlier transport experiments. They are not needed to run the IRC executable. The browser experiment uses the separate `browser-helper` package and its pinned Playwright/Chromium runtime; its historical bundle remains in the Windows workflow. No browser companion or extension is part of the IRC prototype.

Veylurk can describe an account only as **observed through Twitch chat membership**. It cannot establish video watch time, exact arrival or departure, anonymous video viewers, complete coverage, or current presence from silence.

Released under the [Unlicense](LICENSE).
