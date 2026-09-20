# Veylurk

Veylurk is planned as a lightweight local Rust service for Twitch chat-presence observations. The intended product will target Windows, retain evidence in SQLite, and provide an English localhost interface for searching by channel, broadcast, or observed account.

This repository currently contains the Stage 2 direct transport probe and the **Stage 3 browser-assisted proof of concept**. Neither binary is the Veylurk application; they do not store history or provide a web interface.

## Current finding

Twitch's normal logged-out web interface exposed changing samples of chat-connected accounts for a tested third-party live channel. A standalone anonymous HTTP request using Twitch's public web client identifier reached Twitch, but Twitch returned `IntegrityCheckFailed` for `channel.chatters` and requested an integrity challenge. The probe stops there. It does not copy browser credentials, obtain an integrity token, solve a challenge, or claim that sampled accounts are video viewers.

This leaves the browser UI path feasible for experimentation while the preferred lightweight standalone transport is currently blocked. It does not establish a safe collection cadence or a complete population model.

The Stage 3 probe launches installed Microsoft Edge or Google Chrome with a new temporary profile, creates up to three ordinary popout-chat tabs, reopens the public viewer panel for each bounded sample, and reads rendered DOM rows. Browser automation is limited to loopback. It does not use the user's browser profile, inspect browser network requests, copy cookies or tokens, solve challenges, or claim that panel samples are independent. Any explicit challenge stops the run.

## Probe behavior

The CLI is constrained to 1–20 samples, an interval of at least 15 seconds, at most 10 distinct sequential channels, a global time budget, a 15-second request cap, and a 2 MiB response cap. It retries only transport failures, at most twice, and stops on authorization, rate limits, integrity challenges, channel mismatch, and schema failures.

It prints aggregate JSON Lines only: counts, overlap, union size, latency, and a one-way fingerprint. Account names remain in memory only long enough to calculate those metrics and are never printed or persisted. A nonzero exit means the requested experiment did not complete.

```text
veylurk-probe --channel ouaiseddy --samples 1 --timeout 30
```

Use `veylurk-probe --help` for all options. Absence of stream metadata is treated as unknown, not proof that a channel is offline. The persisted query and undocumented schema can change without notice.

```text
veylurk-browser-probe --channel parolesdhonneur_ --samples 3 --interval 15 --timeout 120
```

The browser probe prints aggregate counts, overlap, cumulative discovery, and one-way fingerprints. Usernames stay in memory. Its three-tab cap is an experiment boundary and provides no evidence for dozens-channel scale. The preflight runtime budget is checked between operations; an in-flight browser protocol operation has a separate 25-second I/O cap.

## Build and test

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

GitHub Actions runs these checks on Windows and preserves the Stage 2 artifact while also uploading `veylurk-browser-probe.exe` as `veylurk-browser-probe-windows-x64`. `scripts/measure-browser-probe.ps1` samples the Rust and browser process tree, reports simultaneous memory peaks and aggregate CPU, and checks observed process identities for survivors after exit. Polling can miss short-lived processes.

Veylurk can only describe an account as **observed in a Twitch chat-connected sample**. It cannot establish video watch time, exact arrival or departure, anonymous viewers, or complete coverage.

Released under the [Unlicense](LICENSE).
