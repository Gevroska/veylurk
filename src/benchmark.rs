//! Opt-in, offline measurements through the production Store API, with isolated synthetic data.
use crate::{
    diagnostics::Operation,
    history::{TimeRange, VisitQuery},
    store::{NewObservation, SessionEnd, Store, StoreError},
    timeline::TimelineQuery,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::types::ValueRef;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BASE: &str = "2024-01-01T00:00:00.000Z";
const HOT: &str = "hotaccount";
const CYCLE: usize = 12;

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkConfig {
    pub channels: usize,
    pub events_per_channel: usize,
    pub batch_size: usize,
    pub query_repetitions: usize,
    pub max_seconds: u64,
}

impl BenchmarkConfig {
    pub fn validate(&self) -> Result<()> {
        if !(1..=50).contains(&self.channels)
            || !(200..=20_000).contains(&self.events_per_channel)
            || !(1..=10_000).contains(&self.batch_size)
            || !(1..=20).contains(&self.query_repetitions)
            || !(1..=600).contains(&self.max_seconds)
            || self
                .channels
                .checked_mul(self.events_per_channel)
                .is_none_or(|n| n > 1_000_000)
        {
            return Err(
                std::io::Error::other("benchmark exceeds its finite workload limits").into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct BenchmarkReport {
    pub schema_version: u32,
    pub session_close_strategy: &'static str,
    pub success: bool,
    pub provenance: Value,
    pub config: BenchmarkConfig,
    pub timings: Value,
    pub append_windows: Vec<Value>,
    pub queries: Vec<Value>,
    pub files: Value,
    pub process: Value,
    pub correctness: Value,
}

fn budget(started: Instant, seconds: u64) -> Result<()> {
    if started.elapsed() >= Duration::from_secs(seconds) {
        return Err(std::io::Error::other(
            "elapsed-time budget reached between operations; partial isolated database retained",
        )
        .into());
    }
    Ok(())
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn stamp(base: &DateTime<Utc>, seconds: usize) -> String {
    (*base + chrono::Duration::seconds(seconds as i64)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn event(channel: &str, n: usize, base: &DateTime<Utc>) -> NewObservation {
    let phase = n % CYCLE;
    let kind = match phase {
        1 | 4 | 9 => "names",
        3 | 6 | 7 | 11 => "part",
        _ => "join",
    };
    NewObservation {
        channel: channel.into(),
        account: Some(if matches!(phase, 2 | 7) {
            format!("account{:05}", n / CYCLE)
        } else {
            HOT.into()
        }),
        kind: kind.into(),
        // Preserve receipt order while making one timestamp per cycle move backwards.
        received_at: stamp(base, if phase == 9 { n - 2 } else { n }),
    }
}

fn expected(config: &BenchmarkConfig) -> Value {
    let cycles = config.events_per_channel / CYCLE;
    let remainder = config.events_per_channel % CYCLE;
    let count = |positions: &[usize]| {
        cycles * positions.len() + positions.iter().filter(|&&p| p < remainder).count()
    };
    let rooms = config.channels as i64;
    json!({
        "channels": rooms, "sessions": rooms, "session_channels": rooms,
        "observations": (config.events_per_channel * config.channels) as i64,
        "relationships": ((1 + cycles + usize::from(remainder > 2)) * config.channels) as i64,
        "history_visits": count(&[0,2,4,5,8,9,10]) as i64 * rooms,
        "hot_visits": count(&[0,4,5,8,9,10]) as i64 * rooms,
        "positive_receipts": count(&[0,1,2,4,5,8,9,10]) as i64 * rooms,
        "part_receipts": count(&[3,6,7,11]) as i64 * rooms,
        "ambiguous_visits": count(&[4,9]) as i64 * rooms,
        "out_of_order_visits": (cycles * 2 + if remainder > 9 {2} else {0}) as i64 * rooms,
        "repeated_visits": count(&[1]) as i64 * rooms
    })
}

fn distribution(samples: &[Duration]) -> Value {
    let mut values: Vec<f64> = samples.iter().copied().map(millis).collect();
    values.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        if values.is_empty() {
            0.0
        } else {
            values[((values.len() as f64 * p).ceil() as usize)
                .saturating_sub(1)
                .min(values.len() - 1)]
        }
    };
    json!({"count":values.len(),"sum_ms":values.iter().sum::<f64>(),"p50_ms":percentile(0.50),"p95_ms":percentile(0.95),"p99_ms":percentile(0.99),"max_ms":values.last().copied().unwrap_or(0.0),"percentile_method":"nearest_rank"})
}

#[derive(Default)]
struct FilePeaks {
    database: u64,
    wal: u64,
    combined: u64,
}

impl FilePeaks {
    fn sample(&mut self, path: &Path) {
        let db = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let wal = fs::metadata(format!("{}-wal", path.display()))
            .map(|m| m.len())
            .unwrap_or(0);
        self.database = self.database.max(db);
        self.wal = self.wal.max(wal);
        self.combined = self.combined.max(db + wal);
    }
}

async fn settings(store: &Store) -> std::result::Result<Value, StoreError> {
    store.call_read(Operation::Query, |conn| {
        Ok(json!({
            "sqlite_version": conn.query_row("SELECT sqlite_version()", [], |r|r.get::<_,String>(0))?,
            "journal_mode": conn.pragma_query_value(None,"journal_mode",|r|r.get::<_,String>(0))?,
            "synchronous": conn.pragma_query_value(None,"synchronous",|r|r.get::<_,i64>(0))?,
            "foreign_keys": conn.pragma_query_value(None,"foreign_keys",|r|r.get::<_,i64>(0))?,
            "page_size": conn.pragma_query_value(None,"page_size",|r|r.get::<_,i64>(0))?,
            "wal_autocheckpoint": conn.pragma_query_value(None,"wal_autocheckpoint",|r|r.get::<_,i64>(0))?,
            "journal_size_limit": conn.pragma_query_value(None,"journal_size_limit",|r|r.get::<_,i64>(0))?
        }))
    }).await
}

struct PendingQuery {
    name: &'static str,
    elapsed: Duration,
    rows: Option<usize>,
    has_more: Option<bool>,
    before: Option<i64>,
    repetition: usize,
    // Serialize only after the query wall/CPU measurement phase has ended.
    payload: std::result::Result<Box<dyn FnOnce() -> Result<Value>>, StoreError>,
}

fn query_sample<T: Serialize + 'static>(
    name: &'static str,
    elapsed: Duration,
    result: std::result::Result<Vec<T>, StoreError>,
) -> PendingQuery {
    let rows = result.as_ref().ok().map(Vec::len);
    let payload = result.map(|rows| {
        Box::new(move || Ok(serde_json::to_value(rows)?)) as Box<dyn FnOnce() -> Result<Value>>
    });
    PendingQuery {
        name,
        elapsed,
        rows,
        has_more: None,
        before: None,
        repetition: 0,
        payload,
    }
}

async fn timeline_sample(
    store: &Store,
    name: &'static str,
    channel: Option<String>,
    before: Option<i64>,
    end: &str,
) -> PendingQuery {
    let started = Instant::now();
    let result = store
        .timeline(TimelineQuery {
            account: Some(HOT.into()),
            channel,
            from: Some(BASE.into()),
            to: Some(end.into()),
            before,
            limit: Some(100),
            ..TimelineQuery::default()
        })
        .await;
    let elapsed = started.elapsed();
    let rows = result.as_ref().ok().map(|page| page.intervals.len());
    let has_more = result.as_ref().ok().map(|page| page.has_more);
    let payload = result.map(|page| {
        Box::new(move || Ok(serde_json::to_value(page)?)) as Box<dyn FnOnce() -> Result<Value>>
    });
    PendingQuery {
        name,
        elapsed,
        rows,
        has_more,
        before,
        repetition: 0,
        payload,
    }
}

async fn query_workload(
    store: &Store,
    config: &BenchmarkConfig,
    end: &str,
    started: Instant,
) -> Result<Vec<PendingQuery>> {
    let mut samples = Vec::new();
    // Cursors select representative retained positions; no unbounded timeline walk is timed.
    let cursors = store.call_read(Operation::Query, |conn| {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM history_visits WHERE account=?1",[HOT],|r|r.get(0))?;
        let mut result = Vec::new();
        for offset in [count / 2, (count - 101).max(0)] {
            let id = conn.query_row("SELECT id FROM history_visits WHERE account=?1 ORDER BY id DESC LIMIT 1 OFFSET ?2",rusqlite::params![HOT,offset],|r|r.get::<_,i64>(0))?;
            result.push(id);
        }
        Ok(result)
    }).await?;
    for repetition in 0..config.query_repetitions {
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let first = store
            .visits(VisitQuery {
                account: Some(HOT.into()),
                limit: 100,
                ..VisitQuery::default()
            })
            .await;
        let next = first
            .as_ref()
            .ok()
            .and_then(|rows| rows.last())
            .map(|v| v.id);
        samples.push(query_sample("visits_hot_global_first", at.elapsed(), first));
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store
            .visits(VisitQuery {
                account: Some(HOT.into()),
                before: next,
                limit: 100,
                ..VisitQuery::default()
            })
            .await;
        samples.push(query_sample("visits_hot_global_next", at.elapsed(), result));
        samples.last_mut().unwrap().before = next;
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store
            .visits(VisitQuery {
                account: Some(HOT.into()),
                channel: Some("channel00".into()),
                limit: 100,
                ..VisitQuery::default()
            })
            .await;
        samples.push(query_sample("visits_hot_one_room", at.elapsed(), result));
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store.search(Some(HOT.into()), None, None, 100).await;
        samples.push(query_sample("raw_hot_global", at.elapsed(), result));
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store
            .search_range(
                None,
                Some("channel00".into()),
                None,
                100,
                TimeRange {
                    from: Some(BASE.into()),
                    to: Some(end.into()),
                },
            )
            .await;
        samples.push(query_sample("raw_one_room_range", at.elapsed(), result));
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store.relationships(Some(HOT.into()), None, 0, 100).await;
        samples.push(query_sample(
            "relationships_hot_global",
            at.elapsed(),
            result,
        ));
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store.history("channel00".into(), None, 100).await;
        samples.push(query_sample(
            "session_history_one_room",
            at.elapsed(),
            result,
        ));
        budget(started, config.max_seconds)?;
        let at = Instant::now();
        let result = store
            .visits(VisitQuery {
                account: Some("neverobserved".into()),
                limit: 100,
                ..VisitQuery::default()
            })
            .await;
        samples.push(query_sample("visits_empty_lookup", at.elapsed(), result));
        for (name, channel, cursor) in [
            ("timeline_hot_global_first", None, None),
            ("timeline_hot_global_middle", None, Some(cursors[0])),
            ("timeline_hot_global_deep", None, Some(cursors[1])),
            (
                "timeline_hot_one_room_first",
                Some("channel00".into()),
                None,
            ),
        ] {
            budget(started, config.max_seconds)?;
            samples.push(timeline_sample(store, name, channel, cursor, end).await);
        }
        for sample in samples.iter_mut().rev().take(12) {
            sample.repetition = repetition;
        }
    }
    Ok(samples)
}

struct Identities {
    receipts: Vec<(u32, u32)>,
    sessions: Vec<(u32, u32)>,
}

impl Identities {
    fn lookup(values: &[(u32, u32)], id: i64) -> std::result::Result<(u32, u32), StoreError> {
        usize::try_from(id)
            .ok()
            .and_then(|id| values.get(id))
            .copied()
            .filter(|(_, ordinal)| *ordinal > 0)
            .ok_or(StoreError::Invalid)
    }
    fn receipt(&self, id: i64) -> std::result::Result<(u32, u32), StoreError> {
        Self::lookup(&self.receipts, id)
    }
    fn session(&self, id: i64) -> std::result::Result<(u32, u32), StoreError> {
        Self::lookup(&self.sessions, id)
    }
}

async fn identities(store: &Store, config: &BenchmarkConfig) -> Result<Arc<Identities>> {
    let channels = config.channels;
    let total = config.channels * config.events_per_channel;
    Ok(Arc::new(
        store
            .call_read(Operation::Query, move |conn| {
                let mut receipts = vec![(0, 0); total + 1];
                let mut sessions = vec![(0, 0); channels + 1];
                for (sql, values) in [
                    (
                        "SELECT id,channel FROM observations ORDER BY id",
                        &mut receipts,
                    ),
                    (
                        "SELECT session_id,channel FROM session_channels ORDER BY session_id",
                        &mut sessions,
                    ),
                ] {
                    let mut ordinals = vec![0u32; channels];
                    let mut stmt = conn.prepare(sql)?;
                    let mut rows = stmt.query([])?;
                    while let Some(row) = rows.next()? {
                        let id = row.get::<_, i64>(0)?;
                        let channel = row.get::<_, String>(1)?;
                        let room = channel
                            .strip_prefix("channel")
                            .and_then(|s| s.parse::<usize>().ok())
                            .filter(|&r| r < channels)
                            .ok_or(StoreError::Invalid)?;
                        ordinals[room] += 1;
                        let target = usize::try_from(id)
                            .ok()
                            .and_then(|id| values.get_mut(id))
                            .ok_or(StoreError::Invalid)?;
                        if target.1 != 0 {
                            return Err(StoreError::Invalid);
                        }
                        *target = (room as u32, ordinals[room]);
                    }
                }
                Ok(Identities { receipts, sessions })
            })
            .await?,
    ))
}

fn normalize_result(value: &mut Value, ids: &Identities, session_primary: bool) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                normalize_result(item, ids, session_primary)?;
            }
        }
        Value::Object(fields) => {
            fields.remove("generated_at");
            fields.remove("closed_recorded_at");
            for (name, value) in fields.iter_mut() {
                if let Some(id) = value.as_i64() {
                    let key = match name.as_str() {
                        "session_id" => Some(ids.session(id)?),
                        "id" if session_primary => Some(ids.session(id)?),
                        "id" | "observation_id" | "next_before" => Some(ids.receipt(id)?),
                        _ => None,
                    };
                    if let Some((room, ordinal)) = key {
                        *value = json!([room, ordinal]);
                        continue;
                    }
                }
                normalize_result(value, ids, session_primary)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_queries(
    pending: Vec<PendingQuery>,
    config: &BenchmarkConfig,
    ids: &Identities,
) -> Result<Vec<Value>> {
    let totals = expected(config);
    let hot = totals["hot_visits"].as_u64().unwrap() as usize;
    let per_room = hot / config.channels;
    let cold_rows = (config.events_per_channel / CYCLE) * 2
        + [2, 7]
            .iter()
            .filter(|&&p| p < config.events_per_channel % CYCLE)
            .count();
    let mut digests = BTreeMap::<&str, String>::new();
    let mut output = Vec::new();
    for sample in pending {
        let available = match sample.name {
            "visits_hot_global_first" | "timeline_hot_global_first" => hot,
            "visits_hot_global_next" => hot.saturating_sub(100),
            "visits_hot_one_room" | "timeline_hot_one_room_first" => per_room,
            "timeline_hot_global_middle" => hot - hot / 2 - 1,
            "timeline_hot_global_deep" => hot - hot.saturating_sub(101) - 1,
            "raw_hot_global" => (config.events_per_channel - cold_rows) * config.channels,
            "raw_one_room_range" => config.events_per_channel,
            "relationships_hot_global" => config.channels,
            "session_history_one_room" => 1,
            "visits_empty_lookup" => 0,
            _ => return Err(std::io::Error::other("unknown benchmark query").into()),
        };
        let expected_rows = available.min(100);
        let mut result = json!({"name":sample.name,"wall_ms":millis(sample.elapsed),"rows":sample.rows,"expected_rows":expected_rows,"repetition":sample.repetition,"success":false});
        match sample.payload {
            Err(error) => {
                result["error"] = json!(error.to_string());
            }
            Ok(payload) => {
                let mut payload = payload()?;
                let rows = payload
                    .as_array()
                    .or_else(|| payload.get("intervals").and_then(Value::as_array))
                    .ok_or_else(|| std::io::Error::other("unexpected query result shape"))?;
                let row_ids = rows
                    .iter()
                    .filter_map(|r| r["id"].as_i64())
                    .collect::<Vec<_>>();
                let cursor_valid = sample.name == "session_history_one_room"
                    || row_ids.windows(2).all(|pair| pair[0] > pair[1])
                        && sample
                            .before
                            .is_none_or(|before| row_ids.iter().all(|&id| id < before));
                let scopes_valid = rows.iter().all(|row| {
                    let account_ok = if sample.name.contains("hot") {
                        row["account"] == HOT
                    } else {
                        true
                    };
                    let channel_ok = if sample.name.contains("one_room") {
                        row["channel"] == "channel00"
                    } else {
                        true
                    };
                    account_ok && channel_ok
                });
                let has_more_valid = sample
                    .has_more
                    .is_none_or(|has_more| has_more == (available > 100))
                    && sample.has_more.is_none_or(|has_more| {
                        if has_more {
                            payload["next_before"].as_i64() == row_ids.last().copied()
                        } else {
                            payload["next_before"].is_null()
                        }
                    });
                normalize_result(&mut payload, ids, sample.name == "session_history_one_room")?;
                let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&payload)?));
                let repeat_equal = digests
                    .get(sample.name)
                    .is_none_or(|previous| previous == &digest);
                digests.entry(sample.name).or_insert_with(|| digest.clone());
                result["result_digest_sha256"] = json!(digest);
                result["cursor_order_valid"] = json!(cursor_valid);
                result["scope_valid"] = json!(scopes_valid);
                result["has_more_valid"] = json!(has_more_valid);
                result["repeat_digest_equal"] = json!(repeat_equal);
                result["has_more"] = json!(sample.has_more);
                result["success"] = json!(
                    sample.rows == Some(expected_rows)
                        && cursor_valid
                        && scopes_valid
                        && has_more_valid
                        && repeat_equal
                );
            }
        }
        output.push(result);
    }
    Ok(output)
}

// Hash a typed, length-delimited representation without materializing the retained history.
// Per-room receipt/visit order is retained; global IDs and nondeterministic wall clocks are excluded.
fn hash_rows(
    conn: &rusqlite::Connection,
    hash: &mut Sha256,
    label: &str,
    sql: &str,
    ids: &Identities,
) -> std::result::Result<(), StoreError> {
    hash.update((label.len() as u64).to_le_bytes());
    hash.update(label.as_bytes());
    let mut stmt = conn.prepare(sql)?;
    let width = stmt.column_count();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        hash.update([0xFF]);
        for column in 0..width {
            match row.get_ref(column)? {
                ValueRef::Null => hash.update([0]),
                ValueRef::Integer(n) => {
                    let source = matches!(
                        (label, column),
                        ("observations", 5)
                            | ("history_visits", 12 | 14 | 15 | 16)
                            | ("history_boundaries", 4)
                    );
                    let session = matches!(
                        (label, column),
                        ("sessions", 7)
                            | ("observations", 6)
                            | ("history_visits", 13)
                            | ("history_boundaries", 3)
                    );
                    if source || session {
                        let (room, ordinal) = if source {
                            ids.receipt(n)?
                        } else {
                            ids.session(n)?
                        };
                        hash.update([if source { 0x10 } else { 0x11 }]);
                        hash.update(room.to_le_bytes());
                        hash.update(ordinal.to_le_bytes());
                        continue;
                    }
                    hash.update([1]);
                    hash.update(n.to_le_bytes());
                }
                ValueRef::Real(n) => {
                    hash.update([2]);
                    hash.update(n.to_bits().to_le_bytes());
                }
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                    hash.update([if matches!(row.get_ref(column)?, ValueRef::Text(_)) {
                        3
                    } else {
                        4
                    }]);
                    hash.update((bytes.len() as u64).to_le_bytes());
                    hash.update(bytes);
                }
            }
        }
    }
    Ok(())
}

async fn verify(
    store: &Store,
    config: &BenchmarkConfig,
    started: Instant,
    ids: Arc<Identities>,
) -> Result<Value> {
    budget(started, config.max_seconds)?;
    let expected = expected(config);
    let observed = store
        .call_read(Operation::Query, |conn| {
            let mut result = serde_json::Map::new();
            for table in [
                "channels",
                "sessions",
                "session_channels",
                "observations",
                "relationships",
                "history_visits",
            ] {
                let count: i64 =
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
                result.insert(table.into(), json!(count));
            }
            for (name, sql) in [
                (
                    "hot_visits",
                    "SELECT COUNT(*) FROM history_visits WHERE account='hotaccount'",
                ),
                (
                    "positive_receipts",
                    "SELECT COUNT(*) FROM observations WHERE kind IN ('join','names')",
                ),
                (
                    "part_receipts",
                    "SELECT COUNT(*) FROM observations WHERE kind='part'",
                ),
                (
                    "ambiguous_visits",
                    "SELECT COUNT(*) FROM history_visits WHERE start_basis='uncertain_names'",
                ),
                (
                    "out_of_order_visits",
                    "SELECT COUNT(*) FROM history_visits WHERE out_of_order=1",
                ),
                (
                    "repeated_visits",
                    "SELECT COUNT(*) FROM history_visits WHERE duplicate_or_repeated=1",
                ),
            ] {
                result.insert(
                    name.into(),
                    json!(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?),
                );
            }
            Ok(Value::Object(result))
        })
        .await?;
    budget(started, config.max_seconds)?;
    let validation = store.call_read(Operation::Query, |conn| {
        let integrity: String = conn.query_row("PRAGMA integrity_check",[],|r|r.get(0))?;
        let foreign_key_failures: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check",[],|r|r.get(0))?;
        let totals: (i64,i64) = conn.query_row("SELECT observation_count,session_count FROM collection_totals WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let positive_sum: i64 = conn.query_row("SELECT SUM(positive_observation_count) FROM history_visits",[],|r|r.get(0))?;
        let relationship_sum: i64 = conn.query_row("SELECT SUM(observation_count) FROM relationships",[],|r|r.get(0))?;
        let open_sessions: i64 = conn.query_row("SELECT COUNT(*) FROM sessions WHERE ended_at IS NULL",[],|r|r.get(0))?;
        let open_visits: i64 = conn.query_row("SELECT COUNT(*) FROM history_visits WHERE end_basis IS NULL",[],|r|r.get(0))?;
        Ok(json!({"integrity_check":integrity,"foreign_key_failures":foreign_key_failures,"aggregate_observations":totals.0,"aggregate_sessions":totals.1,"derived_positive_sum":positive_sum,"relationship_observation_sum":relationship_sum,"open_sessions":open_sessions,"open_visits":open_visits}))
    }).await?;
    let mut fingerprints = BTreeMap::new();
    // Separate bounded read jobs retain the production five-second query deadline.
    for (label, sql) in [
        ("channels","SELECT login,tracked,enabled FROM channels ORDER BY login"),
        ("sessions","SELECT sc.channel,s.started_at,s.ended_at,s.end_reason,s.end_uncertain,sc.collection_state,sc.collection_error,s.id FROM sessions s JOIN session_channels sc ON sc.session_id=s.id ORDER BY sc.channel,s.id"),
        ("observations","SELECT channel,account,kind,received_at,received_at_utc,id,session_id FROM observations ORDER BY channel,id"),
        ("relationships","SELECT channel,account,first_observed_at,last_observed_at,observation_count FROM relationships ORDER BY channel,account"),
        ("history_visits","SELECT channel,account,first_observed_at,last_observed_at,end_observed_at,closed_at,start_basis,end_basis,end_reason,positive_observation_count,duplicate_or_repeated,out_of_order,id,session_id,first_observation_id,last_observation_id,end_observation_id FROM history_visits ORDER BY channel,id"),
        ("history_boundaries","SELECT channel,reason,recorded_at,session_id,after_observation_id FROM history_boundaries ORDER BY channel,id"),
        ("models_and_totals","SELECT 'totals',observation_count,session_count FROM collection_totals UNION ALL SELECT 'history_model',version,NULL FROM history_model"),
    ] {
        budget(started, config.max_seconds)?;
        let ids=ids.clone();
        let fingerprint = store.call_read(Operation::Query,move |conn| {
            let mut hash = Sha256::new(); hash_rows(conn,&mut hash,label,sql,&ids)?;
            Ok(format!("{:x}",hash.finalize()))
        }).await?;
        fingerprints.insert(label,fingerprint);
    }
    let encoded = serde_json::to_vec(&fingerprints)?;
    let passed = observed == expected
        && validation["integrity_check"] == "ok"
        && validation["foreign_key_failures"] == 0
        && validation["aggregate_observations"] == expected["observations"]
        && validation["aggregate_sessions"] == expected["sessions"]
        && validation["derived_positive_sum"] == expected["positive_receipts"]
        && validation["relationship_observation_sum"] == expected["observations"]
        && validation["open_sessions"] == 0
        && validation["open_visits"] == 0;
    Ok(
        json!({"passed":passed,"expected":expected,"observed":observed,"validation":validation,"logical_fingerprint_sha256":format!("{:x}",Sha256::digest(encoded)),"table_fingerprints_sha256":fingerprints,"fingerprint_scope":"typed logical content and source/session links normalized to room and receipt/session ordinal; excludes channels.created_at/updated_at, sessions.closed_recorded_at, SQLite layout, and report timings"}),
    )
}

#[derive(Debug, Default, Clone, Serialize)]
struct ProcessSnapshot {
    user_cpu_ms: Option<f64>,
    kernel_cpu_ms: Option<f64>,
    working_set_bytes: Option<u64>,
    peak_working_set_bytes: Option<u64>,
    private_commit_bytes: Option<u64>,
    peak_private_commit_bytes: Option<u64>,
}

#[cfg(windows)]
fn process_snapshot() -> ProcessSnapshot {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };
    let mut result = ProcessSnapshot::default();
    // These APIs inspect only this process; all buffers have their documented size and lifetime.
    unsafe {
        let process = GetCurrentProcess();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) != 0 {
            let ms = |time: FILETIME| {
                ((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)) as f64
                    / 10_000.0
            };
            result.user_cpu_ms = Some(ms(user));
            result.kernel_cpu_ms = Some(ms(kernel));
        }
        let mut memory: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if GetProcessMemoryInfo(
            process,
            (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            memory.cb,
        ) != 0
        {
            result.working_set_bytes = Some(memory.WorkingSetSize as u64);
            result.peak_working_set_bytes = Some(memory.PeakWorkingSetSize as u64);
            result.private_commit_bytes = Some(memory.PrivateUsage as u64);
            result.peak_private_commit_bytes = Some(memory.PeakPagefileUsage as u64);
        }
    }
    result
}

#[cfg(not(windows))]
fn process_snapshot() -> ProcessSnapshot {
    ProcessSnapshot::default()
}

fn cpu_delta(before: &ProcessSnapshot, after: &ProcessSnapshot) -> Value {
    let delta =
        |start: Option<f64>, end: Option<f64>| start.zip(end).map(|(a, b)| (b - a).max(0.0));
    let user = delta(before.user_cpu_ms, after.user_cpu_ms);
    let kernel = delta(before.kernel_cpu_ms, after.kernel_cpu_ms);
    json!({"user_ms":user,"kernel_ms":kernel,"total_ms":user.zip(kernel).map(|(a,b)|a+b)})
}

pub async fn run(output: &Path, config: BenchmarkConfig) -> Result<BenchmarkReport> {
    config.validate()?;
    // Atomic creation rejects existing directories (even empty ones), symlinks, and files.
    // No existing database or credential path is opened anywhere in this harness.
    fs::create_dir(output)?;
    let path = output.join("synthetic.sqlite3");
    let started = Instant::now();
    let initial_process = process_snapshot();
    let opening = Instant::now();
    let store = Store::open(&path).await?;
    let open_ms = millis(opening.elapsed());
    budget(started, config.max_seconds)?;
    let initial_settings = settings(&store).await?;
    let base = DateTime::parse_from_rfc3339(BASE)?.with_timezone(&Utc);
    let end = stamp(&base, config.events_per_channel + 1);
    let names = (0..config.channels)
        .map(|n| format!("channel{n:02}"))
        .collect::<Vec<_>>();
    budget(started, config.max_seconds)?;
    let creating = Instant::now();
    store.track(names.clone(), true).await?;
    let mut sessions = Vec::new();
    for channel in &names {
        budget(started, config.max_seconds)?;
        sessions.push(
            store
                .begin_session_at(vec![channel.clone()], BASE.into())
                .await?,
        );
    }
    let session_create_ms = millis(creating.elapsed());
    let mut peaks = FilePeaks::default();
    peaks.sample(&path);
    let total = config.channels * config.events_per_channel;
    let flushes = total.div_ceil(config.batch_size);
    let mut append = Vec::with_capacity(flushes);
    let mut sql = Duration::ZERO;
    let mut commits = Duration::ZERO;
    let mut preparing = Duration::ZERO;
    let ingest_process_before = process_snapshot();
    let ingest_started = Instant::now();
    for offset in (0..total).step_by(config.batch_size) {
        budget(started, config.max_seconds)?;
        let preparation = Instant::now();
        let mut groups = BTreeMap::<i64, Vec<NewObservation>>::new();
        for position in offset..(offset + config.batch_size).min(total) {
            let room = position % config.channels;
            groups.entry(sessions[room]).or_default().push(event(
                &names[room],
                position / config.channels,
                &base,
            ));
        }
        preparing += preparation.elapsed();
        let appending = Instant::now();
        let (sql_time, commit_time) = store
            .append_sessions_timed(groups.into_iter().collect())
            .await?;
        append.push(appending.elapsed());
        sql += sql_time;
        commits += commit_time;
        peaks.sample(&path);
        let ingested = (offset + config.batch_size).min(total);
        if append.len() == 1 || append.len() % 50 == 0 || ingested == total {
            eprintln!("Offline synthetic benchmark: {ingested}/{total} receipts; {}/{} flushes; elapsed {:.2}s",append.len(),flushes,started.elapsed().as_secs_f64());
        }
    }
    let ingest_wall_ms = millis(ingest_started.elapsed());
    let ingest_process_after = process_snapshot();
    budget(started, config.max_seconds)?;
    let closing = Instant::now();
    budget(started, config.max_seconds)?;
    store
        .end_sessions(
            sessions
                .into_iter()
                .map(|session_id| SessionEnd {
                    session_id,
                    reason: "synthetic_fixture_end".into(),
                    source_at: Some(end.clone()),
                })
                .collect(),
        )
        .await?;
    peaks.sample(&path);
    let session_close_ms = millis(closing.elapsed());
    let query_process_before = process_snapshot();
    let querying = Instant::now();
    let pending_queries = query_workload(&store, &config, &end, started).await?;
    let query_wall_ms = millis(querying.elapsed());
    let query_process_after = process_snapshot();
    budget(started, config.max_seconds)?;
    let verifying = Instant::now();
    let ids = identities(&store, &config).await?;
    budget(started, config.max_seconds)?;
    let validating_queries = Instant::now();
    let queries = validate_queries(pending_queries, &config, &ids)?;
    let query_validation_wall_ms = millis(validating_queries.elapsed());
    let correctness = verify(&store, &config, started, ids).await?;
    let verify_wall_ms = millis(verifying.elapsed());
    budget(started, config.max_seconds)?;
    let final_settings = settings(&store).await?;
    budget(started, config.max_seconds)?;
    peaks.sample(&path);
    let checkpointing = Instant::now();
    store.checkpoint().await?;
    let explicit_checkpoint_ms = millis(checkpointing.elapsed());
    let final_db = fs::metadata(&path)?.len();
    let final_wal = fs::metadata(format!("{}-wal", path.display()))
        .map(|m| m.len())
        .unwrap_or(0);
    let final_process = process_snapshot();
    let settings_ok = initial_settings == final_settings
        && final_settings["journal_mode"] == "wal"
        && final_settings["synchronous"] == 2
        && final_settings["foreign_keys"] == 1;
    let mut windows = Vec::new();
    for quarter in 0..4 {
        let from = quarter * append.len() / 4;
        let to = (quarter + 1) * append.len() / 4;
        windows.push(json!({"quarter":quarter+1,"first_flush":from+1,"last_flush":to,"append_wall":distribution(&append[from..to])}));
    }
    let success = correctness["passed"] == true
        && settings_ok
        && final_wal == 0
        && queries.iter().all(|q| q["success"] == true);
    Ok(BenchmarkReport {
        schema_version: 1,
        session_close_strategy: "one_atomic_durable_transaction",
        success,
        config,
        provenance: json!({"workload":"offline_synthetic_irc_receipts_v1","network_used":false,"twitch_credentials_used":false,"existing_database_used":false,"live_twitch_throughput_claim":false,"timestamp_base":BASE,"fixture_cycle":"JOIN hot; NAMES hot repeat; JOIN distinct cold; PART hot; ambiguous NAMES hot; JOIN hot confirmation; PART hot; PART cold; JOIN hot return; backwards NAMES hot; JOIN hot confirmation; PART hot","scope":"Store ingestion and query measurements; excludes transport, HTTP, browser, and collection reliability","time_budget":"checked between operations; in-flight durable filesystem operations may finish after the budget"}),
        timings: json!({"units":"milliseconds","clock":"monotonic wall clock; CPU reported separately","open_wall_ms":open_ms,"session_create_wall_ms":session_create_ms,"ingest_wall_ms":ingest_wall_ms,"preparation_wall_sum_ms":millis(preparing),"append_wall":distribution(&append),"append_sql_wall_sum_ms":millis(sql),"commit_including_automatic_checkpoint_wall_sum_ms":millis(commits),"automatic_checkpoint_separate_timing_available":false,"session_close_wall_ms":session_close_ms,"query_wall_ms":query_wall_ms,"query_result_validation_wall_ms":query_validation_wall_ms,"correctness_wall_ms":verify_wall_ms,"explicit_final_checkpoint_wall_ms":explicit_checkpoint_ms,"total_wall_ms":millis(started.elapsed())}),
        append_windows: windows,
        queries,
        files: json!({"peak_database_bytes_sampled_after_each_flush":peaks.database,"peak_wal_bytes_sampled_after_each_flush":peaks.wal,"peak_combined_bytes_sampled_after_each_flush":peaks.combined,"final_database_bytes":final_db,"final_wal_bytes":final_wal,"settings_before":initial_settings,"settings_after":final_settings,"settings_unchanged_and_full_durability":settings_ok}),
        process: json!({"source":"Windows GetProcessTimes / GetProcessMemoryInfo; null when unavailable","peak_scope":"process lifetime; snapshots identify measurement versus verification phases","private_commit_note":"private committed bytes, not private working set","before":initial_process,"after_ingest":ingest_process_after,"after_queries_before_verification":query_process_after,"after_verification":final_process,"cpu_ingest":cpu_delta(&ingest_process_before,&ingest_process_after),"cpu_queries_excluding_result_validation":cpu_delta(&query_process_before,&query_process_after),"cpu_total":cpu_delta(&initial_process,&final_process)}),
        correctness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BenchmarkConfig {
        BenchmarkConfig {
            channels: 2,
            events_per_channel: 208,
            batch_size: 128,
            query_repetitions: 2,
            max_seconds: 60,
        }
    }

    #[test]
    fn workload_guards_and_known_default_totals() {
        let mut c = config();
        assert!(c.validate().is_ok());
        c.channels = 51;
        assert!(c.validate().is_err());
        c = config();
        c.batch_size = 10001;
        assert!(c.validate().is_err());
        let e = expected(&BenchmarkConfig {
            channels: 50,
            events_per_channel: 2200,
            ..config()
        });
        assert_eq!(e["observations"], 110000);
        assert_eq!(e["history_visits"], 64150);
        assert_eq!(e["relationships"], 9250);
        assert_eq!(e["hot_visits"], 54950);
    }

    #[tokio::test]
    async fn fresh_directory_refusal_and_batch_independent_logical_evidence() {
        let parent = tempfile::tempdir().unwrap();
        assert!(run(parent.path(), config()).await.is_err());
        let first = run(&parent.path().join("first"), config()).await.unwrap();
        let second = run(
            &parent.path().join("second"),
            BenchmarkConfig {
                batch_size: 2048,
                ..config()
            },
        )
        .await
        .unwrap();
        assert!(first.success, "{:#?}", first.correctness);
        assert!(second.success, "{:#?}", second.correctness);
        assert_eq!(
            first.correctness["logical_fingerprint_sha256"],
            second.correctness["logical_fingerprint_sha256"]
        );
        assert!(run(&parent.path().join("first"), config()).await.is_err());
    }
}
