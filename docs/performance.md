# Short performance benchmarks

The owner requests short, intensive measurements with many channels, followed by optimization of the largest measured costs. Long reliability trials are owner-managed and are not a delivery gate for this performance work.

## Local measurement harness

The `veylurk-perf-bench-windows-x64` Windows artifact contains an offline executable. It opens no network connection, reads no Twitch credentials, and refuses every existing output directory. Its parent directory must already exist.

```powershell
.\veylurk-perf-bench.exe --output-dir R:\Benchmarks\veylurk-baseline-50-1 --channels 50 --events-per-channel 2200 --batch-size 512 --query-repetitions 3 --max-seconds 120
```

Run 10 and 25 channels once, then 50 channels three times for baseline and candidate. Use fresh directories on the same volume, the same release toolchain, arguments and machine workload. Run cases sequentially. Store `report.json` with the executable checksum, revision and hardware information. Compare medians and observed ranges rather than selecting the fastest run.

The fixed mixed-event workload includes repeated observations, departures, returns, ambiguous roster entries and backwards timestamps. A hot account accumulates many visits, exercising retained-history and timeline queries. The report separates setup, data generation, SQL, commits including automatic checkpoints, session closure, queries, correctness verification and the final checkpoint. It reports append quantiles, per-query timings, CPU time, process memory and database/WAL sizes. Automatic-checkpoint time is included in commit time and is not independently identified.

Every run checks exact raw/derived/aggregate counts, SQLite integrity, foreign keys, FULL durability and a stable logical fingerprint. The fingerprint compares evidence independently of SQLite file layout, global IDs and nondeterministic housekeeping timestamps. An optimization must preserve these logical results. The elapsed-time guard is checked between operations; it cannot impose a hard deadline on an in-flight durable filesystem flush.

The initial baseline closes each room session separately. The candidate closes the same sessions, cutoffs and reasons in one bounded atomic transaction, matching terminal collector cleanup. Its report identifies this strategy. Compare session-close wall time separately from ingestion and the final checkpoint; matching evidence fingerprints and query digests are required. A failed batch rolls back all its closures, leaving committed observations intact for the existing uncertain restart recovery.

Schema initialization groups core creation and legacy import into one transaction, and history schema creation into another. Version checks precede schema writes; history recomputation retains its separate transaction. This reduces durable commits on fresh databases without weakening `synchronous=FULL`. Existing histories receive a nonunique partial covering index on `(end_observation_id, session_id, channel, account)` for non-null departure links. Timeline queries use it to exclude attached PART receipts by direct indexed lookup rather than repeatedly scanning an account's visits. Query semantics and model version are unchanged.

Use `--mixed-reads` to issue one bounded hot-account history request at a time during ingestion. Its latency includes queueing, execution and delivery. This mode disables per-flush filesystem sampling and progress logging so those observers cannot create artificial idle windows for the reader. It samples file sizes only at phase boundaries; sampled maxima are not continuous WAL peaks. Write-only reports attribute filesystem sampling separately. Compare SQL and durable commit measurements directly; gross ingestion wall time also includes benchmark observers and must not be advertised as production throughput.

Failed after-load queries and verification retain a failed report with measured samples, failure phase and SQLite error details. A failed case is diagnostic evidence, never an accepted performance result. The production five-second SQL query budget, bounded receive buffer, 512-event append cap and durable write policy remain unchanged.

For a native index-only comparison, run the same harness with `--without-timeline-index` in another fresh directory. Only that synthetic database loses the new index, before fixture ingestion. Its separately reported variant setup cost is excluded from append and query timings. Both variants retain the startup and batched session-close improvements. Compare full logical fingerprints, matching page digests, query interruption outcomes, pure SQL/commit cost and final database size. This is an index experiment, not a replay of every historical application behavior.

Short real-Twitch runs are measured separately, with an isolated database and the already authorized executable when possible. Report configured channels, actual room states and received records: 50 configured channels do not imply 50 live broadcasts or complete membership. These measurements characterize resource use and API responsiveness; deterministic offline runs provide the before/after comparison.

## Existing capacity evidence

Veylurk accepts up to 50 configured channels. This is a configuration bound, not a promise of complete chat membership, constant resource use, or useful data from every channel. History has no automatic expiry. Database size grows with received observations, session changes and broadcast metadata. Relationship summaries are persisted aggregates; raw history remains independently searchable with bounded pages.

## Repeatable offline workload

The Windows workflow runs the ignored Rust `scale_workload` tests explicitly in addition to ordinary tests:

```text
cargo test --locked scale_workload -- --ignored --nocapture --test-threads=1
```

The cases run serially to avoid competing database workloads distorting the measured I/O timings. Each retains 2,200 observations per room, including 110,000 at 50 rooms, a 120-second elapsed-time guard, a 128 MiB controlled WAL budget and a checked final truncating checkpoint. Mixed-room transactions contain at most 512 events, matching the collector. Output reports ingestion, SQL execution, durable commit/checkpoint, query, reopen and maximum append-stall wall times; these are not CPU-utilization measurements or a hard filesystem deadline. One native Windows CI run measured 62.86 seconds of ingestion (10.34 seconds of SQL work and 52.27 seconds of commit/checkpoint work), with a 12.64-second maximum append stall. Lossless stall acceptance instead requires a separate collector fixture with 50 rooms, 4,096 receipts initially outstanding, and 26,000 additional receipts paced at 2,000 per second during a 13-second storage hold. That fixture must demonstrate a direct PONG within one second near the end of the hold, exact durable receipt counts and per-room order, no reconnect, and a second successful burst after draining. This defines a finite buffered burst, not sustainable throughput of 2,000 receipts per second or a universal no-loss guarantee; separate overflow tests must verify explicit coverage gaps when the buffer is exhausted.

Storage retains `synchronous=FULL` and one serialized connection. Automatic checkpoints target 8192 WAL frames, approximately 32 MiB with the default 4096-byte pages; recycled file allocation targets 64 MiB. These are checkpoint and reuse targets, not a hard active-WAL cap. A transaction can overshoot the target, and an external database reader can delay checkpoint completion. The SQLite instruction progress handler cannot interrupt a blocked filesystem flush. Slow storage can still produce collection gaps; keep those gaps explicit and retain the WAL if a checkpoint fails.

The collector starts its 250 ms flush timer when observations enter an empty pending buffer. Later receipts preserve that deadline; reaching 512 rows still triggers immediate flushing. This prevents an overdue idle timer from forcing a tiny commit after a slow write. Receipt timestamps, order, permit ownership and durability are unchanged; scheduling and storage stalls can still delay persistence.

A Windows diagnostic recovery run before this timer correction recorded 103 successful commits: 58 full batches held 29,696 rows, while 45 smaller batches held only 207 rows. SQL execution took 1.748 seconds and commits took 28.011 seconds; the recovery deadline was missed. These test-only completion counters motivated the batching correction. They exclude queue waiting and unfinished operations, and do not identify the cause of the 5.947-second maximum commit. Paused-time regressions cover timer age and immediate full-batch readiness; the existing recovery workload and acceptance limits remain unchanged.

Only query jobs have the five-second SQL execution budget. Durable mutations, including append transactions and checkpoints, have no elapsed-time cancellation callback; their logical batch and transaction limits remain in place. Slow storage can delay both a durable job and reads queued behind it. The receive buffer remains bounded, with explicit coverage gaps on overflow. This removes false write cancellations without promising a hard filesystem deadline or sustained lossless ingestion. See [fault evidence and policy](diagnostics.md).

The workload uses synthetic channel counts of 10, 25 and 50 and timestamped observation histories. It exercises storage, history search, relationship queries, pagination and restart retention. Its test output reports the actual generated record counts and timings. It uses temporary databases without Twitch connections or credentials. CI success checks correctness under the fixture's chosen load; elapsed times from shared GitHub runners are diagnostic measurements, not a stable performance benchmark or service-level guarantee.

Run the same command on the intended Windows machine to measure its CPU and storage configuration. Record the revision, Windows version, CPU, RAM, storage type, generated row counts, database/WAL sizes and measured insertion/query/reopen times with the output. Keep the fixture's synthetic timestamps distinct from elapsed collection time: a history spanning months of timestamps is not months of observed operation. Fixtures do not simulate Twitch connection limits, room membership availability, network interruptions or machine sleep.

## Existing live evidence

The earlier two-channel Windows trial consisted of two 120-second runs. It retained 691 observations on the first run, loaded those records after restart, and reached 2,083. Searches in both directions and pause/resume were checked; peak working set was approximately 18.1 MB. These measurements apply only to that short trial and environment.

## Owner-managed reliability checks

A real extended trial at 10, 25 and 50 channels is outside the requested optimization work. The owner will perform any desired long reliability testing. Do not infer a successful 24-hour run from synthetic data, CI, or a short live trial.

For any owner-run reliability trial, record duration, enabled and available channel counts, receive throughput, reconnects and gaps, database/WAL growth, process working set/CPU and local query latency. Keep credentials and tokens out of reports. Reliability results remain separate from short performance measurements.

Windows sign-out, sleep, network loss, unavailable Twitch membership and missing messages are coverage gaps. Report them as unknown periods. An uninterrupted process alone does not establish uninterrupted or complete observations. Keep live measurement notes separate from offline fixture results and human interface acceptance.
