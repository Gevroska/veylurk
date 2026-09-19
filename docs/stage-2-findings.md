# Stage 2 transport findings

## Result

The preferred standalone anonymous HTTP transport is **blocked by Twitch's integrity challenge** as tested on 20 September 2026. This is a transport result, not a claim that the normal Twitch browser UI is inaccessible.

One preliminary direct request used the public Twitch web client identifier and the `CommunityTab` persisted query referenced by the supplied userscript. Twitch returned HTTP 200 with partial channel and live-stream metadata, but `channel.chatters` was `null`. The GraphQL error code was `IntegrityCheckFailed`, and the response requested an integrity challenge. No authentication material, browser token, cookie, or user secret was supplied. No challenge bypass was attempted.

The tested channel was `ouaiseddy`, which Browser independently confirmed was live at the time. Since no chatter sample succeeded, there is no overlap, union, freshness, sampling-cadence, CPU, or memory result for successful collection. Stage 1 browser observations remain the evidence that repeated UI samples can change.

## Prototype

The Rust prototype makes this boundary reproducible and fails closed. It validates the requested channel name, conditionally cross-checks IDs, requires the chatter role arrays, deduplicates across roles case-insensitively, and distinguishes integrity, authorization, rate-limit, persisted-query, schema, transport, and cross-channel failures.

It is bounded to ten sequential channels, twenty samples per channel, a minimum 15-second interval, a global timeout, three total transport attempts, and a 2 MiB response. It emits aggregate metrics only and has no database. Missing stream metadata is unknown rather than offline.

## Decision

Do not design the production collector around anonymous direct calls to this internal GraphQL operation. A maintainable transport boundary must be selected and validated before the full collector is built. A browser-assisted adapter could be evaluated separately, including resource cost, session handling, Twitch changes, and acceptable behavior across dozens of channels.

The current evidence supports only “observed in a chat-connected-account sample.” It does not support “viewer,” “watch time,” exact presence intervals, or statistical departure confidence.
