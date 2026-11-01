# Native browser crash containment

Status: implementation and verification in progress, November 2, 2026.

## Problem and scope

The November 1 test crash was Windows Application Error event 64657 at
21:11:42.561 UTC: access violation `0xc0000005` in
`EmbeddedBrowserWebView.dll` 156.0.4314.58, offset `0x448084`. WebView2 was loaded
inside the application process. Its native failure therefore stopped the HTTP
server, IRC collection and Community collection together. The unfinished sample
was correctly interrupted during subsequent recovery.

Event metadata does not identify the failing native call. No retained crash dump
or demonstrated Rust/COM lifetime error supplies a narrow causal correction.
The existing hidden-window change is not evidence that the DLL fault was fixed.

The correction moves the integrated native browser into a private child process
of the same executable. The server, IRC collector, metadata and SQLite writer
stay in the main process. Production is excluded from development and rollout;
only the isolated test installation on port 8796 may be replaced.

## Required invariants

- Dispatch worker mode before the application's asynchronous runtime, database,
  HTTP listener or metadata credential initialization.
- Assign the waiting child to a parent-owned Windows Job before allowing native
  initialization. The Job must cover subsequent browser descendants.
- Use inherited anonymous pipes, a versioned bounded protocol and validated
  identities. Do not log frames, accounts, credentials, cookies or tokens.
- Preserve the original admission deadlines, cancellation generations, source
  validation and fully delivered receipts. A crash cannot fabricate a successful
  or duplicate sample.
- Keep native window hiding and ordinary explicit sign-in behavior unchanged.
- Bound blocked transport and worker-tree retirement. Verify tree disappearance;
  do not replace a worker with uncertain cleanup.
- On abnormal worker exit, keep the main application alive and fail Community
  closed. Preserve source barriers. Record numeric exit and containment evidence
  separately from normal WebView2 cleanup; do not invent a normal browser exit.
- Do not automatically restart or resample after the first abnormal exit.

The first abnormal worker failure pauses Community until an explicit owner recovery.
The main application continues serving its interface and IRC history. An app
restart retains the durable pause; it does not silently clear source denials.

## Verification plan

1. Inspect the actual Windows event, native lifecycle and Microsoft's API
   requirements; independently review the process-boundary design.
2. Implement worker transport, Job ownership and early dispatch. Add offline
   regressions for malformed/partial/oversized frames, stale results, original
   deadlines, cancellation, full transport and exactly-once completion.
3. Verify actual abnormal child exit and descendant cleanup on Windows. Confirm
   the parent remains responsive, preserves delivered success and records a
   failed unfinished attempt without silently restarting.
4. Independently review the patch and run exact native/application CI. Publish
   only an artifact qualified by the relevant complete workflows.
5. Normally stop the existing test, back up and verify history and credentials,
   then install at the existing approved executable path. Verify authenticated
   hidden collection and normal exit using isolated test data.
6. Inject a bounded worker failure on an isolated test run. Verify main HTTP,
   IRC and SQLite health, persisted failure/barrier and zero worker processes.
   Preserve all history and settings, restart normally, and check both original
   channels. Recheck production identity without modifying it.

Fault injection can establish containment and finite live samples can establish
compatibility. Neither identifies or proves elimination of the original native
DLL defect. Evidence and exact outcomes will be added after verification.

Microsoft references: [process events](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/process-related-events),
[threading model](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/threading-model)
and [controller Close](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller#close).
