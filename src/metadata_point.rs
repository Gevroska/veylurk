//! Bounded historical metadata context; never an assertion of continuous live status.
use super::{database_error, Metadata};
use crate::{
    diagnostics::Operation,
    history::utc,
    store::{normalize, StoreError},
};
use axum::{
    extract::{rejection::QueryRejection, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const PRECEDING: &str = "SELECT id,checked_at,state,broadcast_id FROM metadata_checks WHERE channel=?1 AND checked_at<=?2 ORDER BY checked_at DESC,id DESC LIMIT 1";
const FOLLOWING: &str = "SELECT id,checked_at,state,broadcast_id FROM metadata_checks WHERE channel=?1 AND checked_at>?2 ORDER BY checked_at,id LIMIT 1";
const BROADCAST: &str = "SELECT id,channel,started_at,first_seen_at,last_live_at,offline_observed_at FROM metadata_broadcasts WHERE id=?1";
const TITLE: &str = "SELECT title,seen_at,game_name,language FROM metadata_titles WHERE broadcast_id=?1 AND seen_at<=?2 ORDER BY seen_at DESC,id DESC LIMIT 1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PointQuery {
    at: String,
    broadcast_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct Check {
    id: i64,
    checked_at: String,
    state: String,
    broadcast_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct Broadcast {
    id: i64,
    channel: String,
    started_at: String,
    first_seen_at: String,
    last_live_at: String,
    offline_observed_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Title {
    title: String,
    seen_at: String,
    game_name: String,
    language: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Context {
    channel: String,
    at: String,
    basis: &'static str,
    reason: Option<&'static str>,
    check: Option<Check>,
    next_check: Option<Check>,
    broadcast: Option<Broadcast>,
    title: Option<Title>,
}

fn stamp(value: &str) -> Result<String, StoreError> {
    let at = utc(value)?;
    if at.len() != 24 || !at.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
        return Err(StoreError::Invalid);
    }
    Ok(at)
}

fn canonical(value: &str) -> bool {
    stamp(value).is_ok_and(|normalized| normalized == value)
}

fn milliseconds_between(from: &str, to: &str) -> Option<i64> {
    Some(
        DateTime::parse_from_rfc3339(to)
            .ok()?
            .signed_duration_since(DateTime::parse_from_rfc3339(from).ok()?)
            .num_milliseconds(),
    )
}

fn check_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Check> {
    Ok(Check {
        id: row.get(0)?,
        checked_at: row.get(1)?,
        state: row.get(2)?,
        broadcast_id: row.get(3)?,
    })
}

fn broadcast(conn: &Connection, id: i64) -> rusqlite::Result<Option<Broadcast>> {
    conn.query_row(BROADCAST, [id], |row| {
        Ok(Broadcast {
            id: row.get(0)?,
            channel: row.get(1)?,
            started_at: row.get(2)?,
            first_seen_at: row.get(3)?,
            last_live_at: row.get(4)?,
            offline_observed_at: row.get(5)?,
        })
    })
    .optional()
}

fn valid_broadcast(value: &Broadcast, channel: &str) -> bool {
    value.channel == channel
        && value.id > 0
        && canonical(&value.started_at)
        && canonical(&value.first_seen_at)
        && canonical(&value.last_live_at)
        && value.started_at <= value.first_seen_at
        && value.first_seen_at <= value.last_live_at
        && value
            .offline_observed_at
            .as_deref()
            .is_none_or(|at| canonical(at) && at >= value.last_live_at.as_str())
}

fn lookup(
    conn: &Connection,
    channel: String,
    at: String,
    selected: Option<i64>,
) -> Result<Context, StoreError> {
    // Validate the explicit scope even when no check exists. No current metadata health is needed.
    if let Some(id) = selected {
        let requested = broadcast(conn, id)?.ok_or(StoreError::NotFound)?;
        if requested.channel != channel {
            return Err(StoreError::Invalid);
        }
    }
    let check = conn
        .query_row(PRECEDING, params![channel, at], check_row)
        .optional()?;
    let next_check = conn
        .query_row(FOLLOWING, params![channel, at], check_row)
        .optional()?;
    let mut context = Context {
        channel,
        at,
        basis: "unavailable",
        reason: Some("no_history"),
        check,
        next_check,
        broadcast: None,
        title: None,
    };
    let Some(check) = context.check.as_ref() else {
        return Ok(context);
    };
    context.reason = Some("inconsistent_history");
    if !canonical(&check.checked_at) {
        return Ok(context);
    }
    match check.state.as_str() {
        "unknown" => {
            context.reason = Some("metadata_unknown");
            return Ok(context);
        }
        "offline" => {
            context.reason = Some("offline_report");
            return Ok(context);
        }
        "live" => {}
        _ => return Ok(context),
    }
    let Some(id) = check.broadcast_id else {
        return Ok(context);
    };
    let Some(value) = broadcast(conn, id)? else {
        return Ok(context);
    };
    if !valid_broadcast(&value, &context.channel)
        || check.checked_at < value.first_seen_at
        || check.checked_at > value.last_live_at
    {
        return Ok(context);
    }
    if selected.is_some_and(|selected| selected != id) {
        context.reason = Some("broadcast_mismatch");
        return Ok(context);
    }
    let exact = check.checked_at == context.at;
    let in_window = value.started_at <= context.at && context.at <= value.last_live_at;
    context.broadcast = Some(value);
    if !in_window {
        context.reason = Some("outside_broadcast_window");
        return Ok(context);
    }
    if !exact {
        if let Some(next) = context.next_check.as_ref() {
            if !canonical(&next.checked_at)
                || !matches!(next.state.as_str(), "live" | "offline" | "unknown")
            {
                return Ok(context);
            }
            if next.state == "live" && next.broadcast_id != Some(id) {
                let Some(next_id) = next.broadcast_id else {
                    return Ok(context);
                };
                let Some(next_broadcast) = broadcast(conn, next_id)? else {
                    return Ok(context);
                };
                if !valid_broadcast(&next_broadcast, &context.channel)
                    || next.checked_at < next_broadcast.first_seen_at
                    || next.checked_at > next_broadcast.last_live_at
                {
                    return Ok(context);
                }
                if next_broadcast.started_at <= context.at {
                    context.reason = Some("transition_ambiguous");
                    return Ok(context);
                }
            }
        }
        let next_at = context
            .next_check
            .as_ref()
            .map_or(context.at.as_str(), |next| next.checked_at.as_str());
        if milliseconds_between(&check.checked_at, next_at)
            .is_none_or(|milliseconds| milliseconds > 150_000 || milliseconds < 0)
        {
            context.reason = Some("polling_gap");
            return Ok(context);
        }
    }
    // Use the last reported title at the preceding CHECK, never a future change or b.title.
    let title = conn
        .query_row(TITLE, params![id, check.checked_at], |row| {
            Ok(Title {
                title: row.get(0)?,
                seen_at: row.get(1)?,
                game_name: row.get(2)?,
                language: row.get(3)?,
            })
        })
        .optional()?;
    let Some(title) = title else {
        context.reason = Some("missing_title_history");
        return Ok(context);
    };
    if !canonical(&title.seen_at)
        || title.seen_at
            < context
                .broadcast
                .as_ref()
                .expect("validated broadcast")
                .first_seen_at
    {
        return Ok(context);
    }
    context.title = Some(title);
    context.basis = if exact {
        "exact_check"
    } else {
        "last_reported"
    };
    context.reason = None;
    Ok(context)
}

fn invalid() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(
            json!({"error":{"code":"invalid_query","message":"Supply an exact channel login, an RFC3339 instant and an optional positive broadcast ID."}}),
        ),
    )
}

pub(super) async fn at(
    State(service): State<Metadata>,
    Path(login): Path<String>,
    query: Result<Query<PointQuery>, QueryRejection>,
) -> Result<Json<Context>, (StatusCode, Json<Value>)> {
    let Query(query) = query.map_err(|_| invalid())?;
    let channel = normalize(&login).map_err(|_| invalid())?;
    let at = stamp(&query.at).map_err(|_| invalid())?;
    if query.broadcast_id.is_some_and(|id| id <= 0) {
        return Err(invalid());
    }
    service
        .store
        .call_read(Operation::Query, move |conn| {
            lookup(conn, channel, at, query.broadcast_id)
        })
        .await
        .map(Json)
        .map_err(|error| match error {
            StoreError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(json!({"error":{"code":"not_found","message":"Broadcast not found."}})),
            ),
            StoreError::Invalid => invalid(),
            error => database_error(error),
        })
}

#[cfg(test)]
#[path = "metadata_point_tests.rs"]
mod tests;
