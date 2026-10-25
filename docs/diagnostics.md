# Local fault diagnostics

The application keeps bounded fault records independently of SQLite. This makes a failed storage operation inspectable even when the database cannot accept a diagnostic row. The records are local operational evidence, separate from account observations and inferred visits.

The journal uses the database filename followed by `.diagnostics.jsonl`, beside that database, and at most one `.previous` file. Each file is limited to 64 KiB, each record to 1 KiB, and each process to 64 recorded faults. A bounded writer queue holds at most 16 records. Diagnostic I/O runs outside the SQLite worker so a diagnostic file cannot add a blocking write to an already failing collection transaction.

## Reading a fault

The versioned schema contains only fixed operation, event, phase and category identifiers; UTC time, process ID and sequence; elapsed milliseconds; optional numeric SQLite and operating-system error codes; whether the installed progress callback requested interruption; and the observed SQLite autocommit state after the operation. It contains no SQL, parameters, account or channel names, database paths, credentials, request URLs, or arbitrary exception messages.

Collection health exposes the first failed storage operation and the latest diagnostic for the current process. Nonfatal signal-registration faults do not replace the first storage failure. A later cleanup or checkpoint error also does not replace that first failure. An error in a local query or optional metadata write does not by itself mean the collector stopped; inspect the collector state separately.

`progress_interrupt_requested=true` means the installed SQLite progress callback actually returned an interruption request. SQLite primary code 9 identifies `SQLITE_INTERRUPT`; that code without the callback flag does not establish that Veylurk's deadline caused it. `autocommit_after=true` means no SQLite transaction remains active; it is not, by itself, proof that every preceding operation rolled back.

The progress callback cannot preempt a blocked filesystem flush. Elapsed time is not CPU time, and a slow commit does not by itself identify a deadline interruption. Queue waits and remote metadata requests occur outside a storage job's execution budget.

## Persistence and failure handling

The status response distinguishes `not_attempted`, `pending`, `ready` and `failed` diagnostic persistence, with write-failure, pending and dropped-record counts. An in-memory fault is available before the failed job returns; a queued record is not yet confirmed written. The application attempts a bounded drain before exit. If the diagnostic filesystem is unavailable or the drain times out, persistence remains best-effort. Disk failure, power loss or forced termination can still prevent a file from being written.

Diagnostics never replace the original storage error or recursively log a journal failure. If received collector evidence cannot be committed, collection stops with an explicit failure boundary. Interrupted flushes are not blindly replayed: an earlier batch may already have committed. Existing history and the WAL are retained for investigation. Do not delete the WAL or treat a nonzero process exit as a clean shutdown.

The shutdown selector handles a failed Ctrl+C registration as one diagnostic and continues waiting for the local shutdown request or configured duration. It does not repeatedly poll the failed signal future or treat registration failure as a user stop.

## Validation limits

Offline tests force an interruption inside the real append transaction and check the actual SQLite code, callback flag, raw and derived rollback, retained committed baseline, collection failure boundary, and a fresh deadline on the following job. Other tests cover signal-registration failure, first-failure retention, redaction, rotation, queue limits and failed persistence. These use isolated data and loopback fixtures.

Two early live starts of revision `4cb1c257` stopped with generic storage errors. That revision did not retain the underlying SQLite code. Later two-channel runs were healthy, but neither those runs nor the new fault-injection tests establish the cause of the earlier errors. This increment improves diagnosis and fixes signal-result handling; it leaves the storage deadline and recovery policy unchanged.
