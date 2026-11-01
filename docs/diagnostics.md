# Local fault diagnostics

The application keeps bounded fault records independently of SQLite. This makes a failed storage operation inspectable even when the database cannot accept a diagnostic row. The records are local operational evidence, separate from account observations and inferred visits.

## Community browser faults

Community status keeps the latest source failure separately from the browser cleanup result. A page-readiness deadline can therefore remain visible even if cleanup subsequently fails. The interface labels these details as belonging to the current run. A persisted generic `source_denied` pause from an older run does not establish that Twitch denied a request.

The current Community diagnostic snapshot contains fixed failure and phase codes, structural readiness flags, browser identity, numeric exit and operation results, and elapsed cleanup time. It contains no account names, page text, URLs, cookies or credentials. These details are held in bounded memory and carried with the reserved terminal event. The primary failure and cleanup details are not saved across application restarts; the persisted pause remains separate. Browser cleanup performs no diagnostic file writes.

The service window is auxiliary. Its Close button minimizes it without cancelling an accepted job, closing admission, changing the controller epoch or resuming a paused sign-in. Sign-in can be reopened through the existing control; sampling still requires explicit Resume. Disable and application shutdown still retire the host and require the existing cleanup confirmation. The standalone proof window retains its genuine Close behavior.

`host_termination` retains the first observed stop trigger, its UTC millisecond timestamp, controller epoch and optional job ID. Reasons are fixed identifiers: `shutdown_requested`, `command_channel_closed`, `source_failure_without_controller`, `cleanup_unconfirmed`, `startup_failed`, `message_pump_quit`, `message_pump_error`, `native_host_interrupted` and `proof_window_close`. Cleanup and the exit guard cannot replace an earlier reason. A service window dismissal increments only the bounded `window_dismissal` count and latest timestamp. These two status fields describe the current service-host lifetime and reset when a new host starts; controller recycling does not reset them.

After receiving the terminal report, the coordinator queues only the typed termination snapshot in the existing fault journal as `community_host_termination`. No native-thread file write or additional writer is introduced. This event can become the latest diagnostic but never occupies the first storage failure. Existing record, queue, rotation and process limits still apply. An unexpected host terminal also ends an open Community run with `host_failure` at the observed coordinator boundary; explicit retirement keeps its own end reason. An older unrecorded stop cannot be reconstructed retrospectively.

Browser cleanup requires a matching normal browser-exit notification and successful controller and observer cleanup. An event received before retirement is retained for the same browser identity. A shutdown requested during recycling uses that existing retirement rather than losing its identity. A page timeout may use the existing per-channel backoff after confirmed cleanup; an actual source denial or uncertain cleanup still requires explicit Resume. No player-readiness or source-authentication checks are relaxed.

## Storage fault journal

The journal uses the database filename followed by `.diagnostics.jsonl`, beside that database, and at most one `.previous` file. Each file is limited to 64 KiB, each record to 1 KiB, and each process to 64 recorded faults. A bounded writer queue holds at most 16 records. Diagnostic I/O runs outside the SQLite worker so a diagnostic file cannot add a blocking write to an already failing collection transaction.

## Reading a fault

The versioned schema contains only fixed operation, event, phase, job-class and category identifiers; UTC time, process ID and sequence; elapsed milliseconds; optional numeric SQLite and operating-system error codes; whether the production query-budget callback requested interruption; the observed SQLite autocommit state after the operation; and the optional typed host-termination snapshot. Existing storage and signal records omit `host_termination`. It contains no SQL, parameters, account or channel names, database paths, credentials, request URLs, or arbitrary exception messages.

Collection health exposes the first failed storage operation and the latest diagnostic for the current process. Nonfatal signal-registration faults do not replace the first storage failure. A later cleanup or checkpoint error also does not replace that first failure. An error in a local query or optional metadata write does not by itself mean the collector stopped; inspect the collector state separately.

`progress_interrupt_requested=true` means the production query-budget callback actually returned an interruption request. SQLite primary code 9 identifies `SQLITE_INTERRUPT`; that code without the callback flag does not establish that a query budget caused it. `autocommit_after=true` means no SQLite transaction remains active; it is not, by itself, proof that every preceding operation rolled back. Older records may omit the additive `job_class` field.

Query jobs retain a five-second execution budget, starting when the worker begins that query. Durable mutations install no elapsed-time cancellation handler. This includes observation batches, channel configuration, session and room boundaries, metadata/settings initialization and updates, and checkpointing. A mixed read/write operation uses the durable policy for its whole transaction. Handlers are cleared before and after every job so an expired query cannot affect a later write.

The progress callback cannot preempt a blocked filesystem flush. Elapsed time is not CPU time, and a slow commit does not by itself identify an interruption. Queue waits and remote metadata requests occur outside a query's execution budget. A durable write and requests queued behind it can still wait on slow storage. Existing transaction and batch bounds, the bounded receive buffer, admission deadline, overflow gaps and fail-stop behavior remain unchanged; this policy does not guarantee lossless collection during arbitrary storage stalls.

## Persistence and failure handling

The status response distinguishes `not_attempted`, `pending`, `ready` and `failed` diagnostic persistence, with write-failure, pending and dropped-record counts. An in-memory fault is available before the failed job returns; a queued record is not yet confirmed written. The application attempts a bounded drain before exit. If the diagnostic filesystem is unavailable or the drain times out, persistence remains best-effort. Disk failure, power loss or forced termination can still prevent a file from being written.

Diagnostics never replace the original storage error or recursively log a journal failure. If received collector evidence cannot be committed, collection stops with an explicit failure boundary. Interrupted flushes are not blindly replayed: an earlier batch may already have committed. Existing history and the WAL are retained for investigation. Do not delete the WAL or treat a nonzero process exit as a clean shutdown.

The shutdown selector handles a failed Ctrl+C registration as one diagnostic and continues waiting for the local shutdown request or configured duration. It does not repeatedly poll the failed signal future or treat registration failure as a user stop.

## Validation limits

Offline tests advance a fake query clock beyond five seconds during a real durable append, then verify raw and derived data commit without sleeping. A collector regression verifies the same session continues and commits a subsequent receipt. A separate query regression still produces actual SQLite interruption through the query-budget callback and verifies handler isolation on following queries and writes.

Exceptional write-interruption tests separately install a test-only forced progress callback after real transaction updates. They check actual `SQLITE_INTERRUPT`, complete raw/derived/totals rollback, retained committed baseline, diagnostics and an explicit collector failure boundary. The query-budget flag remains false for that separately injected fault. These tests do not pretend that production durable writes have a query deadline. Other isolated tests cover signal-registration failure, first-failure retention, redaction, rotation, queue limits and failed persistence.

Two early live starts of revision `4cb1c257` stopped with generic storage errors; their cause remains unknown. Later, owner-authorized starts of diagnostic revision `4df70187` retained append failures with SQLite code 9, `progress_interrupt_requested=true` and elapsed wall times of 5,535 and 39,393 milliseconds. Those later records establish that the earlier all-job cancellation policy interrupted real observation writes. They do not establish the cause of the earlier generic errors or a separate CI recovery timeout.

The correction separates query cancellation from durable mutations. It does not raise a timeout, retry partial flushes, relax `synchronous=FULL`, introduce another SQLite connection, or enlarge the receive buffer. Live validation of each newly built executable still requires the owner's firewall authorization.
