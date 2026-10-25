# Bounded runtime measurement

Run `Measure-Veylurk.ps1` from an existing Windows PowerShell 5.1 session against an already running extracted package. The command only makes local GET requests and reads the identified process counters. It never launches or stops Veylurk, installs a startup task, contacts Twitch, or opens its database or credentials.

```powershell
.\Measure-Veylurk.ps1 -PackageDirectory 'C:\Apps\Veylurk' -Port 8794 -DurationSeconds 3600 -IntervalSeconds 15 -OutputDirectory 'C:\Reports\veylurk-trial-01'
```

The report parent must already exist. The destination must be fresh, outside the application package, and free of junctions, symbolic links, or other reparse points in its ancestry. Existing reports are never overwritten. Keep the script beside `Veylurk-Operations.ps1` and `Veylurk-Launcher.ps1`.

Before creating a report, the tool requires `/api/runtime` to identify the exact selected executable, database, port, process, and loopback listener. It retains the original process handle and process start time. Every subsequent status read is bracketed by runtime checks matching the original `instance_id`, PID and `started_at`, executable and database. An unavailable or changed identity yields an explicit sample without aggregate or process readings. A replacement never becomes the measured target. Initial identity failure rejects the trial. After successful initialization, API failures continue as explicit samples until the finite trial ends.

`samples.jsonl` contains UTC timestamps, actual monotonic elapsed and request durations, gaps between sample starts, aggregate observation/session counts and their deltas, enabled/connected channel counts, allowlisted collector/storage states, storage and available disk bytes, working set and private bytes. CPU reports the process CPU delta divided by the actual monotonic elapsed time between process snapshots. `one_core_percent` uses one logical core as 100% and can exceed 100%; `machine_percent` additionally divides by the logical processor count in the summary. These are sampled process values, not system-wide measurements. The first reading, negative/reset counters, missing metrics, failed requests, and identity changes have explicit result codes; missing numbers remain null. CPU and aggregate baselines reset across identity failures. Aggregate counters reset across missing status samples; valid original-process CPU readings can continue when status alone fails.

`summary.json` records the requested and actual duration, sample outcomes, sample-start gap maximum, sampled memory peaks, process identity, limits and termination reason. It is written on normal finite completion. An interrupted PowerShell session or filesystem failure can leave partial JSONL without a summary. These files omit paths, accounts, channel names, raw history, response bodies, arbitrary error text, credentials and URLs. Only explicitly selected numeric values and allowlisted states are persisted. No metadata requests are necessary for this measurement.

Duration is 1–86,400 seconds; interval is at least 2 seconds. Sampling starts immediately after identity validation and does not catch up missed intervals. The hard ceilings are 43,200 samples, 8 KiB per sample, 128 MiB for both report files together (including 32 KiB reserved for the summary), and 256 KiB per API response. Each local request has a maximum two-second timeout, further reduced to the remaining trial time. Redirects and proxies are disabled. A slow response may therefore leave no remaining time for later requests in that sample. No new request or sleep is scheduled beyond the duration; local validation, serialization and filesystem finalization add small overhead. A size or count ceiling ends sampling with an explicit reason; an unpersisted size-limited sample is counted in the summary. The script does not impose a limit on the application's storage growth.

Tool sampling gaps describe the measurement tool, not app collection coverage. Even successful periodic samples cannot prove uninterrupted collection, IRC membership, account presence, visits, viewing, or broadcast coverage between observations. Use the application's evidence and gap model separately. A growing observation counter shows retained chat observations; it does not show video watching.

Run the invented fixture suite without accessing a running application:

```powershell
powershell.exe -NoProfile -File .\scripts\Test-VeylurkMeasurement.ps1
```

The suite mocks all network/process observations and uses temporary sentinel files. It checks exact ownership, finite failure/replacement behavior, CPU denominators, counter resets, redaction, fresh destinations, junction rejection and report bounds. It does not constitute a live unattended soak or a performance threshold result.
