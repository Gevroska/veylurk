# Experimental Community presence estimates

The Community model estimates chat membership from repeated samples. It adds a
probability layer to the account timeline and the selected-broadcast timeline.
Original Community receipt ticks and IRC evidence remain separate and intact.
The model reads retained records; it makes no additional Twitch requests and
does not change collection settings, raw history or database schema.

The experimental inference layer is implemented. Candidate acceptance is
recorded by the Windows workflow and finite application checks. Empirical
calibration against independently known account sessions is not available.

## Meaning of the display

The model reports a central probability of chat membership and a range across
up to 65 tested parameter scenarios. These are conditional model estimates.
The range across assumptions is not a confidence interval and does not cover
every possible assumption between the tested scenarios.

- **Estimated present:** all tested scenarios assign at least 99% probability
  of membership at the evaluated time.
- **Estimated absent:** all tested scenarios assign at most 1% probability
  of membership at that time.
- **Uncertain:** supported estimates do not all satisfy either threshold.
- **Unknown:** one or more scenarios lack temporal support, or there is not
  enough usable evidence to run the model.

The chart uses at most 2,048 display cells per channel. Each cell is evaluated
at its midpoint, shown in its details. Its color summarizes that point only;
it is not a 99% guarantee for every instant in the cell, an entire colored
strip, a visit or a broadcast. The actual evaluation resolution is reported.
Brief visits can fall between evaluated points. Raw receipt ticks remain
visible independently of this resolution.

Historical smoothing uses later observations up to the displayed `as_of` time.
It can refine an earlier estimate when more observations become available.
The beginning of a modeled segment is left-censored: it is not an estimated
arrival time. Transition regions remain uncertain; the model does not recover
exact departure and return times.

Chat membership and video viewing are different. A returned account is a
receipt from Twitch, not independently verified viewing or watch time.

## Declared assumptions

The settings below are assumed scenario values, not learned behavior of Twitch
or a particular user. They are available in the chart's model controls.

| Entered parameter | Default | Allowed input |
| --- | ---: | ---: |
| Mean connected duration | 60 minutes | 5 to 1,440 minutes |
| Mean disconnected duration | 720 minutes | 30 to 10,080 minutes |
| Effective detection scale | 0.5 | 0.05 to 1 |
| Absent-report likelihood ratio | 0.01 | 0.0001 to 0.1 |
| Assumed response lag | 30 seconds | 0 to 300 seconds |
| Maximum supported observation gap | 3,600 seconds | 60 to 21,600 seconds |

The initial membership probability is 0.5. Sensitivity scenarios test 0.1 and
0.9 instead. They also test half/double connected and disconnected durations,
half/double detection scale, one tenth/ten times the absent-report ratio, and
zero/twice the entered lag. Detection scale and absent-report ratio are clipped
to their input bounds. Sensitivity durations and lag can extend beyond the
entered-input bounds. Identical scenarios are deduplicated.

The model treats reported chat-account total `N` as its assumed sampling
population and uses the count `k` of unique, exclusively Viewer-role members
actually stored for that sample. The reported total has not been established
as the exact population eligible for the Viewer list. Lower effective
detection is an assumption; it is not a measured correction for that mismatch,
selection bias, correlated results or caching.

Independent sample emissions, uniform inclusion within the assumed population,
response lag and natural arrival/departure rates remain unvalidated. Browser
cache checks cannot establish server freshness. The interface must retain the
label **Model estimates — uncalibrated** even when a result reaches 99% under
all tested scenarios.

## Model and observations

Algorithm version: `community_presence_v1`. The hidden state is absent or
present in chat, with repeated arrivals and departures allowed. For mean absent
and present durations `T_absent` and `T_present` in seconds, define:

```text
a = 1 / T_absent
b = 1 / T_present
pi = a / (a + b)
e = exp(-(a + b) * elapsed_seconds)

P00 = 1 - pi + pi*e       P01 = pi*(1-e)
P10 = (1-pi)*(1-e)        P11 = pi + (1-pi)*e
```

For each eligible sample, `p = clamp(detection_scale*k/N, 0.000001, 0.999999)`
and `q = false_report_ratio*p`. Positive observation likelihoods for absent and
present states are `[q,p]`; misses use `[1-q,1-p]`. Normalized forward/backward
smoothing computes the historical probability, including times between
observations. Scenario lag shifts modeled observation times; actual receipt
timestamps are preserved.

Only successful native, post-trigger HTTP 200 network responses with coherent
timestamps, positive encoded length and explicitly false browser cache flags
are eligible. Counts must be coherent, with positive `N >= returned_accounts`,
and `0 < k <= returned Viewer-category rows`. Privileged or mixed-role target
receipts in the analyzed context make that query unsupported for this normal
Viewer-category model. Their original observations remain available.

Each failure, cancellation, interrupted/pending attempt, run change, ineligible
sample, non-increasing receipt clock or excessive gap breaks a segment. Within
a segment, modeling starts at the first eligible Viewer-positive receipt and
requires at least three eligible observations from that point. There is no
extrapolation before or after supported observations. A display cell crossing
any scenario's support or segment boundary is unknown.

## Scope, completeness and resource limits

The API requires an exact account and a channel or selected broadcast. It uses
the existing timeline's inclusive From/exclusive Until bounds, seven-day
default, 31-day request limit, future clamp and confirmed broadcast clipping.
Six hours of preceding channel samples supply context, including before a
broadcast's start. Selecting a broadcast does not itself create a membership
arrival or departure. Identical resolved ranges, context and settings use the
same channel model.

An account overview models up to eight visible evidence channels sequentially,
with an explicit notice if more channels are visible. Filtering a channel
selects it directly. Positive receipt pages identify visible channels only;
each channel model independently reads complete sample opportunities. Loading
another receipt page does not change an already completed model for the same
channel, fixed range and parameters.

Extraction uses indexed success-time and primary-key seeks in one cancellable
storage read. It is limited to 50,000 successful samples, a 200,000-ID attempt
span and 5,000,000 returned member rows before member aggregation. All channel
attempts inside the successful ID span are included, preserving interior
failures and clock problems. Leading/trailing failures cannot create support
outside successful observations. Input overflow produces no partial estimates.

The process admits one expensive extraction-and-computation job at a time
without an analysis queue. Lightweight axis resolution uses the existing store
queue. The admission permit remains owned until extraction and computation
actually finish, including abandoned requests. Pure inference runs outside the
storage worker with cooperative cancellation and a two-second computation
deadline. SQLite retains its existing five-second query progress budget;
filesystem delays can still extend wall time. Busy, timeout and input-limit
results contain no probability cells. Raw evidence remains independently usable.

## Local API

```text
GET /api/community/presence?account=alice&channel=example
    &from=2026-11-01T00:00:00.000Z&to=2026-11-01T01:00:00.000Z
    &connected_minutes=60&disconnected_minutes=720
    &detection_scale=0.5&false_report_ratio=0.01
    &lag_seconds=30&max_gap_seconds=3600
```

`broadcast_id` may supply the channel. Mismatches, unknown fields, invalid dates
and nonfinite/out-of-range parameters are rejected. The existing localhost
Host protections apply.

The response includes version, parameters, effective/context ranges, `as_of`,
resolution, sample/exclusion counts, completeness and status. Every probability
cell contains `from`, `to`, `evaluated_at`, `presence_probability`,
`sensitivity_min`, `sensitivity_max`, classification and an optional unknown
reason. Metadata includes `calibration_status: "uncalibrated"`,
`probability_kind: "model_based_pointwise"`,
`inference: "historical_smoother"` and `threshold: 0.99`.

Processing-complete statuses can still be `no_data`, `insufficient_data` or
`role_unsupported`; these contain no probability cells. Resource failures use
`input_limit`, `analysis_busy` or `analysis_timeout`, with `complete: false` and
no partial cells. A storage failure returns the normal service error. Existing
IRC timeline confidence fields are unchanged.

## Validation and remaining calibration

Deterministic tests compare small hidden-state calculations against exhaustive
state enumeration and exercise stays, departures, returns, time-varying counts,
lag, failures, roles, cache uncertainty, gaps and bounds. Synthetic data has
known constructed answers; passing those tests validates implementation, not
Twitch sampling assumptions or real-world calibration.

For illustration only, independent uniform samples of 100 from 400 eligible
accounts have conditional miss probability `0.75^m` for a continuously present
account. Sixteen misses give 1.00226%; seventeen give 0.75169%. This is not the
posterior probability of absence, and the default dynamic model need not give
these values.

Empirical calibration requires separately recorded reference chat sessions,
including confirmed entry, stay, exit and return periods. Detection rates,
miss-run dependence, selection bias, reporting delays and transition errors
must be evaluated on held-out representative sessions, accounts and channels.
Scripted durations do not establish natural users' return rates. A calibrated
pointwise result and simultaneous coverage of an entire trajectory are separate
claims. Neither is certified by this implementation or its synthetic tests.
