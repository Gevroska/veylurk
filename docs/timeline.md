# Account timelines and confidence

An account timeline shows received Twitch chat evidence on a shared time axis. It does not measure video viewing or establish continuous attendance.

## Use the chart

In **Observation history**, enter an exact account login and search. Leave the channel field empty to compare channels, or enter a channel to inspect that pair. The existing From and Until controls select the time window.

In **Broadcasts**, enter the viewer's exact login in **Viewer / account login** and leave **Channel login (optional)** empty to list associated broadcasts across all channels. Add a channel only to restrict that account's results to one streamer. Open a broadcast's evidence summary for its account timeline, restricted to the intersection of the selected dates and the broadcast's reported start through its last confirmed live check. An offline check does not extend that window.

The default window is seven days, with a maximum of 31 days per query. Choose narrower date bounds when inspecting older history. The chart displays the effective dates returned by the server. Broadcasts shorter than the selected window use their own reported bounds.

Each page contains at most 25 timeline records in the interface. The API accepts up to 100. Use the page controls to inspect more evidence. The full search period stays fixed while changing pages, but collection can update derived visits between requests; pages are not an immutable snapshot. A partial page is labelled as partial: evidence outside the displayed page must not be interpreted as missing from the database.

Hold the primary mouse button inside the plot and drag horizontally between two times to zoom into that interval. Either direction works. **Zoom in**, **Zoom out** and **Reset zoom** also work from the keyboard. Zoom changes the visible portion of the loaded evidence, keeps the search dates and page cursors intact, and persists while paging through the same search. A new search or selected broadcast resets it. Clipped intervals do not create receipt markers at the zoom edges, and unloaded time remains unknown.

Hover over a positive receipt point or a patterned interval, or focus a supported shape with the keyboard, to inspect the recorded broadcast title for that channel and time. A point uses its exact receipt timestamp; a patterned interval uses the selected horizontal position. The details distinguish the selected time, the metadata check time and the first recorded time of that title. Between metadata checks, **Last reported title** is a historical snapshot, not a verified title change time.

The lookup never fills older periods with a broadcast's current title. Missing title history, unknown/offline reports, conflicting transitions, times outside the confirmed broadcast window and metadata gaps produce an unavailable explanation. Nonexact lookups across checks more than 150 seconds apart are conservatively unavailable; this is a display policy, not a confidence calculation. Stored historical titles remain available without current Twitch credentials. Hovering reads local history and makes no Twitch request.

## Read the evidence

- **Positive receipt points:** actual first and last NAMES/JOIN receipt bounds for a displayed evidence visit. These are not every intervening event. Original receipt times outside the selected window are not moved onto its edges.
- **Patterned inferred intervals:** an earlier supported positive receipt with no recorded departure or collection boundary before the indicated time, within the same session. Continuity remains uncertain. Clipping an interval does not create a new receipt.
- **Departure markers:** received PART events. A PART terminates inference; it does not establish the exact time the person left or prove absence afterward. Standalone PART receipts can appear without an earlier positive visit.
- **Unknown background:** presence and absence are not established by the displayed evidence. Missing receipts, interruptions and unloaded pages cannot prove absence.

Ambiguous roster entries after a departure and out-of-order evidence do not produce inferred intervals. Collection gaps and session boundaries prevent inferred continuity across them. Older gaps that were never recorded cannot be reconstructed. Reported boundary and receipt times remain available in the accessible details table.

## Why the chart does not claim 99%

A mathematical confidence calculation requires a justified observation model. For independent, uniform samples of size `k` from a fixed population `N`, the chance of missing a continuously present account in `m` samples is:

```text
P(no detection | continuously present) = (1 - k/N)^m
```

With 100 accounts sampled from 2,000, 90 independent samples reduce that conditional miss probability to about 0.99%. At three seconds per sample, the interval is four minutes and 30 seconds. This is a detection calculation under explicit assumptions. It is not `P(absent | no detection)` and does not identify an arrival or departure time.

Estimating the latter would additionally require assumptions about departures, returns, detection errors and observation dependence, followed by validation against known presence. A numerical model can produce a probability without that validation, but the result would not justify a calibrated 99% claim.

Veylurk's current IRC event stream is not an independent uniform sampler. Twitch documents missing membership events in large chat rooms, and chat-connected accounts are a different population from video viewers. The chart therefore reports evidence and uncertainty without a probability score or an absence duration.

Sources: [NIST binomial distribution](https://www.itl.nist.gov/div898/handbook/eda/section3/eda366i.htm), [Twitch IRC membership limits](https://dev.twitch.tv/docs/chat/irc/#part-command), and [Get Chatters](https://dev.twitch.tv/docs/api/reference/#get-chatters).

## Preservation and resource bounds

The endpoint is read-only and uses existing observations, visits and session boundaries. It neither changes collection cadence nor rewrites history. Response size, page size, date range and query execution are bounded. The chart uses embedded SVG and existing theme styles; collection requires no chart library or browser runtime.

The Windows workflow runs ordinary tests and the fixed-time 50-room buffered storage recovery fixture in separate test processes. The recovery fixture retains its debug profile, workload, four Tokio workers, PONG checks, durability assertions and original deadlines. Isolation removes overlapping storage workers from other tests; it does not establish why an earlier recovery deadline was missed or constitute extended live-capacity evidence.
