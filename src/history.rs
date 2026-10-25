//! Versioned, recomputable summaries of received IRC evidence, never watch time.
use crate::store::{normalize, Store, StoreError};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub const MODEL_VERSION: i64 = 1;
pub const EXPLANATION: &str = "IRC names/JOIN receipts are positive chat evidence; PART is departure evidence. Receipt timestamps are not event occurrence times. Repeated or delayed events and all unobserved time remain uncertain. Post-departure or backwards-clock roster receipts form observation-only ambiguous segments (supported_visit=false), never a supported return. Visits never establish video watching, complete membership, or continuous presence. Sessions and recorded collection gaps split visits. Older unrecorded room gaps cannot be reconstructed. Span is the distance between positive receipt times, not a watching duration.";
pub const PRESENCE_EXPLANATION: &str = "Observed means a positive receipt at the requested instant. Inferred means an earlier positive receipt in the same collection session with no recorded PART or collection gap before that instant. It is an uncertain candidate, not proof of continuous presence. Missing accounts and collection gaps are unknown, not verified absence. Receipt time is not event occurrence time.";

#[derive(Debug, Clone, Default, Serialize)]
pub struct TimeRange {
    pub from: Option<String>,
    pub to: Option<String>,
}
pub fn utc(value: &str) -> Result<String, StoreError> {
    if value.len() > 64 {
        return Err(StoreError::Invalid);
    }
    DateTime::parse_from_rfc3339(value)
        .map(|at| {
            at.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .map_err(|_| StoreError::Invalid)
}
impl TimeRange {
    pub fn parse(from: Option<String>, to: Option<String>) -> Result<Self, StoreError> {
        let range = Self {
            from: from.as_deref().map(utc).transpose()?,
            to: to.as_deref().map(utc).transpose()?,
        };
        if matches!((&range.from, &range.to), (Some(from), Some(to)) if from >= to) {
            return Err(StoreError::Invalid);
        }
        Ok(range)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Visit {
    pub id: i64,
    pub model_version: i64,
    pub account: String,
    pub channel: String,
    pub session_id: i64,
    pub first_observed_at: String,
    pub last_observed_at: String,
    pub end_observed_at: Option<String>,
    pub start_basis: String,
    pub end_basis: String,
    pub end_reason: Option<String>,
    pub positive_observation_count: i64,
    pub duplicate_or_repeated: bool,
    pub out_of_order: bool,
    pub span_seconds: i64,
    pub uncertain: bool,
    pub supported_visit: bool,
}
fn visit_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Visit> {
    Ok(Visit {
        id: row.get(0)?,
        model_version: MODEL_VERSION,
        account: row.get(1)?,
        channel: row.get(2)?,
        session_id: row.get(3)?,
        first_observed_at: row.get(4)?,
        last_observed_at: row.get(5)?,
        end_observed_at: row.get(6)?,
        start_basis: row.get(7)?,
        end_basis: row.get(8)?,
        end_reason: row.get(9)?,
        positive_observation_count: row.get(10)?,
        duplicate_or_repeated: row.get(11)?,
        out_of_order: row.get(12)?,
        span_seconds: row.get(13)?,
        uncertain: true,
        supported_visit: row.get::<_, String>(7)? != "uncertain_names",
    })
}
const VISIT_COLUMNS: &str = "v.id,v.account,v.channel,v.session_id,v.first_observed_at,v.last_observed_at,v.end_observed_at,v.start_basis,COALESCE(v.end_basis,'open'),v.end_reason,v.positive_observation_count,v.duplicate_or_repeated,v.out_of_order,MAX(0,CAST((julianday(v.last_observed_at)-julianday(v.first_observed_at))*86400+0.0001 AS INTEGER))";

pub(crate) fn initialize(conn: &mut Connection) -> Result<(), StoreError> {
    // The source timestamp remains untouched. A normalized projection permits indexed UTC filters.
    let has_utc = {
        let mut stmt = conn.prepare("PRAGMA table_info(observations)")?;
        let columns = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        columns.iter().any(|name| name == "received_at_utc")
    };
    if !has_utc {
        conn.execute_batch("ALTER TABLE observations ADD COLUMN received_at_utc TEXT;")?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS observations_utc ON observations(received_at_utc,id);
        CREATE INDEX IF NOT EXISTS observations_account_utc ON observations(account,received_at_utc,id);
        CREATE INDEX IF NOT EXISTS observations_channel_utc ON observations(channel,received_at_utc,id);
        CREATE TABLE IF NOT EXISTS history_model(id INTEGER PRIMARY KEY CHECK(id=1),version INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS history_boundaries(id INTEGER PRIMARY KEY,session_id INTEGER NOT NULL,channel TEXT NOT NULL,after_observation_id INTEGER NOT NULL,reason TEXT NOT NULL,recorded_at TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS history_boundaries_receipt ON history_boundaries(after_observation_id,id);
        CREATE TABLE IF NOT EXISTS history_visits(id INTEGER PRIMARY KEY,account TEXT NOT NULL,channel TEXT NOT NULL,session_id INTEGER NOT NULL,first_observed_at TEXT NOT NULL,last_observed_at TEXT NOT NULL,first_observation_id INTEGER NOT NULL,last_observation_id INTEGER NOT NULL,end_observed_at TEXT,end_observation_id INTEGER,closed_at TEXT,start_basis TEXT NOT NULL,end_basis TEXT,end_reason TEXT,positive_observation_count INTEGER NOT NULL,duplicate_or_repeated INTEGER NOT NULL DEFAULT 0,out_of_order INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS history_visits_account ON history_visits(account,id);
        CREATE INDEX IF NOT EXISTS history_visits_channel ON history_visits(channel,id);
        CREATE INDEX IF NOT EXISTS history_visits_time ON history_visits(first_observed_at,last_observed_at,id);
        CREATE INDEX IF NOT EXISTS history_visits_scope ON history_visits(session_id,channel,account,id);
        CREATE INDEX IF NOT EXISTS history_visits_open ON history_visits(session_id,channel,account) WHERE end_basis IS NULL;")?;
    let version: Option<i64> = conn
        .query_row("SELECT version FROM history_model WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    if version.is_some_and(|version| version > MODEL_VERSION) {
        return Err(StoreError::Initialize);
    }
    if version != Some(MODEL_VERSION) {
        recompute(conn)?;
    }
    Ok(())
}

/// Streams retained source rows through the current model without loading the database into memory.
/// Recorded room boundaries are retained independently of derived visits for future model upgrades.
pub fn recompute(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM history_visits", [])?;
    {
        let mut stmt=tx.prepare("SELECT id,session_id,channel,account,kind,received_at,0 AS boundary FROM observations UNION ALL SELECT after_observation_id,session_id,channel,NULL,reason,recorded_at,1 FROM history_boundaries ORDER BY 1,7")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let session: i64 = row.get(1)?;
            let channel: String = row.get(2)?;
            let kind: String = row.get(4)?;
            let raw: String = row.get(5)?;
            if row.get::<_, bool>(6)? {
                close_room(&tx, session, &channel, "coverage_gap", &kind, &raw)?;
                continue;
            }
            let stamp = utc(&raw).ok();
            tx.execute(
                "UPDATE observations SET received_at_utc=?1 WHERE id=?2",
                params![stamp, id],
            )?;
            if let (Some(account), Some(stamp)) = (row.get::<_, Option<String>>(3)?, stamp) {
                derive(&tx, id, session, &channel, &account, &kind, &stamp)?;
            }
        }
    }
    tx.execute("UPDATE history_visits SET end_basis='session_end',end_reason=(SELECT COALESCE(end_reason,'session_end') FROM sessions WHERE id=history_visits.session_id),closed_at=(SELECT ended_at FROM sessions WHERE id=history_visits.session_id) WHERE end_basis IS NULL AND EXISTS(SELECT 1 FROM sessions WHERE id=history_visits.session_id AND ended_at IS NOT NULL)",[])?;
    tx.execute("INSERT INTO history_model(id,version) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET version=excluded.version",[MODEL_VERSION])?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn close_room(
    conn: &Connection,
    session: i64,
    channel: &str,
    basis: &str,
    reason: &str,
    stamp: &str,
) -> rusqlite::Result<()> {
    conn.execute("UPDATE history_visits SET end_basis=?1,end_reason=?2,closed_at=?5 WHERE session_id=?3 AND channel=?4 AND end_basis IS NULL",params![basis,reason,session,channel,stamp])?;
    Ok(())
}
pub(crate) fn gap(
    conn: &Connection,
    session: i64,
    channel: &str,
    reason: &str,
) -> rusqlite::Result<()> {
    let stamp = crate::store::now();
    conn.execute("INSERT INTO history_boundaries(session_id,channel,after_observation_id,reason,recorded_at) VALUES(?1,?2,COALESCE((SELECT MAX(id) FROM observations),0),?3,?4)",params![session,channel,reason,stamp])?;
    close_room(conn, session, channel, "coverage_gap", reason, &stamp)
}
pub(crate) fn derive(
    conn: &Connection,
    id: i64,
    session: i64,
    channel: &str,
    account: &str,
    kind: &str,
    stamp: &str,
) -> rusqlite::Result<()> {
    if !matches!(kind, "names" | "join" | "part") {
        return Ok(());
    }
    let active:Option<(i64,String,String)>=conn.query_row("SELECT id,last_observed_at,start_basis FROM history_visits WHERE session_id=?1 AND channel=?2 AND account=?3 AND end_basis IS NULL ORDER BY id DESC LIMIT 1",params![session,channel,account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if kind == "part" {
        if let Some((visit, last, _)) = active {
            let backwards = stamp < last.as_str();
            conn.execute("UPDATE history_visits SET end_observed_at=?1,closed_at=CASE WHEN ?3 THEN NULL ELSE ?1 END,end_observation_id=?2,end_basis=CASE WHEN ?3 THEN 'out_of_order' ELSE 'part' END,end_reason=CASE WHEN ?3 THEN 'part_receipt_clock_moved_backwards' ELSE 'part_received' END,out_of_order=MAX(out_of_order,?3) WHERE id=?4",params![stamp,id,backwards,visit])?;
        }
        return Ok(());
    }
    if let Some((visit, last, basis)) = active {
        if kind == "join" && basis == "uncertain_names" && stamp >= last.as_str() {
            conn.execute("UPDATE history_visits SET end_basis='join_confirmation',end_reason='join_after_ambiguous_roster',closed_at=?1 WHERE id=?2",params![stamp,visit])?;
            conn.execute("INSERT INTO history_visits(id,account,channel,session_id,first_observed_at,last_observed_at,first_observation_id,last_observation_id,start_basis,positive_observation_count) VALUES(?1,?2,?3,?4,?5,?5,?1,?1,'join',1)",params![id,account,channel,session,stamp])?;
            return Ok(());
        }
        if stamp < last.as_str() {
            conn.execute("UPDATE history_visits SET end_basis='out_of_order',end_reason='receipt_clock_moved_backwards',closed_at=last_observed_at,out_of_order=1 WHERE id=?1",[visit])?;
            let basis = if kind == "names" {
                "uncertain_names"
            } else {
                kind
            };
            conn.execute("INSERT INTO history_visits(id,account,channel,session_id,first_observed_at,last_observed_at,first_observation_id,last_observation_id,start_basis,positive_observation_count,out_of_order) VALUES(?1,?2,?3,?4,?5,?5,?1,?1,?6,1,1)",params![id,account,channel,session,stamp,basis])?;
        } else {
            conn.execute("UPDATE history_visits SET last_observed_at=?1,last_observation_id=?2,positive_observation_count=positive_observation_count+1,duplicate_or_repeated=1 WHERE id=?3",params![stamp,id,visit])?;
        }
    } else {
        let prior:Option<(String,Option<String>)>=conn.query_row("SELECT end_basis,end_observed_at FROM history_visits WHERE session_id=?1 AND channel=?2 AND account=?3 ORDER BY id DESC LIMIT 1",params![session,channel,account],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let ambiguous = kind == "names"
            && prior
                .as_ref()
                .is_some_and(|(basis, _)| matches!(basis.as_str(), "part" | "out_of_order"));
        let backwards = prior
            .as_ref()
            .and_then(|(_, at)| at.as_deref())
            .is_some_and(|at| stamp < at);
        let basis = if ambiguous { "uncertain_names" } else { kind };
        conn.execute("INSERT INTO history_visits(id,account,channel,session_id,first_observed_at,last_observed_at,first_observation_id,last_observation_id,start_basis,positive_observation_count,out_of_order) VALUES(?1,?2,?3,?4,?5,?5,?1,?1,?6,1,?7)",params![id,account,channel,session,stamp,basis,backwards])?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct VisitQuery {
    pub account: Option<String>,
    pub channel: Option<String>,
    pub range: TimeRange,
    pub before: Option<i64>,
    pub offset: usize,
    pub limit: usize,
}
impl Store {
    pub async fn visits(&self, query: VisitQuery) -> Result<Vec<Visit>, StoreError> {
        let account = query.account.as_deref().map(normalize).transpose()?;
        let channel = query.channel.as_deref().map(normalize).transpose()?;
        if query.limit == 0
            || query.limit > 501
            || query.offset > 1_000_000
            || query.before.is_some_and(|v| v <= 0)
        {
            return Err(StoreError::Invalid);
        }
        self.call(move |conn| {
            let mut sql = format!("SELECT {VISIT_COLUMNS} FROM history_visits v WHERE v.id<?");
            let mut values = vec![rusqlite::types::Value::Integer(
                query.before.unwrap_or(i64::MAX),
            )];
            for (field, value) in [("v.account", account), ("v.channel", channel)] {
                if let Some(value) = value {
                    sql.push_str(&format!(" AND {field}=?"));
                    values.push(value.into());
                }
            }
            // Range overlap is based only on positive evidence, never a PART-derived extension.
            if let Some(from) = query.range.from {
                sql.push_str(" AND v.last_observed_at>=?");
                values.push(from.into());
            }
            if let Some(to) = query.range.to {
                sql.push_str(" AND v.first_observed_at<?");
                values.push(to.into());
            }
            sql.push_str(" ORDER BY v.id DESC LIMIT ? OFFSET ?");
            values.push((query.limit as i64).into());
            values.push((query.offset as i64).into());
            let mut stmt = conn.prepare(&sql)?;
            let result = stmt
                .query_map(rusqlite::params_from_iter(values), visit_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(result)
        })
        .await
    }

    pub async fn presence(
        &self,
        channel: String,
        account: Option<String>,
        at: String,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, StoreError> {
        let channel = normalize(&channel)?;
        let account = account.as_deref().map(normalize).transpose()?;
        let at = utc(&at)?;
        if at > crate::store::now() {
            return Err(StoreError::Invalid);
        }
        if limit == 0 || limit > 501 || offset > 1_000_000 {
            return Err(StoreError::Invalid);
        }
        self.call(move|conn| {
            // Require the session's receipt evidence to cover the requested instant. No inference
            // past its recorded end, a PART, a gap, or a backwards-clock boundary.
            let mut stmt=conn.prepare("WITH candidates AS (SELECT v.*,EXISTS(SELECT 1 FROM observations o WHERE o.session_id=v.session_id AND o.channel=v.channel AND o.account=v.account AND o.kind IN ('names','join') AND o.id>=v.first_observation_id AND o.id<=v.last_observation_id AND o.received_at_utc=?1) AS exact FROM history_visits v JOIN sessions s ON s.id=v.session_id WHERE v.channel=?2 AND (?3 IS NULL OR v.account=?3) AND v.first_observed_at<=?1 AND (EXISTS(SELECT 1 FROM observations o WHERE o.session_id=v.session_id AND o.channel=v.channel AND o.account=v.account AND o.kind IN ('names','join') AND o.id>=v.first_observation_id AND o.id<=v.last_observation_id AND o.received_at_utc=?1) OR ((s.ended_at IS NULL OR s.ended_at>?1) AND (v.closed_at IS NULL OR v.closed_at>?1) AND COALESCE(v.end_basis,'open')<>'out_of_order' AND v.start_basis<>'uncertain_names' AND v.out_of_order=0))),ranked AS (SELECT *,ROW_NUMBER() OVER(PARTITION BY account ORDER BY exact DESC,id DESC) AS rank FROM candidates) SELECT v.account,v.session_id,(SELECT MAX(received_at_utc) FROM observations o WHERE o.session_id=v.session_id AND o.channel=v.channel AND o.account=v.account AND o.kind IN ('names','join') AND o.id>=v.first_observation_id AND o.id<=v.last_observation_id AND o.received_at_utc<=?1),v.exact FROM ranked v WHERE rank=1 ORDER BY v.account LIMIT ?4 OFFSET ?5")?;
            let rows=stmt.query_map(params![at,channel,account,limit as i64,offset as i64],|r|Ok(serde_json::json!({"account":r.get::<_,String>(0)?,"session_id":r.get::<_,i64>(1)?,"observed_at":r.get::<_,Option<String>>(2)?,"state":if r.get::<_,bool>(3)? {"observed"}else{"inferred"},"uncertain":true,"explanation":PRESENCE_EXPLANATION})))?.collect::<rusqlite::Result<Vec<_>>>()?;Ok(rows)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewObservation;

    async fn fixture() -> (tempfile::TempDir, Store, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("history.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        (dir, store, session)
    }
    fn event(account: &str, kind: &str, at: &str) -> NewObservation {
        NewObservation {
            channel: "example".into(),
            account: Some(account.into()),
            kind: kind.into(),
            received_at: format!("2026-01-01T{at}Z"),
        }
    }
    async fn all(store: &Store) -> Vec<Visit> {
        store
            .visits(VisitQuery {
                limit: 501,
                ..VisitQuery::default()
            })
            .await
            .unwrap()
    }

    #[test]
    fn utc_ranges_are_half_open_and_reject_invalid_or_reversed_dates() {
        let range = TimeRange::parse(
            Some("2026-01-01T02:00:00+02:00".into()),
            Some("2026-01-01T00:01:00Z".into()),
        )
        .unwrap();
        assert_eq!(range.from.as_deref(), Some("2026-01-01T00:00:00.000Z"));
        assert!(TimeRange::parse(Some("yesterday".into()), None).is_err());
        assert!(TimeRange::parse(range.to, range.from).is_err());
        assert!(TimeRange::parse(
            Some("2026-01-01T00:00:00Z".into()),
            Some("2026-01-01T00:00:00Z".into())
        )
        .is_err());
    }

    #[tokio::test]
    async fn departure_return_part_only_and_repeats_preserve_positive_evidence() {
        let (_dir, store, session) = fixture().await;
        store
            .append(
                session,
                vec![
                    event("alice", "join", "00:00:00"),
                    event("alice", "names", "00:01:00"),
                    event("alice", "names", "00:01:00"),
                    event("alice", "part", "00:05:00"),
                    event("nobody", "part", "00:05:01"),
                    event("alice", "join", "00:10:00"),
                ],
            )
            .await
            .unwrap();
        let rows = all(&store).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].last_observed_at, "2026-01-01T00:01:00.000Z");
        assert_eq!(
            rows[1].end_observed_at.as_deref(),
            Some("2026-01-01T00:05:00.000Z")
        );
        assert_eq!(rows[1].positive_observation_count, 3);
        assert!(rows[1].duplicate_or_repeated);
        assert_eq!(rows[1].span_seconds, 60);
        assert_eq!(rows[1].end_basis, "part");
        assert_eq!(rows[0].start_basis, "join");
        assert_eq!(rows[0].end_basis, "open");
        assert!(rows.iter().all(|v| v.uncertain));
        let missing = store
            .presence("example".into(), None, "2026-01-01T00:06:00Z".into(), 0, 10)
            .await
            .unwrap();
        assert!(missing.is_empty());
        let observed = store
            .presence("example".into(), None, "2026-01-01T00:01:00Z".into(), 0, 10)
            .await
            .unwrap();
        assert_eq!(observed[0]["state"], "observed");
        let inferred = store
            .presence("example".into(), None, "2026-01-01T00:02:00Z".into(), 0, 10)
            .await
            .unwrap();
        assert_eq!(inferred[0]["state"], "inferred");
        let part = store
            .presence("example".into(), None, "2026-01-01T00:05:00Z".into(), 0, 10)
            .await
            .unwrap();
        assert!(part.is_empty());
    }

    #[tokio::test]
    async fn backwards_receipts_split_and_flag_uncertainty_without_reordering_raw_data() {
        let (_dir, store, session) = fixture().await;
        store
            .append(
                session,
                vec![
                    event("alice", "join", "00:10:00"),
                    event("alice", "names", "00:09:00"),
                    event("alice", "part", "00:08:00"),
                ],
            )
            .await
            .unwrap();
        let visits = all(&store).await;
        assert_eq!(visits.len(), 2);
        assert!(visits.iter().all(|v| v.out_of_order));
        assert_eq!(visits[1].end_basis, "out_of_order");
        assert_eq!(visits[0].last_observed_at, "2026-01-01T00:09:00.000Z");
        assert_eq!(
            visits[0].end_observed_at.as_deref(),
            Some("2026-01-01T00:08:00.000Z")
        );
        let raw = store.search(None, None, None, 10).await.unwrap();
        assert_eq!(raw[0].kind, "part");
        assert_eq!(raw.len(), 3);
    }

    #[tokio::test]
    async fn delayed_roster_after_part_does_not_fabricate_a_supported_return_or_presence() {
        let (_dir, store, session) = fixture().await;
        store
            .append(
                session,
                vec![
                    event("alice", "join", "00:00:00"),
                    event("alice", "part", "00:01:00"),
                    event("alice", "names", "00:02:00"),
                    event("alice", "names", "00:02:00"),
                ],
            )
            .await
            .unwrap();
        let rows = all(&store).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].start_basis, "uncertain_names");
        assert!(!rows[0].supported_visit);
        assert_eq!(rows.iter().filter(|v| v.supported_visit).count(), 1);
        assert!(store
            .presence("example".into(), None, "2026-01-01T00:02:01Z".into(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .presence("example".into(), None, "2026-01-01T00:02:00Z".into(), 0, 10)
                .await
                .unwrap()[0]["state"],
            "observed"
        );
        store
            .append(session, vec![event("alice", "join", "00:03:00")])
            .await
            .unwrap();
        let rows = all(&store).await;
        assert_eq!(rows.iter().filter(|v| v.supported_visit).count(), 2);
        assert_eq!(rows[1].end_basis, "join_confirmation");
    }

    #[tokio::test]
    async fn backwards_part_does_not_create_a_false_departure_bound_or_inferred_presence() {
        let (_dir, store, session) = fixture().await;
        store
            .append(
                session,
                vec![
                    event("alice", "join", "00:02:00"),
                    event("alice", "part", "00:01:00"),
                ],
            )
            .await
            .unwrap();
        let rows = all(&store).await;
        assert_eq!(rows[0].end_basis, "out_of_order");
        assert_eq!(rows[0].last_observed_at, "2026-01-01T00:02:00.000Z");
        assert!(store
            .presence("example".into(), None, "2026-01-01T00:03:00Z".into(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .presence("example".into(), None, "2026-01-01T00:02:00Z".into(), 0, 10)
                .await
                .unwrap()[0]["state"],
            "observed"
        );
    }

    #[tokio::test]
    async fn point_in_time_rejects_predictions_beyond_the_current_clock() {
        let (_dir, store, session) = fixture().await;
        store
            .append(session, vec![event("alice", "join", "00:00:00")])
            .await
            .unwrap();
        assert!(matches!(
            store
                .presence("example".into(), None, "2030-01-01T00:00:00Z".into(), 0, 10)
                .await,
            Err(StoreError::Invalid)
        ));
    }

    #[tokio::test]
    async fn collection_gaps_and_sessions_split_visits_and_survive_recomputation() {
        let (_dir, store, session) = fixture().await;
        store
            .append(session, vec![event("alice", "names", "00:00:00")])
            .await
            .unwrap();
        store
            .room_state(session, "example".into(), false)
            .await
            .unwrap();
        store
            .call(|conn| {
                conn.execute(
                    "UPDATE history_boundaries SET recorded_at='2026-01-01T00:01:00.000Z'",
                    [],
                )?;
                conn.execute(
                    "UPDATE history_visits SET closed_at='2026-01-01T00:01:00.000Z'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        store
            .room_state(session, "example".into(), true)
            .await
            .unwrap();
        store
            .append(session, vec![event("alice", "names", "00:02:00")])
            .await
            .unwrap();
        store
            .end_session(session, "connection_lost".into())
            .await
            .unwrap();
        let next = store.begin_session(vec!["example".into()]).await.unwrap();
        store
            .append(next, vec![event("alice", "join", "00:04:00")])
            .await
            .unwrap();
        let before = all(&store).await;
        assert_eq!(before.len(), 3);
        assert_eq!(before[2].end_basis, "coverage_gap");
        assert_eq!(before[1].end_basis, "session_end");
        store.call(recompute).await.unwrap();
        let after = all(&store).await;
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap()
        );
        assert!(store
            .presence("example".into(), None, "2026-01-01T00:01:30Z".into(), 0, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn range_queries_use_normalized_receipts_and_exclusive_upper_bound() {
        let (_dir, store, session) = fixture().await;
        let mut first = event("alice", "join", "00:00:00");
        first.received_at = "2026-01-01T02:00:00+02:00".into();
        store
            .append(
                session,
                vec![
                    first,
                    event("alice", "part", "00:00:59"),
                    event("bob", "join", "00:01:00"),
                ],
            )
            .await
            .unwrap();
        let range = TimeRange::parse(
            Some("2026-01-01T00:00:00Z".into()),
            Some("2026-01-01T00:01:00Z".into()),
        )
        .unwrap();
        let raw = store
            .search_range(None, None, None, 100, range.clone())
            .await
            .unwrap();
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[1].received_at, "2026-01-01T02:00:00+02:00");
        let relationships = store
            .relationships_range(None, None, 0, 100, range.clone())
            .await
            .unwrap();
        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].account, "alice");
        let visits = store
            .visits(VisitQuery {
                range,
                limit: 10,
                ..VisitQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(visits.len(), 1);
        assert_eq!(visits[0].account, "alice");
    }

    #[test]
    fn migration_retains_raw_evidence_and_streams_recomputable_results() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE sessions(id INTEGER PRIMARY KEY,ended_at TEXT,end_reason TEXT);INSERT INTO sessions VALUES(1,'2026-01-01T00:03:00.000Z','shutdown');CREATE TABLE observations(id INTEGER PRIMARY KEY,session_id INTEGER,channel TEXT,account TEXT,kind TEXT,received_at TEXT);INSERT INTO observations VALUES(1,1,'example','alice','names','2026-01-01T02:00:00+02:00'),(2,1,'example','alice','part','2026-01-01T00:02:00Z'),(3,1,'example','bad','names','legacy-invalid-timestamp');").unwrap();
        initialize(&mut conn).unwrap();
        recompute(&mut conn).unwrap();
        let row: (String, String) = conn
            .query_row(
                "SELECT received_at,received_at_utc FROM observations WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "2026-01-01T02:00:00+02:00".into(),
                "2026-01-01T00:00:00.000Z".into()
            )
        );
        let counts: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM observations),(SELECT COUNT(*) FROM history_visits)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (3, 1));
    }

    #[tokio::test]
    async fn new_session_append_remains_bounded_after_large_retained_room_history() {
        let (_dir, store, session) = fixture().await;
        // Isolated synthetic derived history reproduces a long-retained room without
        // spending this test's time generating a hundred thousand source receipts.
        store.call(|conn| {
            conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) INSERT INTO history_visits(id,account,channel,session_id,first_observed_at,last_observed_at,first_observation_id,last_observation_id,start_basis,end_basis,end_reason,positive_observation_count) SELECT 1000000+x,'old'||x,'example',999,'2024-01-01T00:00:00.000Z','2024-01-01T00:00:00.000Z',1000000+x,1000000+x,'join','session_end','shutdown',1 FROM n;")?;Ok(())
        }).await.unwrap();
        let observations = (0..512)
            .map(|n| event(&format!("new{n}"), "names", "00:00:00"))
            .collect();
        // The storage worker enforces its five-second query deadline on this append.
        store.append(session, observations).await.unwrap();
        assert_eq!(store.stats().await.unwrap().observation_count, 512);
        assert_eq!(
            store
                .visits(VisitQuery {
                    account: Some("new511".into()),
                    limit: 10,
                    ..VisitQuery::default()
                })
                .await
                .unwrap()
                .len(),
            1
        );
    }

    async fn scale_workload(channels: usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scale.sqlite3");
        let store = Store::open(&path).await.unwrap();
        let names = (0..channels)
            .map(|n| format!("channel{n}"))
            .collect::<Vec<_>>();
        store.track(names.clone(), true).await.unwrap();
        let session = store.begin_session(names.clone()).await.unwrap();
        let started = std::time::Instant::now();
        let base = DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z").unwrap();
        for channel in names {
            let rows = (0..2200)
                .map(|n| NewObservation {
                    channel: channel.clone(),
                    account: Some(format!("account{}", n / 2)),
                    kind: if n % 2 == 0 { "join" } else { "part" }.into(),
                    received_at: (base + chrono::Duration::hours(n)).to_rfc3339(),
                })
                .collect();
            store.append(session, rows).await.unwrap();
        }
        let range = TimeRange::parse(
            Some("2024-01-10T00:00:00Z".into()),
            Some("2024-04-01T00:00:00Z".into()),
        )
        .unwrap();
        let first = store
            .visits(VisitQuery {
                channel: Some("channel0".into()),
                range: range.clone(),
                limit: 37,
                ..VisitQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(first.len(), 37);
        let second = store
            .visits(VisitQuery {
                channel: Some("channel0".into()),
                range: range.clone(),
                before: first.last().map(|v| v.id),
                limit: 37,
                ..VisitQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(second.len(), 37);
        assert!(second[0].id < first.last().unwrap().id);
        assert_eq!(
            store
                .search_range(None, Some("channel0".into()), None, 51, range)
                .await
                .unwrap()
                .len(),
            51
        );
        assert_eq!(
            store.stats().await.unwrap().observation_count,
            channels as i64 * 2200
        );
        let metadata = crate::metadata::initialize(store.clone()).await.unwrap();
        store.call(|conn| {
            conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<2999) INSERT INTO metadata_checks(channel,checked_at,state,viewer_count) SELECT 'channel0',strftime('%Y-%m-%dT%H:%M:%fZ','2024-01-01','+'||x||' hours'),'live',100+x FROM n;")?;Ok(())
        }).await.unwrap();
        use tower::ServiceExt;
        let response=metadata.router().oneshot(axum::http::Request::builder().uri("/api/channels/channel0/metadata/history?limit=19&from=2024-01-10T00:00:00Z&to=2024-04-01T00:00:00Z").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let history: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(history["observations"].as_array().unwrap().len(), 19);
        assert_eq!(history["has_more"], true);
        drop(metadata);
        store
            .end_session(session, "fixture_shutdown".into())
            .await
            .unwrap();
        store.checkpoint().await.unwrap();
        drop(store);
        let reopened = loop {
            match Store::open(&path).await {
                Ok(store) => break store,
                Err(_) => {
                    assert!(started.elapsed().as_secs() < 120);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        };
        assert_eq!(
            reopened.stats().await.unwrap().observation_count,
            channels as i64 * 2200
        );
        assert_eq!(
            reopened
                .visits(VisitQuery {
                    limit: 20,
                    ..VisitQuery::default()
                })
                .await
                .unwrap()
                .len(),
            20
        );
        eprintln!("Synthetic history workload: {channels} channels, {} retained receipts, {:?}; not live resource validation",channels*2200,started.elapsed());
        assert!(started.elapsed().as_secs() < 120);
    }
    #[tokio::test]
    #[ignore = "Opt-in synthetic retained-history workload"]
    async fn scale_workload_10_channels() {
        scale_workload(10).await;
    }
    #[tokio::test]
    #[ignore = "Opt-in synthetic retained-history workload"]
    async fn scale_workload_25_channels() {
        scale_workload(25).await;
    }
    #[tokio::test]
    #[ignore = "Opt-in synthetic retained-history workload"]
    async fn scale_workload_50_channels() {
        scale_workload(50).await;
    }
}
