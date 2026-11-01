# Shared Community source

The optional native host runs on one dedicated STA thread. Constructing ordinary
application settings or leaving Community disabled does not start it. An explicit
`HostHandle::spawn(HostOptions { profile_dir })` starts the command host and checks
the existing runtime/profile. Admission-ready does not mean browser or source
readiness. The integrated host creates WebView2 only for a valid Sample or
ShowLogin, using that absolute application-owned directory; no browser or
profile is imported.

The coordinator submits one `try_sample(job_id, channel)` and consumes its
`SampleCompleted` before submitting another. Successful payloads contain normalized
members with combined roles (the six `CATEGORIES` bits, mask 63), category row
counts, reported count, Unix-millisecond receipt time, HTTP status and the shared
allowlisted provenance. They do not claim server freshness, coverage or viewing
duration. The source never persists member names; the application storage actor
owns persistence. `ShowLogin` stops sampling and opens normal owner-operated Twitch
sign-in. Passwords and verification codes stay on Twitch.

Commands and status queues are bounded. Cancellation and shutdown use independent
atomic controls, checked by the 250ms native timer and before job callbacks read
their payloads. A single completion and terminal cleanup each have a reserved
slot, so a full status queue cannot block cleanup or discard the active job result.
Admission, accepted job ownership and terminal completion share one short lock;
every accepted sample has one completion even if startup fails before dequeuing it.
An idle pause check has a five-second callback deadline. A queued sample retains
its original 25-second admission deadline; neither waiting nor recycling renews it.
Ordinary source/UI failures close the controller and confirm normal browser exit
before entering dormancy. A later valid command creates the next browser.
Authentication, challenge, integrity and HTTP
401/403/429 failures pause Community. No IRC state is touched.

At quiet boundaries the host recycles before 48 retained navigation IDs, below the
shared gate's limit of 64. It invalidates the controller epoch and job generation,
detaches handlers, closes normally, and requires browser-exit confirmation within
five seconds before replacement. Old callbacks fail their epoch/generation checks.
An unconfirmed cleanup stops this host rather than starting a second controller.
Background jobs use a fixed 1280 by 808 browser viewport and ordinary minimization;
automatic sampling loads only Twitch Popout Chat. Exact route and zero video
elements remain required. Visible positive identity/chat readiness is verified
for two seconds before opening Community. Its expected panel temporarily hides
those controls, so authentication is unobservable until the normal panel Close
action restores chat. The fully validated response is held until visible
authentication is stable again for two seconds, within the original deadline.
Denials still latch throughout. Unexpected video fails the attempt. The manual
full-channel proof retains its separate paused-video contract.

After an accepted service sample, confirmed browser cleanup leaves the host
dormant instead of creating an empty page. Dormant shutdown requires no invented
browser-exit observation. Cold initialization consumes the original accepted
job deadline; cancellation, shutdown and stale commands remain checked.

The finite proof is an adapter over this same parser, mode-specific readiness,
correlation, deadline and native code. Its autonomous source uses Popout Chat
but keeps its finite controller schedule; only the integrated service qualifies
browser dormancy. Only finite aggregate-evidence mode retains overlap/union sets.
Production mode never writes those artifacts or retains cumulative account unions.

Run the crate's formatting, clippy and tests with its committed lockfile on Windows.
The existing local GNU cache lacks `webview2-com`; native compilation and offline
fixtures are checked by the Windows workflow. No fixture run establishes new live
source acceptance or profile compatibility.
