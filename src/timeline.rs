//! Bounded, read-only receipt markers and uncertain chat-connection intervals.
use crate::{
    diagnostics::Operation,
    history::{utc, MODEL_VERSION},
    store::{normalize, now, Store, StoreError},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const EXPLANATION: &str = "Positive points are first/last retained JOIN or NAMES receipt endpoints, not every receipt or exact arrival times. PART points are actual departure receipts, including standalone PART receipts; they do not establish subsequent absence. Patterned intervals infer chat connection after a positive receipt until the earliest recorded PART, collection gap, session cutoff, or window end. Ambiguous NAMES and inconsistent or backwards clocks have points only. All other time is unknown. Receipt times do not establish video viewing, watch time, or calibrated confidence. The axis is pinned across pages; retained visits can still change between requests, so this is not a frozen evidence snapshot.";
pub const PAGE_SCOPE: &str = "This page contains only the returned evidence. Additional evidence may be unloaded, including when more pages exist. Unshown or unknown time is never verified absence.";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineQuery {
    pub account: Option<String>,
    pub channel: Option<String>,
    pub broadcast_id: Option<i64>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<usize>,
    pub before: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineRange {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineBroadcast {
    pub id: i64,
    pub channel: String,
    pub started_at: String,
    pub last_live_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineMarker {
    pub observation_id: i64,
    pub at: String,
    pub kind: &'static str,
    pub basis: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineInterval {
    pub kind: String,
    pub id: i64,
    pub account: String,
    pub channel: String,
    pub session_id: i64,
    pub first_observed_at: Option<String>,
    pub last_observed_at: Option<String>,
    pub start_basis: String,
    pub end_basis: String,
    pub end_reason: Option<String>,
    pub supported_visit: bool,
    pub out_of_order: bool,
    pub session_end_uncertain: bool,
    pub boundary_at: Option<String>,
    pub boundary_basis: Option<String>,
    pub inference_from: Option<String>,
    pub inference_to: Option<String>,
    pub inference_end_basis: Option<String>,
    pub markers: Vec<TimelineMarker>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timeline {
    pub account: String,
    pub channel: Option<String>,
    pub broadcast: Option<TimelineBroadcast>,
    pub effective_range: TimelineRange,
    /// Fixed inference cutoff, equal to effective_range.to; not a snapshot version.
    pub as_of: String,
    pub generated_at: String,
    pub empty: bool,
    pub intervals: Vec<TimelineInterval>,
    pub has_more: bool,
    pub next_before: Option<i64>,
    pub limit: usize,
    pub model_version: i64,
    pub confidence: Option<f64>,
    pub confidence_status: &'static str,
    pub page_scope: &'static str,
    pub explanation: &'static str,
}

// Restrict sortable UTC projections to the four-digit-year representation used by SQLite.
fn stamp(value: &str) -> Result<String, StoreError> {
    let value = utc(value)?;
    if value.len() != 24 || !value.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
        return Err(StoreError::Invalid);
    }
    Ok(value)
}

fn datetime(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| StoreError::Invalid)
}

fn format_stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

struct RawInterval {
    item: TimelineInterval,
    first_id: Option<i64>,
    last_id: Option<i64>,
    part_id: Option<i64>,
    part_at: Option<String>,
    closed_at: Option<String>,
    session_start: String,
    session_end: Option<String>,
}

impl RawInterval {
    fn project(mut self, range: &TimelineRange) -> TimelineInterval {
        let item = &mut self.item;
        let in_window = |at: &str| at >= range.from.as_str() && at < range.to.as_str();
        let mut marker = |id: Option<i64>, at: Option<&str>, kind, basis: &str| {
            if let (Some(id), Some(at)) = (id, at.and_then(|at| stamp(at).ok())) {
                if in_window(&at)
                    && !item.markers.iter().any(|m| {
                        m.observation_id == id || (m.at == at && kind != "part" && m.kind != "part")
                    })
                {
                    item.markers.push(TimelineMarker {
                        observation_id: id,
                        at,
                        kind,
                        basis: basis.into(),
                    });
                }
            }
        };
        marker(
            self.first_id,
            item.first_observed_at.as_deref(),
            "first_positive",
            &item.start_basis,
        );
        marker(
            self.last_id,
            item.last_observed_at.as_deref(),
            "last_positive",
            "positive_receipt",
        );
        marker(
            self.part_id,
            self.part_at.as_deref(),
            "part",
            "part_receipt",
        );

        let closed_basis = if item.end_basis == "part" && self.closed_at != self.part_at {
            "inconsistent_boundary"
        } else {
            item.end_basis.as_str()
        };
        let mut boundary = match (&self.closed_at, &self.session_end) {
            (Some(closed), Some(end)) if end < closed => Some((end, "session_end")),
            (Some(closed), _) => Some((closed, closed_basis)),
            (None, Some(end)) => Some((end, "session_end")),
            _ => None,
        };
        if let Some(part) = &self.part_at {
            if boundary.is_none_or(|(at, _)| part <= at) {
                boundary = Some((part, "part"));
            }
        }
        if let Some((at, basis)) = boundary {
            item.boundary_at = Some(at.clone());
            item.boundary_basis = Some(basis.into());
        }
        if item.kind == "departure" {
            item.boundary_at = self.part_at;
            item.boundary_basis = Some("part".into());
            return self.item;
        }

        // Every retained boundary must be coherent before any continuous strip is emitted.
        // Recovery persistence timestamps are deliberately never consulted.
        let coherent = match (&item.first_observed_at, &item.last_observed_at) {
            (Some(first), Some(last)) => {
                stamp(first).is_ok()
                    && stamp(last).is_ok()
                    && stamp(&self.session_start).is_ok()
                    && first <= last
                    && &self.session_start <= first
                    && self
                        .closed_at
                        .as_ref()
                        .is_none_or(|at| stamp(at).is_ok() && at >= last)
                    && self
                        .session_end
                        .as_ref()
                        .is_none_or(|at| stamp(at).is_ok() && at >= last)
                    && self
                        .part_at
                        .as_ref()
                        .is_none_or(|at| stamp(at).is_ok() && at >= last)
                    && (self.part_at.is_none() || item.end_basis == "part")
                    && (item.end_basis != "part"
                        || (self.part_at.is_some()
                            && self.part_id.is_some()
                            && self.part_at == self.closed_at))
                    && (item.end_basis == "open" || self.closed_at.is_some())
            }
            _ => false,
        };
        if coherent
            && item.supported_visit
            && !item.out_of_order
            && item.end_basis != "out_of_order"
        {
            let first = item.first_observed_at.as_ref().expect("coherent positive");
            let from = first.max(&range.from).clone();
            let mut to = range.to.clone();
            let mut basis = "range_end";
            if let Some((at, boundary_basis)) = boundary {
                if at <= &to {
                    to = at.clone();
                    basis = boundary_basis;
                }
            }
            if from < to {
                item.inference_from = Some(from);
                item.inference_to = Some(to);
                item.inference_end_basis = Some(basis.into());
            }
        }
        self.item
    }
}

impl Store {
    pub async fn timeline(&self, query: TimelineQuery) -> Result<Timeline, StoreError> {
        self.timeline_at(query, now()).await
    }

    async fn timeline_at(
        &self,
        query: TimelineQuery,
        generated_at: String,
    ) -> Result<Timeline, StoreError> {
        let account = normalize(query.account.as_deref().ok_or(StoreError::Invalid)?)?;
        let mut channel = query.channel.as_deref().map(normalize).transpose()?;
        let limit = query.limit.unwrap_or(25);
        if !(1..=100).contains(&limit)
            || query.before.is_some_and(|id| id <= 0)
            || query.broadcast_id.is_some_and(|id| id <= 0)
        {
            return Err(StoreError::Invalid);
        }
        let supplied_from = query.from.as_deref().map(stamp).transpose()?;
        let supplied_to = query.to.as_deref().map(stamp).transpose()?;
        let generated_at = stamp(&generated_at)?;
        self.call_read(Operation::Query, move |conn| {
            let broadcast = if let Some(id) = query.broadcast_id {
                Some(conn.query_row("SELECT id,channel,started_at,last_live_at FROM metadata_broadcasts WHERE id=?1", [id], |r| {
                    Ok(TimelineBroadcast { id:r.get(0)?,channel:r.get(1)?,started_at:r.get(2)?,last_live_at:r.get(3)? })
                }).optional()?.ok_or(StoreError::NotFound)?)
            } else { None };
            let broadcast_end = if let Some(broadcast) = &broadcast {
                if channel.as_ref().is_some_and(|channel| channel != &broadcast.channel) {
                    return Err(StoreError::Invalid);
                }
                channel = Some(broadcast.channel.clone());
                let start = stamp(&broadcast.started_at)?;
                let end = stamp(&broadcast.last_live_at)?;
                if start > end { return Err(StoreError::Invalid); }
                Some(stamp(&format_stamp(datetime(&end)?.checked_add_signed(Duration::milliseconds(1)).ok_or(StoreError::Invalid)?))?)
            } else { None };
            let to = supplied_to.or_else(|| broadcast_end.clone()).unwrap_or_else(|| generated_at.clone());
            let from = match supplied_from {
                Some(from) => from,
                None => stamp(&format_stamp(datetime(&to)?.checked_sub_signed(Duration::days(7)).ok_or(StoreError::Invalid)?))?,
            };
            // Validate the caller's original bounds before clipping to broadcast/current time.
            if from >= to || datetime(&to)?.signed_duration_since(datetime(&from)?) > Duration::days(31) {
                return Err(StoreError::Invalid);
            }
            let mut from = from;
            let mut to = to.min(generated_at.clone());
            if let (Some(broadcast), Some(end)) = (&broadcast, broadcast_end) {
                from = from.max(broadcast.started_at.clone());
                to = to.min(end);
            }
            let empty = from >= to;
            if empty { from = to.clone(); }
            let range = TimelineRange { from, to };
            let mut response = Timeline {
                account:account.clone(),channel:channel.clone(),broadcast,effective_range:range.clone(),
                as_of:range.to.clone(),generated_at,empty,intervals:Vec::new(),has_more:false,next_before:None,
                limit,model_version:MODEL_VERSION,confidence:None,confidence_status:"not_calibrated",page_scope:PAGE_SCOPE,explanation:EXPLANATION,
            };
            if empty { return Ok(response); }

            // The union's keys are globally unique source observation IDs: a visit starts on a
            // positive receipt, while an unattached departure has its own PART receipt ID.
            // An attached PART is emitted only as its visit's marker. Both branches are capped
            // before the final merge, and the entire job shares the cancellable query budget.
            let sql = "WITH visits AS (
                SELECT 'visit' AS kind,v.id,v.account,v.channel,v.session_id,v.first_observed_at,v.last_observed_at,
                    v.start_basis,COALESCE(v.end_basis,'open') AS end_basis,v.end_reason,v.out_of_order,
                    v.first_observation_id,v.last_observation_id,v.end_observation_id,v.end_observed_at,v.closed_at,
                    s.started_at,s.ended_at,s.end_uncertain
                FROM history_visits v JOIN sessions s ON s.id=v.session_id
                WHERE v.account=?1 AND (?2 IS NULL OR v.channel=?2) AND v.id<?3
                AND ((v.first_observed_at>=?4 AND v.first_observed_at<?5)
                  OR (v.last_observed_at>=?4 AND v.last_observed_at<?5)
                  OR (v.end_observed_at>=?4 AND v.end_observed_at<?5)
                  OR (v.first_observed_at<?5 AND v.start_basis<>'uncertain_names' AND v.out_of_order=0
                    AND COALESCE(v.end_basis,'open')<>'out_of_order' AND v.first_observed_at<=v.last_observed_at
                    AND s.started_at<=v.first_observed_at AND (v.closed_at IS NULL OR v.closed_at>=v.last_observed_at)
                    AND (s.ended_at IS NULL OR s.ended_at>=v.last_observed_at)
                    AND (v.closed_at IS NULL OR v.closed_at>?4) AND (s.ended_at IS NULL OR s.ended_at>?4)
                    AND (v.end_basis IS NULL OR v.closed_at IS NOT NULL)))
                ORDER BY v.id DESC LIMIT ?6
            ), departures AS (
                SELECT 'departure' AS kind,o.id,o.account,o.channel,o.session_id,NULL,NULL,
                    'part_only','part','part_received',0,NULL,NULL,o.id,o.received_at_utc,o.received_at_utc,
                    s.started_at,s.ended_at,s.end_uncertain
                FROM observations o JOIN sessions s ON s.id=o.session_id
                WHERE o.account=?1 AND (?2 IS NULL OR o.channel=?2) AND o.id<?3 AND o.kind='part'
                    AND o.received_at_utc>=?4 AND o.received_at_utc<?5
                    AND NOT EXISTS(SELECT 1 FROM history_visits v WHERE v.session_id=o.session_id AND v.channel=o.channel AND v.account=o.account AND v.end_observation_id=o.id)
                ORDER BY o.id DESC LIMIT ?6
            ) SELECT * FROM visits UNION ALL SELECT * FROM departures ORDER BY id DESC LIMIT ?6";
            let mut statement = conn.prepare(sql)?;
            let mut rows = statement.query_map(params![account,channel,query.before.unwrap_or(i64::MAX),range.from,range.to,(limit+1) as i64], |r| {
                let kind:String = r.get(0)?;
                let start_basis:String = r.get(7)?;
                Ok(RawInterval {
                    item:TimelineInterval { supported_visit:kind=="visit" && start_basis!="uncertain_names",kind,id:r.get(1)?,account:r.get(2)?,channel:r.get(3)?,session_id:r.get(4)?,
                        first_observed_at:r.get(5)?,last_observed_at:r.get(6)?,start_basis,end_basis:r.get(8)?,end_reason:r.get(9)?,out_of_order:r.get(10)?,session_end_uncertain:r.get(18)?,
                        boundary_at:None,boundary_basis:None,inference_from:None,inference_to:None,inference_end_basis:None,markers:Vec::new() },
                    first_id:r.get(11)?,last_id:r.get(12)?,part_id:r.get(13)?,part_at:r.get(14)?,closed_at:r.get(15)?,session_start:r.get(16)?,session_end:r.get(17)?,
                })
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            response.has_more = rows.len()>limit;
            rows.truncate(limit);
            response.next_before = response.has_more.then(|| rows.last().expect("nonempty limited page").item.id);
            response.intervals = rows.into_iter().map(|row|row.project(&range)).collect();
            Ok(response)
        }).await
    }
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;
