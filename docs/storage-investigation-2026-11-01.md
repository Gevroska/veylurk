# Storage investigation and responsiveness evidence

Date: 2026-11-01. Candidate: `4ea5d9e36ea704a72cf8bab5136bf7bc76638d75`.

The candidate adds an independent storage heartbeat, bounded read-response waiting and safe skipping of abandoned queued reads. Automated checks and an isolated endpoint check verify those changes. The cause of the earlier R: delays remains unresolved; the later five-minute observation did not reproduce the reported minutes-long stall.

## Evidence and time boundaries

| Evidence | Scope and result |
| --- | --- |
| Historical incident, approximately 13:35–13:46 UTC | Durable application work, independent report writes and an offline history read were delayed. These are coordinator-verified incident observations, separate from the later measurement below. |
| Five-minute read-only observation | Original staged artifacts: `C:\Users\Geoffrey\AppData\Local\Temp\veylurk-completion-20261101-1540\storage-observation\storage-observation.json` and `storage-samples.jsonl`. The summary records 300 requested seconds, 300.0087712 elapsed seconds, 30 samples, read-only operation and no production access. |
| Observation sample timestamps | First sample: `2026-11-01T15:14:11.8560966Z`; last sample: `2026-11-01T15:19:02.1531671Z`. The final sample started about 290.3 seconds into the finite observation; sample timestamps do not imply continuous measurement between polls. |
| Windows application CI | Workflow run `41321346204` passed at candidate `4ea5d9e`, including the exact fifty-room collector fixture with two thirteen-second actor stalls and the existing 10/25/50-channel workloads. This is synthetic storage/collector evidence, separate from live Community breadth or a physical storage diagnosis. |
| Isolated candidate check | The coordinator verified test process 21500 on port 8796, Community schema version 3 and successful `/api/storage/activity` responses taking approximately 1–2 ms. These are point-in-time checks after the candidate rollout, not measurements from the earlier incident or a continuous latency guarantee. |

During the historical investigation, only the identified read-only Python inspection was cancelled. The application was not terminated as that recovery action. No Windows disk, Storage Spaces, Syncthing or production setting was changed for this investigation.

## What the five-minute observation measured

The JSONL contains 29 `ok` samples and one `unavailable` sample. The first sample was unavailable and its native provider probe took 2.3458761 seconds. Missing values remain missing; they are not zero activity.

| Metric | Observed maximum | Interpretation limit |
| --- | ---: | --- |
| R: reported queue | 11 | A sampled queue value, not an application queue or continuous queue history. |
| R: reported read rate | 11.899 MiB/s | Volume counter rate; it does not identify the issuing process or folder. |
| R: reported write rate | 49.896 MiB/s | Volume counter rate; it does not establish application commit latency. |
| Syncthing process transfer read rate | 3.153 MiB/s | Process transfer counters include cached and non-disk I/O, including network transfers. They do not attribute physical R: traffic to Syncthing. |
| Native provider probe duration | 2.346 seconds | Duration of the measurement probe, including an unavailable result; not a direct file-read or durable-flush latency. |
| Provider disk-time counter | 1,420 | This counter can exceed 100. It must not be interpreted as literal disk utilization. |

Rates are rounded to three decimals using 1 MiB = 1,048,576 bytes. These are separate observed maxima and need not have occurred in the same sample. The probe ran synchronously with at most one outstanding OS probe; native provider I/O could exceed its requested deadline. The observation's finite duration did not make each underlying OS call preemptible. Reports were staged on C: rather than written to the investigated R: volume.

This artifact contains volume, Syncthing process-transfer and memory snapshots. It does not provide simultaneous per-job SQLite execution/commit timing or physical-device attribution. Its native probe duration must not be substituted for an independent scratch-file latency measurement. The short later interval cannot establish the absence of an intermittent fault outside its samples.

## Competing explanations and supporting boundaries

The coordinator verified that R: uses the Romeo virtual disk with a Simple layout and P: uses the Papa virtual disk with a Parity layout. Both virtual disks were healthy at inspection and shared physical device IDs `5`, `0` and `3`. This establishes a possible cross-volume contention path. A healthy status does not exclude latency, and shared devices do not identify the cause of the historical stall.

Read-only Syncthing inspection found 17 configured folders: C: 3, G: 1, P: 4, R: 8 and V: 1. R:/P: scans and cleanup overlapped the historical incident interval. No directly configured folder root covered the isolated or production application paths, but reparse-point and junction ancestry was not fully resolved. The observed overlap is correlation; neither the process transfer counters nor the folder inventory proves that Syncthing caused the delay.

| Explanation | Evidence available | What remains unresolved |
| --- | --- | --- |
| Shared volume or underlying-device delay | Historical durable jobs, independent report writes and an offline read were all affected; R: and P: share physical devices. | Simultaneous device latency, queue and operation evidence is missing for the historical interval. The responsible process, device or storage-stack condition is unknown. |
| Application queue blocked behind active durable work | One serialized SQLite worker means an old active job can hold later requests. The candidate exposes that state independently. | An old active duration includes SQL, flushes and filesystem work. Duration alone cannot separate those causes. |
| A costly application query or computation | Such work can consume actor time and delay later jobs. | No matched CPU/profile and independent volume-latency evidence isolates an application hot path in the historical incident. |
| Syncthing load on shared storage | Scans/cleanup overlapped the incident and some configured folders use R:/P:. | Physical I/O attribution, complete relevant reparse ancestry and discriminating timing evidence are missing. No Syncthing setting was changed to manufacture a comparison. |

No broader status cache, extra filesystem worker or SQL optimization was justified by this evidence. The implemented change targets bounded responsiveness and diagnosis without claiming a storage speedup.

## Candidate activity and read-wait contract

`GET /api/storage/activity` uses the existing localhost Host/security checks and returns `{process_id, port, activity}`. It reads fixed-size memory only: no actor job, SQLite query, file metadata, disk-free-space call or diagnostic-journal flush. Consumers cross-check process/port against the established `/api/runtime` instance identity. `activity.schema_version` is 1; this memory snapshot version is independent of Community database schema version 3.

All activity timestamps are monotonic milliseconds since this Store opened, rather than UTC or evidence receipt times. The snapshot retains one active job and one last completed job, with fixed operation and job-class identifiers. It contains no SQL, parameters, account/channel names, paths, arbitrary backend messages or per-job history.

| Field or group | Meaning |
| --- | --- |
| `sampled_at_ms` | Monotonic snapshot time. |
| `admission_waiting` | Call futures currently waiting for a bounded actor-channel permit. This precedes admission to the actor queue. |
| `waiting_to_start`, `oldest_queued_age_ms` | Admitted jobs not yet started and the oldest admitted wait. These include the brief synchronous send/dequeue handoff. |
| `actor_queue_capacity`, `admitted_job_capacity` | The channel remains 64 jobs. Fixed activity storage holds at most 65 admitted jobs, accounting for the worker's one dequeued job. `waiting_to_start` can briefly reach 65 during a handoff. |
| `active` | Fixed operation/class, request/admission/start stamps, admission wait, queue wait and current active age; null when idle. Active age is elapsed wall time, not CPU time or a storage-failure verdict. |
| `last_completed` | The most recent terminal operation/class, timing and outcome: `succeeded`, `failed`, `canceled_before_start` or `worker_unavailable`. A cancelled queued job has no start or execution duration. |
| `admission_wait_ms` | Count/sum/max for successful admissions; abandoned and unavailable admissions are counted separately. |
| `queue_wait_ms` | Count/sum/max for started, cancelled and worker-dropped queued jobs; excludes pending admission. |
| `execution_ms` | Count/sum/max for returned operations, including errors; excludes admission, queue waiting and response delivery. |
| `totals` | Requested, admitted, started, succeeded, failed, cancelled-before-start, abandoned-admission, admission-unavailable, worker-dropped-before-start and read-response-timeout counters. Numeric totals and duration summaries saturate at `u64::MAX`. |

`totals.failed` counts returned operation errors and an active job lost with the worker. `totals.read_response_timeouts` is separate: an expired response wait does not itself latch the first storage failure, indicate a failed device or establish a collector failure. Success/failure counters describe actor operations, not HTTP delivery outcomes; a reader can time out before its active operation later succeeds.

Read callers have a ten-second total admission/queue/execution-response wait (`read_response_budget_ms = 10000`). SQLite queries retain their separate fresh five-second execution budget after dequeue (`query_execution_budget_ms = 5000`). A timeout returns `read_wait_timeout`. Before admission, cancellation releases the waiting future/permit accounting and delivers no job. After admission, a read whose oneshot receiver is already closed is skipped before execution. Abandoned queued reads remain bounded by the existing queue until the worker reaches them; they do not create an unbounded job log or enlarge the queue.

A receiver can disappear after the dequeue check, so an already started read may still finish. Neither the response deadline nor SQLite's progress callback can preempt blocked OS I/O. Query handlers are cleared before and after executed jobs so a previous query's expired callback does not affect subsequent operations.

Durable mutations have no new elapsed-time cancellation and are never skipped because their caller disconnected. They retain SQLite's actual result and existing busy timeout. The single connection, `synchronous=FULL`, transaction/batch bounds, collector receive buffer, explicit overflow gaps and no-partial-write-retry policy remain unchanged. The fifty-room fixture's injected hold uses durable write policy because it models nonpreemptible storage work; its thirteen-second holds, PONG deadlines, receive-budget checks and integrity assertions remain intact.

The normal `/api/status` endpoint still awaits `store.stats()` and, after successful statistics, calls synchronous disk-free-space inspection. A stats read deadline is reported as `storage_health: read_wait_timeout` with unavailable counts/bytes rather than fabricated zeroes or a new storage fault. The separate disk-free call can still delay aggregate status. The memory activity endpoint is the independent heartbeat; the ten-second read budget is not a blanket ten-second bound on every HTTP handler or OS operation.

## Verified outcome and remaining diagnosis

The passing Windows workflow covers gated-worker activity responsiveness, admission/queue cleanup, abandoned reads, disconnected admitted mutations, timeout/callback isolation, fixed counter bounds and error redaction, alongside the existing collector recovery and scale fixtures. The isolated candidate's 1–2 ms activity responses demonstrate that the route is available in that runtime. The synthetic gated test, rather than that point check alone, establishes that its handler bypasses a held SQLite worker.

For a recurrence, the useful discriminator is simultaneous memory activity, application CPU/I/O, correctly mapped volume/device latency and a bounded independent storage observation. High admission/queue waiting behind an old active job identifies actor blockage; coincident independent volume delays would support shared storage delay. Responsive volume observations with high application CPU would instead motivate a matched closed-fixture application profile. A single queue value, a Syncthing scan overlap or a disk-time counter above 100 cannot make that distinction.

Implementation and automated responsiveness checks are verified; the historical root cause remains open. No evidence here establishes that the storage fault was fixed, that Syncthing caused it or that arbitrary future stalls cannot exhaust the collector's existing bounded admission budget.


## Later final-verification recurrence

After the candidate's verified normal exit, direct read-only history inspection
on R: again progressed slowly. A point check found the identified Python reader
at 0.516 CPU seconds, 2,232 read operations and about 9.86 MB of process transfer
reads while its wall time had grown substantially. The R: queue was sampled at
4 with about 124 MB/s reported volume reads. These are point observations and do
not identify the issuing process, physical latency or responsible storage layer.

Only that identified read-only inspection helper was cancelled. The stopped
application database was copied sequentially to C: in 2.486 seconds; complete
source and destination SHA-256 hashes matched. On that byte-identical copy,
integrity, zero pending attempts and all pre-upgrade immutable history comparisons
passed. The application database remained on R:. This further demonstrates a
responsiveness issue worth retaining, rather than establishing corruption or
proving a particular cause. No service, storage or Syncthing change was made.
