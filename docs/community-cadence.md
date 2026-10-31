# Community sampling cadence

Community collection remains disabled by default. In **Collection health**, choose
**Fixed** or **Adaptive** and save the settings. Both modes use one minimized
application-owned browser, with at least 30 seconds between job starts and at
least 30 seconds after a completed attempt. These are conservative engineering
limits, not published Twitch allowances. Actual channel intervals can be longer.

## Modes

Fixed uses the configured per-channel interval. Adaptive treats that interval as
the slow-chat or unknown-count baseline and calculates a target from the latest
successfully saved response:

`max(30, ceil(baseline_seconds * viewer_category_rows / reported_chatters))`

The formula is used only when both counts are positive and reported chatters are
at least the viewer-category row count. Missing, zero or inconsistent inputs use
the baseline, with an explicit fallback reason. Counts from failed, cancelled or
unsaved attempts never change the target. Staff/moderator/VIP/broadcaster totals,
returned unique account totals and video audience counts are not substitutes.

For a 300-second baseline and 100 viewer-category rows, reported counts of 100,
279, 464 and 1,000 produce targets of 300, 108, 65 and 30 seconds respectively.
These are scheduling inputs from a response, not validated population sizes,
sampling fractions, coverage estimates or probabilities of absence.

## Scheduling and status

Channels retain their own due times and are selected by the oldest eligible due
opportunity, with a deterministic login tie-break. A large reported count does
not give a channel permission to skip overdue smaller channels. Failure backoff
is a separate eligibility fence; changing mode or baseline does not clear it.
Already-overdue work remains overdue, and an active sample is not cancelled just
to change cadence. Settings are applied from the existing attempt anchor rather
than repeatedly postponing a channel from the current time.

Health reports each selected channel's calculated target, count/fallback basis,
latest measured interval, consecutive unsuccessful attempts, next eligibility
estimate and last local receipt. Measured intervals describe successful accepted
sample completions; exact source receipt timestamps remain separately visible.
Eligibility times are estimates rather than promised dispatch times. Fifty
channels need at least 25 minutes per circuit even before page-loading delays.

After a restart, count inputs are initially unknown. The first attempt is eligible
subject to global scheduling, then saved responses supply the new counts. No
retained-history scan or unbounded account union is needed. Authentication,
integrity, challenge and rate-limit denials still persistently pause Community;
only explicit Resume clears that pause after browser cleanup.

## Upgrade and local API

Fresh databases start disabled, adaptive, with a 300-second baseline. Existing
Community version 1 databases migrate atomically to version 2 and retain **Fixed**
mode, enabled preference, interval, denial pause, records and identifiers. Choosing
Adaptive is explicit for existing installations. Unknown/corrupt versions or
settings fail without committing a partial migration. Preserve the usual data
backup before upgrading; a version 1 executable cannot open version 2 Community data.

The settings endpoint adds `cadence_mode: "fixed" | "adaptive"`. An older two-field
PATCH body remains accepted: omitted mode is resolved from the coordinator's
current settings when the command is dequeued. The response returns the resolved
full settings. Source/member cursors and graph semantics are unchanged.

This feature changes when samples are requested. It does not turn missing sample
membership into departure, viewing time, 100% discovery or calibrated 99% confidence.
