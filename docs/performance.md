# Capacity and extended operation evidence

Veylurk accepts up to 50 configured channels. This is a configuration bound, not a promise of complete chat membership, constant resource use, or useful data from every channel. History has no automatic expiry. Database size grows with received observations, session changes and broadcast metadata. Relationship summaries are persisted aggregates; raw history remains independently searchable with bounded pages.

## Repeatable offline workload

The Windows workflow runs the ignored Rust `scale_workload` tests explicitly in addition to ordinary tests:

```text
cargo test --locked scale_workload -- --ignored --nocapture --test-threads=1
```

The cases run serially to avoid competing database workloads distorting the measured I/O timings. Each retains 2,200 observations per room, including 110,000 at 50 rooms, a 120-second elapsed-time guard, a 128 MiB controlled WAL budget and a checked final truncating checkpoint. Mixed-room transactions contain at most 512 events, matching the collector. Output reports ingestion, SQL execution, durable commit/checkpoint, query, reopen and maximum append-stall wall times; these are not CPU-utilization measurements or a hard filesystem deadline. One native Windows CI run measured 62.86 seconds of ingestion (10.34 seconds of SQL work and 52.27 seconds of commit/checkpoint work), with a 12.64-second maximum append stall. Lossless stall acceptance instead requires a separate collector fixture with 50 rooms, 4,096 receipts initially outstanding, and 26,000 additional receipts paced at 2,000 per second during a 13-second storage hold. That fixture must demonstrate a direct PONG within one second near the end of the hold, exact durable receipt counts and per-room order, no reconnect, and a second successful burst after draining. This defines a finite buffered burst, not sustainable throughput of 2,000 receipts per second or a universal no-loss guarantee; separate overflow tests must verify explicit coverage gaps when the buffer is exhausted.

Storage retains `synchronous=FULL` and one serialized connection. Automatic checkpoints target 8192 WAL frames, approximately 32 MiB with the default 4096-byte pages; recycled file allocation targets 64 MiB. These are checkpoint and reuse targets, not a hard active-WAL cap. A transaction can overshoot the target, and an external database reader can delay checkpoint completion. The SQLite instruction progress handler cannot interrupt a blocked filesystem flush. Slow storage can still produce collection gaps; keep those gaps explicit and retain the WAL if a checkpoint fails.

Only query jobs have the five-second SQL execution budget. Durable mutations, including append transactions and checkpoints, have no elapsed-time cancellation callback; their logical batch and transaction limits remain in place. Slow storage can delay both a durable job and reads queued behind it. The receive buffer remains bounded, with explicit coverage gaps on overflow. This removes false write cancellations without promising a hard filesystem deadline or sustained lossless ingestion. See [fault evidence and policy](diagnostics.md).

The workload uses synthetic channel counts of 10, 25 and 50 and timestamped observation histories. It exercises storage, history search, relationship queries, pagination and restart retention. Its test output reports the actual generated record counts and timings. It uses temporary databases without Twitch connections or credentials. CI success checks correctness under the fixture's chosen load; elapsed times from shared GitHub runners are diagnostic measurements, not a stable performance benchmark or service-level guarantee.

Run the same command on the intended Windows machine to measure its CPU and storage configuration. Record the revision, Windows version, CPU, RAM, storage type, generated row counts, database/WAL sizes and measured insertion/query/reopen times with the output. Keep the fixture's synthetic timestamps distinct from elapsed collection time: a history spanning months of timestamps is not months of observed operation. Fixtures do not simulate Twitch connection limits, room membership availability, network interruptions or machine sleep.

## Existing live evidence

The earlier two-channel Windows trial consisted of two 120-second runs. It retained 691 observations on the first run, loaded those records after restart, and reached 2,083. Searches in both directions and pause/resume were checked; peak working set was approximately 18.1 MB. These measurements apply only to that short trial and environment.

## Pending live scale acceptance

A real extended trial at 10, 25 and 50 channels is still pending. It requires approved Internet access for the exact executable and a user-selected channel set. Do not infer a successful 24-hour run from synthetic data, CI, or the earlier two-channel trial.

For each selected scale, run for at least 24 hours if that is the intended unattended workload. Record wall-clock duration, enabled and available channel counts, receive throughput, reconnects and gaps, database/WAL growth, process working set/CPU and local query latency. Include quiet and busy periods, pause/resume, a controlled network outage, restart continuity, login startup under the actual Windows account, and backup/restore into a fresh package. Recheck optional metadata without including credentials or tokens in reports. Stop expansion if resource use, storage warnings or failed writes threaten history preservation.

Windows sign-out, sleep, network loss, unavailable Twitch membership and missing messages are coverage gaps. Report them as unknown periods. An uninterrupted process alone does not establish uninterrupted or complete observations. Keep live measurement notes separate from offline fixture results and human interface acceptance.
