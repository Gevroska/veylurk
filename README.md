# Veylurk

Veylurk is planned as a lightweight local Rust service for Twitch chat-presence observations. The intended product will target Windows, retain evidence in SQLite, and provide an English localhost interface for searching by channel, broadcast, or observed account.

This repository currently contains only the **Stage 2 transport proof of concept**. It is not the Veylurk application and does not store history or provide a web interface.

## Current finding

Twitch's normal logged-out web interface exposed changing samples of chat-connected accounts for a tested third-party live channel. A standalone anonymous HTTP request using Twitch's public web client identifier reached Twitch, but Twitch returned `IntegrityCheckFailed` for `channel.chatters` and requested an integrity challenge. The probe stops there. It does not copy browser credentials, obtain an integrity token, solve a challenge, or claim that sampled accounts are video viewers.

This leaves the browser UI path feasible for experimentation while the preferred lightweight standalone transport is currently blocked. It does not establish a safe collection cadence or a complete population model.

## Probe behavior

The CLI is constrained to 1–20 samples, an interval of at least 15 seconds, at most 10 distinct sequential channels, a global time budget, a 15-second request cap, and a 2 MiB response cap. It retries only transport failures, at most twice, and stops on authorization, rate limits, integrity challenges, channel mismatch, and schema failures.

It prints aggregate JSON Lines only: counts, overlap, union size, latency, and a one-way fingerprint. Account names remain in memory only long enough to calculate those metrics and are never printed or persisted. A nonzero exit means the requested experiment did not complete.

```text
veylurk-probe --channel ouaiseddy --samples 1 --timeout 30
```

Use `veylurk-probe --help` for all options. Absence of stream metadata is treated as unknown, not proof that a channel is offline. The persisted query and undocumented schema can change without notice.

## Build and test

```text
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

GitHub Actions runs these checks on Windows and uploads `veylurk-probe.exe` as `veylurk-probe-windows-x64`. `scripts/measure-probe.ps1` reports exit code, wall time, CPU time, and peak working set for one bounded run.

Veylurk can only describe an account as **observed in a Twitch chat-connected sample**. It cannot establish video watch time, exact arrival or departure, anonymous viewers, or complete coverage.

Released under the [Unlicense](LICENSE).
