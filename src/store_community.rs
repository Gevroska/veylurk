//! Independent Community response receipts. These records never enter IRC history derivation.
use super::{normalize, now, Store, StoreError};
use crate::diagnostics::Operation;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const COMMUNITY_SCHEMA_VERSION: i64 = 3;
pub const COMMUNITY_MAX_MEMBERS: usize = 4096;
pub const COMMUNITY_ROLE_BROADCASTER: u32 = 1;
pub const COMMUNITY_ROLE_STAFF: u32 = 2;
pub const COMMUNITY_ROLE_MODERATOR: u32 = 4;
pub const COMMUNITY_ROLE_VIP: u32 = 8;
pub const COMMUNITY_ROLE_CHATBOT: u32 = 16;
pub const COMMUNITY_ROLE_VIEWER: u32 = 32;
const ROLE_MASK: u32 = 63;
const MAX_TIMING_MS: f64 = 86_400_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityCadenceMode {
    Fixed,
    Adaptive,
}

impl CommunityCadenceMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Adaptive => "adaptive",
        }
    }

    fn from_code(code: &str) -> Result<Self, StoreError> {
        match code {
            "fixed" => Ok(Self::Fixed),
            "adaptive" => Ok(Self::Adaptive),
            _ => Err(StoreError::Invalid),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommunitySettings {
    pub enabled: bool,
    pub target_interval_seconds: u32,
    pub cadence_mode: CommunityCadenceMode,
}

impl Default for CommunitySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            target_interval_seconds: 300,
            cadence_mode: CommunityCadenceMode::Adaptive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityPauseReason {
    AuthenticationDenied,
    IntegrityDenied,
    ChallengeDenied,
    RateLimited,
    SourceDenied,
    CleanupUnconfirmed,
}

impl CommunityPauseReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::AuthenticationDenied => "authentication_denied",
            Self::IntegrityDenied => "integrity_denied",
            Self::ChallengeDenied => "challenge_denied",
            Self::RateLimited => "rate_limited",
            Self::SourceDenied => "source_denied",
            Self::CleanupUnconfirmed => "cleanup_unconfirmed",
        }
    }

    fn from_code(code: &str) -> Result<Self, StoreError> {
        match code {
            "authentication_denied" => Ok(Self::AuthenticationDenied),
            "integrity_denied" => Ok(Self::IntegrityDenied),
            "challenge_denied" => Ok(Self::ChallengeDenied),
            "rate_limited" => Ok(Self::RateLimited),
            "source_denied" => Ok(Self::SourceDenied),
            "cleanup_unconfirmed" => Ok(Self::CleanupUnconfirmed),
            _ => Err(StoreError::Invalid),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityRunEndReason {
    Shutdown,
    Disabled,
    SignIn,
    Paused,
    HostFailure,
    ProcessInterrupted,
}

impl CommunityRunEndReason {
    fn code(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Disabled => "disabled",
            Self::SignIn => "sign_in",
            Self::Paused => "paused",
            Self::HostFailure => "host_failure",
            Self::ProcessInterrupted => "process_interrupted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityFailureCode {
    AuthenticationDenied,
    IntegrityDenied,
    ChallengeDenied,
    RateLimited,
    HttpDenied,
    HttpError,
    RequestSizeLimit,
    ResponseSizeLimit,
    RequestStreamUnavailable,
    ResponseStreamUnavailable,
    InvalidJson,
    BatchLimit,
    AmbiguousSource,
    BatchMismatch,
    SourceErrors,
    MissingChatters,
    InvalidShape,
    MemberLimit,
    Timeout,
    NavigationFailed,
    PlayerNotPaused,
    StaleIdentity,
    HostUnavailable,
    HostFailure,
}

impl CommunityFailureCode {
    fn code(self) -> &'static str {
        match self {
            Self::AuthenticationDenied => "authentication_denied",
            Self::IntegrityDenied => "integrity_denied",
            Self::ChallengeDenied => "challenge_denied",
            Self::RateLimited => "rate_limited",
            Self::HttpDenied => "http_denied",
            Self::HttpError => "http_error",
            Self::RequestSizeLimit => "request_size_limit",
            Self::ResponseSizeLimit => "response_size_limit",
            Self::RequestStreamUnavailable => "request_stream_unavailable",
            Self::ResponseStreamUnavailable => "response_stream_unavailable",
            Self::InvalidJson => "invalid_json",
            Self::BatchLimit => "batch_limit",
            Self::AmbiguousSource => "ambiguous_source",
            Self::BatchMismatch => "batch_mismatch",
            Self::SourceErrors => "source_errors",
            Self::MissingChatters => "missing_chatters",
            Self::InvalidShape => "invalid_shape",
            Self::MemberLimit => "member_limit",
            Self::Timeout => "timeout",
            Self::NavigationFailed => "navigation_failed",
            Self::PlayerNotPaused => "player_not_paused",
            Self::StaleIdentity => "stale_identity",
            Self::HostUnavailable => "host_unavailable",
            Self::HostFailure => "host_failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityTransport {
    NetworkResponse,
    CacheOrServiceWorker,
    Unknown,
}

impl CommunityTransport {
    fn code(self) -> &'static str {
        match self {
            Self::NetworkResponse => "network_response",
            Self::CacheOrServiceWorker => "cache_or_service_worker",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommunityServerFreshness {
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommunityProvenance {
    pub request_after_trigger_verified: bool,
    pub request_after_trigger_ms: Option<f64>,
    pub request_to_response_ms: Option<f64>,
    pub request_to_finished_ms: Option<f64>,
    pub finished_minus_response_ms: Option<f64>,
    pub from_disk_cache: Option<bool>,
    pub from_service_worker: Option<bool>,
    pub from_prefetch_cache: Option<bool>,
    pub served_from_cache_event: bool,
    pub encoded_bytes: Option<u64>,
    pub transport: CommunityTransport,
    pub server_freshness: CommunityServerFreshness,
    pub http_status: u16,
}

impl CommunityProvenance {
    fn validate(&self) -> Result<(), StoreError> {
        if !self.request_after_trigger_verified
            || self.http_status != 200
            || [
                self.request_after_trigger_ms,
                self.request_to_response_ms,
                self.request_to_finished_ms,
            ]
            .into_iter()
            .flatten()
            .any(|v| !v.is_finite() || !(0.0..=MAX_TIMING_MS).contains(&v))
            || self
                .finished_minus_response_ms
                .is_some_and(|v| !v.is_finite() || v.abs() > MAX_TIMING_MS)
            || self.encoded_bytes.is_some_and(|n| n > i64::MAX as u64)
        {
            return Err(StoreError::Invalid);
        }
        let flags = [
            self.from_disk_cache,
            self.from_service_worker,
            self.from_prefetch_cache,
        ];
        let expected = if self.served_from_cache_event || flags.contains(&Some(true)) {
            CommunityTransport::CacheOrServiceWorker
        } else if flags.iter().all(|flag| *flag == Some(false))
            && self.encoded_bytes.is_some_and(|n| n > 0)
        {
            CommunityTransport::NetworkResponse
        } else {
            CommunityTransport::Unknown
        };
        if self.transport != expected {
            return Err(StoreError::Invalid);
        }
        if let (Some(response), Some(finished), Some(delta)) = (
            self.request_to_response_ms,
            self.request_to_finished_ms,
            self.finished_minus_response_ms,
        ) {
            // These are projections of the same native times; allow floating-point rounding.
            if ((finished - response) - delta).abs() > 0.001 {
                return Err(StoreError::Invalid);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommunityCategoryCounts {
    pub broadcasters: u32,
    pub staff: u32,
    pub moderators: u32,
    pub vips: u32,
    pub chatbots: u32,
    pub viewers: u32,
}

impl CommunityCategoryCounts {
    fn values(self) -> [u32; 6] {
        [
            self.broadcasters,
            self.staff,
            self.moderators,
            self.vips,
            self.chatbots,
            self.viewers,
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommunityMemberInput {
    pub account: String,
    pub roles: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NewCommunitySample {
    pub run_id: i64,
    pub job_id: u64,
    pub channel: String,
    pub started_at_utc: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommunityCompletion {
    Success {
        received_at_utc: String,
        reported_chatters: Option<u64>,
        members: Vec<CommunityMemberInput>,
        categories: CommunityCategoryCounts,
        provenance: CommunityProvenance,
    },
    Failed {
        completed_at_utc: String,
        code: CommunityFailureCode,
    },
    Cancelled {
        completed_at_utc: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommunityReceiptQuery {
    pub account: Option<String>,
    pub channel: Option<String>,
    pub from_utc: Option<String>,
    pub to_utc: Option<String>,
    pub broadcast_id: Option<i64>,
    pub before_member_id: Option<i64>,
    /// A page contains between 1 and 100 member receipts.
    pub limit: usize,
}

impl Default for CommunityReceiptQuery {
    fn default() -> Self {
        Self {
            account: None,
            channel: None,
            from_utc: None,
            to_utc: None,
            broadcast_id: None,
            before_member_id: None,
            limit: 25,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CommunityReceipt {
    pub member_id: i64,
    pub sample_id: i64,
    pub run_id: i64,
    pub job_id: u64,
    pub source: &'static str,
    pub channel: String,
    pub account: String,
    pub roles: u32,
    pub started_at_utc: String,
    pub received_at_utc: String,
    pub reported_chatters: Option<u64>,
    pub returned_accounts: u32,
    pub categories: CommunityCategoryCounts,
    pub provenance: CommunityProvenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommunityReceiptPage {
    pub rows: Vec<CommunityReceipt>,
    pub next_before_member_id: Option<i64>,
    /// None means unbounded retained history on this side, rather than an implicit recent window.
    pub from_utc: Option<String>,
    pub to_utc: Option<String>,
    pub channel: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommunityStats {
    pub schema_version: i64,
    pub run_count: i64,
    pub sample_count: i64,
    pub pending_count: i64,
    pub successful_count: i64,
    pub failed_count: i64,
    pub cancelled_count: i64,
    pub interrupted_count: i64,
    pub member_count: i64,
    pub last_received_at_utc: Option<String>,
}

// Released v1 definition: fresh databases use the same atomic evolution as existing v1 data.
const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS community_schema_version(id INTEGER PRIMARY KEY CHECK(id=1),version INTEGER NOT NULL);
INSERT OR IGNORE INTO community_schema_version VALUES(1,1);
CREATE TABLE IF NOT EXISTS community_settings(id INTEGER PRIMARY KEY CHECK(id=1),enabled INTEGER NOT NULL CHECK(enabled IN(0,1)),target_interval_seconds INTEGER NOT NULL CHECK(target_interval_seconds BETWEEN 30 AND 86400));
INSERT OR IGNORE INTO community_settings VALUES(1,0,300);
CREATE TABLE IF NOT EXISTS community_pause(id INTEGER PRIMARY KEY CHECK(id=1),reason TEXT CHECK(reason IS NULL OR reason IN('authentication_denied','integrity_denied','challenge_denied','rate_limited','source_denied')));
INSERT OR IGNORE INTO community_pause VALUES(1,NULL);
CREATE TABLE IF NOT EXISTS community_runs(id INTEGER PRIMARY KEY AUTOINCREMENT,started_at_utc TEXT NOT NULL,ended_at_utc TEXT,end_reason TEXT,build_identity TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS community_samples(
 id INTEGER PRIMARY KEY AUTOINCREMENT,run_id INTEGER NOT NULL REFERENCES community_runs(id),job_id INTEGER NOT NULL CHECK(job_id>0),channel TEXT NOT NULL,started_at_utc TEXT NOT NULL,completed_at_utc TEXT,received_at_utc TEXT,
 outcome TEXT NOT NULL CHECK(outcome IN('pending','success','failed','cancelled','interrupted')),failure_code TEXT,reported_chatters INTEGER,returned_accounts INTEGER,
 broadcasters INTEGER,staff INTEGER,moderators INTEGER,vips INTEGER,chatbots INTEGER,viewers INTEGER,
 request_after_trigger_verified INTEGER,request_after_trigger_ms REAL,request_to_response_ms REAL,request_to_finished_ms REAL,finished_minus_response_ms REAL,
 from_disk_cache INTEGER,from_service_worker INTEGER,from_prefetch_cache INTEGER,served_from_cache_event INTEGER,encoded_bytes INTEGER,transport TEXT,server_freshness TEXT,http_status INTEGER,
 UNIQUE(run_id,job_id));
CREATE INDEX IF NOT EXISTS community_samples_channel_receipt ON community_samples(channel,received_at_utc,id);
CREATE INDEX IF NOT EXISTS community_samples_receipt ON community_samples(received_at_utc,id);
CREATE INDEX IF NOT EXISTS community_samples_outcome ON community_samples(outcome,id);
CREATE TABLE IF NOT EXISTS community_members(id INTEGER PRIMARY KEY AUTOINCREMENT,sample_id INTEGER NOT NULL REFERENCES community_samples(id),channel TEXT NOT NULL,account TEXT NOT NULL,roles INTEGER NOT NULL CHECK(roles BETWEEN 1 AND 63),UNIQUE(sample_id,account));
CREATE INDEX IF NOT EXISTS community_members_account_member_sample ON community_members(account,id,sample_id);
CREATE INDEX IF NOT EXISTS community_members_channel_member ON community_members(channel,id);
CREATE INDEX IF NOT EXISTS community_members_sample_member ON community_members(sample_id,id);
CREATE TRIGGER IF NOT EXISTS community_members_success_only BEFORE INSERT ON community_members WHEN NOT EXISTS(SELECT 1 FROM community_samples WHERE id=NEW.sample_id AND outcome='success' AND channel=NEW.channel) BEGIN SELECT RAISE(ABORT,'community member requires a matching successful sample'); END;
";

const MIGRATE_V2: &str = "
ALTER TABLE community_settings ADD COLUMN cadence_mode TEXT NOT NULL DEFAULT 'fixed' CHECK(cadence_mode IN('fixed','adaptive'));
UPDATE community_schema_version SET version=2 WHERE id=1;
";

// Preserve released reason values verbatim. A legacy generic pause has no
// recoverable causal detail and must never be inferred to mean cleanup failure.
const MIGRATE_V3: &str = "
CREATE TABLE community_pause_v3(id INTEGER PRIMARY KEY CHECK(id=1),reason TEXT CHECK(reason IS NULL OR reason IN('authentication_denied','integrity_denied','challenge_denied','rate_limited','source_denied','cleanup_unconfirmed')));
INSERT INTO community_pause_v3(id,reason) SELECT id,reason FROM community_pause;
DROP TABLE community_pause;
ALTER TABLE community_pause_v3 RENAME TO community_pause;
UPDATE community_schema_version SET version=3 WHERE id=1;
";

fn validate_v3_pause_schema(conn: &Connection) -> Result<(), StoreError> {
    let sql: String = conn
        .query_row(
            "SELECT sql FROM main.sqlite_schema WHERE type='table' AND name='community_pause'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Initialize)?;
    let normalize = |sql: &str| {
        sql.chars()
            .filter(|ch| !ch.is_ascii_whitespace() && *ch != '"')
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let expected = "CREATE TABLE community_pause(id INTEGER PRIMARY KEY CHECK(id=1),reason TEXT CHECK(reason IS NULL OR reason IN('authentication_denied','integrity_denied','challenge_denied','rate_limited','source_denied','cleanup_unconfirmed')))";
    if normalize(&sql) != normalize(expected) {
        return Err(StoreError::Initialize);
    }
    Ok(())
}

// Only the released layout and its compatible covering extension are ours to evolve.
// Validate metadata rather than SQL spelling, including auxiliary rowid information.
fn sample_member_index_needs_covering(conn: &Connection) -> Result<bool, StoreError> {
    let owner: Option<(String, String, bool)> = conn
        .query_row(
            "SELECT type,tbl_name,sql IS NOT NULL FROM main.sqlite_schema WHERE name='community_members_sample_member'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if !matches!(owner, Some((kind, table, true)) if kind == "index" && table == "community_members")
    {
        return Err(StoreError::Initialize);
    }
    let flags: Option<(bool, String, bool)> = conn
        .query_row(
            "SELECT \"unique\",origin,partial FROM pragma_index_list('community_members','main') WHERE name='community_members_sample_member'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if !matches!(flags, Some((false, origin, false)) if origin == "c") {
        return Err(StoreError::Initialize);
    }
    let columns = conn
        .prepare("PRAGMA main.index_xinfo(community_members_sample_member)")?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let expected: &[(i64, Option<&str>, i64)] = match columns.len() {
        3 => &[(1, Some("sample_id"), 1), (0, Some("id"), 1), (-1, None, 0)],
        4 => &[
            (1, Some("sample_id"), 1),
            (0, Some("id"), 1),
            (4, Some("roles"), 1),
            (-1, None, 0),
        ],
        _ => return Err(StoreError::Initialize),
    };
    if columns.iter().zip(expected).enumerate().any(
        |(position, ((seqno, cid, name, descending, collation, key), expected))| {
            *seqno != position as i64
                || *cid != expected.0
                || name.as_deref() != expected.1
                || *descending != 0
                || collation.as_deref() != Some("BINARY")
                || *key != expected.2
        },
    ) {
        return Err(StoreError::Initialize);
    }
    Ok(columns.len() == 3)
}

/// Runs on the storage worker without a query progress handler. Creation and recovery commit together.
pub(super) fn initialize(conn: &mut Connection) -> Result<(), StoreError> {
    let has_version: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='community_schema_version')", [], |r| r.get(0))?;
    let version = if has_version {
        let versions = (|| -> rusqlite::Result<Vec<(i64, i64)>> {
            conn.prepare("SELECT id,version FROM community_schema_version")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })()
        .map_err(|_| StoreError::Initialize)?;
        match versions.as_slice() {
            [(1, version)] if matches!(*version, 1 | 2 | COMMUNITY_SCHEMA_VERSION) => {
                Some(*version)
            }
            _ => return Err(StoreError::Initialize),
        }
    } else {
        let retained: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name GLOB 'community_*')",
            [],
            |r| r.get(0),
        )?;
        if retained {
            return Err(StoreError::Initialize);
        }
        None
    };
    let tx = conn.transaction()?;
    if version.is_none() {
        tx.execute_batch(SCHEMA_V1)?;
    }
    // Reject unknown retained definitions before changing any access paths or recovering rows.
    let extend_sample_member_index = sample_member_index_needs_covering(&tx)?;
    if version.is_none_or(|version| version == 1) {
        tx.execute_batch(MIGRATE_V2)?;
        if version.is_none() {
            tx.execute(
                "UPDATE community_settings SET cadence_mode='adaptive' WHERE id=1",
                [],
            )?;
        }
    }
    // Invalid retained configuration must not silently change cadence, clear a pause,
    // or perform recovery. A failure rolls back schema and version changes as well.
    read_community_settings(&tx).map_err(|_| StoreError::Initialize)?;
    read_community_pause(&tx).map_err(|_| StoreError::Initialize)?;
    if version.is_none_or(|version| version < 3) {
        tx.execute_batch(MIGRATE_V3)?;
        read_community_settings(&tx).map_err(|_| StoreError::Initialize)?;
        read_community_pause(&tx).map_err(|_| StoreError::Initialize)?;
    }
    validate_v3_pause_schema(&tx)?;
    if extend_sample_member_index {
        // The key prefix and index name remain compatible with existing readers.
        tx.execute_batch(
            "DROP INDEX community_members_sample_member;
            CREATE INDEX community_members_sample_member ON community_members(sample_id,id,roles);",
        )?;
    }
    // This non-unique access path changes no retained data or logical schema contract.
    // Add it atomically for fresh and retained databases without rewriting released DDL.
    tx.execute_batch("CREATE INDEX IF NOT EXISTS community_members_account_channel_member_sample ON community_members(account,channel,id,sample_id);")?;
    let at = now();
    tx.execute("UPDATE community_samples SET outcome='interrupted',failure_code='process_interrupted',completed_at_utc=?1 WHERE outcome='pending'", [&at])?;
    tx.execute("UPDATE community_runs SET ended_at_utc=?1,end_reason='process_interrupted' WHERE ended_at_utc IS NULL", [&at])?;
    tx.commit()?;
    Ok(())
}

fn read_community_settings(conn: &Connection) -> Result<CommunitySettings, StoreError> {
    let (enabled, interval, mode): (i64, i64, String) = conn.query_row(
        "SELECT enabled,target_interval_seconds,cadence_mode FROM community_settings WHERE id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if !matches!(enabled, 0 | 1) || !(30..=86400).contains(&interval) {
        return Err(StoreError::Invalid);
    }
    Ok(CommunitySettings {
        enabled: enabled == 1,
        target_interval_seconds: interval as u32,
        cadence_mode: CommunityCadenceMode::from_code(&mode)?,
    })
}

fn read_community_pause(conn: &Connection) -> Result<Option<CommunityPauseReason>, StoreError> {
    let code: Option<String> =
        conn.query_row("SELECT reason FROM community_pause WHERE id=1", [], |row| {
            row.get(0)
        })?;
    code.as_deref()
        .map(CommunityPauseReason::from_code)
        .transpose()
}

fn stamp(value: &str) -> Result<String, StoreError> {
    let normalized = crate::history::utc(value)?;
    if normalized.len() != 24 || !normalized.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
        return Err(StoreError::Invalid);
    }
    Ok(normalized)
}

impl Store {
    pub async fn community_pause(&self) -> Result<Option<CommunityPauseReason>, StoreError> {
        self.call_read(Operation::Query, |conn| read_community_pause(conn))
            .await
    }

    /// Only an explicit None clears the durable pause; enabled preferences are independent.
    pub async fn set_community_pause(
        &self,
        reason: Option<CommunityPauseReason>,
    ) -> Result<(), StoreError> {
        self.call_write(Operation::CommunityAppend, move |conn| {
            if conn.execute(
                "UPDATE community_pause SET reason=?1 WHERE id=1",
                [reason.map(CommunityPauseReason::code)],
            )? != 1
            {
                return Err(StoreError::Invalid);
            }
            Ok(())
        })
        .await
    }

    /// Members are independent positive receipts, ordered by their own source cursor.
    /// Omitted time bounds mean all retained history; they never silently become a recent window.
    pub async fn community_receipts(
        &self,
        query: CommunityReceiptQuery,
    ) -> Result<CommunityReceiptPage, StoreError> {
        let account = query.account.as_deref().map(normalize).transpose()?;
        let mut channel = query.channel.as_deref().map(normalize).transpose()?;
        let mut from = query.from_utc.as_deref().map(stamp).transpose()?;
        let mut to = query.to_utc.as_deref().map(stamp).transpose()?;
        if !(1..=100).contains(&query.limit)
            || query.before_member_id.is_some_and(|id| id <= 0)
            || query.broadcast_id.is_some_and(|id| id <= 0)
            || matches!((&from,&to),(Some(from),Some(to)) if from>=to)
        {
            return Err(StoreError::Invalid);
        }
        self.call_read(Operation::Query, move |conn| {
            if let Some(id) = query.broadcast_id {
                let has_metadata: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='metadata_broadcasts')", [], |r|r.get(0))?;
                if !has_metadata { return Err(StoreError::NotFound); }
                let (broadcast_channel,start,end): (String,String,String) = conn.query_row("SELECT channel,started_at,last_live_at FROM metadata_broadcasts WHERE id=?1", [id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(StoreError::NotFound)?;
                let broadcast_channel = normalize(&broadcast_channel)?;
                if channel.as_ref().is_some_and(|channel| channel!=&broadcast_channel) { return Err(StoreError::Invalid); }
                channel = Some(broadcast_channel);
                let start = stamp(&start)?;
                let end = stamp(&end)?;
                if start>end { return Err(StoreError::Invalid); }
                let end = DateTime::parse_from_rfc3339(&end).map_err(|_|StoreError::Invalid)?.with_timezone(&Utc).checked_add_signed(Duration::milliseconds(1)).ok_or(StoreError::Invalid)?;
                let end = stamp(&end.to_rfc3339_opts(SecondsFormat::Millis,true))?;
                from = Some(from.map_or(start.clone(),|from|from.max(start)));
                to = Some(to.map_or(end.clone(),|to|to.min(end)));
            }
            let mut page = CommunityReceiptPage { rows:Vec::new(),next_before_member_id:None,from_utc:from.clone(),to_utc:to.clone(),channel:channel.clone(),limit:query.limit };
            if matches!((&from,&to),(Some(from),Some(to)) if from>=to) { return Ok(page); }
            let sql = receipt_sql(account.is_some(),channel.is_some(),from.is_some(),to.is_some());
            let mut statement = conn.prepare_cached(&sql)?;
            let rows = statement.query_map(params![account,channel,from,to,query.before_member_id.unwrap_or(i64::MAX),(query.limit+1) as i64], receipt_row)?;
            page.rows = rows.collect::<Result<Vec<_>,_>>()?;
            if page.rows.len()>query.limit {
                page.rows.truncate(query.limit);
                page.next_before_member_id = page.rows.last().map(|row|row.member_id);
            }
            Ok(page)
        }).await
    }

    pub async fn community_settings(&self) -> Result<CommunitySettings, StoreError> {
        self.call_read(Operation::Query, |conn| read_community_settings(conn))
            .await
    }

    pub async fn set_community_settings(
        &self,
        settings: CommunitySettings,
    ) -> Result<(), StoreError> {
        if !(30..=86400).contains(&settings.target_interval_seconds) {
            return Err(StoreError::Invalid);
        }
        self.call_write(Operation::CommunityAppend, move |conn| {
            conn.execute(
                "UPDATE community_settings SET enabled=?1,target_interval_seconds=?2,cadence_mode=?3 WHERE id=1",
                params![settings.enabled, settings.target_interval_seconds,settings.cadence_mode.code()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn begin_community_run(&self, build_identity: String) -> Result<i64, StoreError> {
        if build_identity.is_empty()
            || build_identity.len() > 128
            || !build_identity
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        {
            return Err(StoreError::Invalid);
        }
        self.call_write(Operation::CommunityAppend, move |conn| {
            conn.execute(
                "INSERT INTO community_runs(started_at_utc,build_identity) VALUES(?1,?2)",
                params![now(), build_identity],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    pub async fn end_community_run(
        &self,
        run_id: i64,
        reason: CommunityRunEndReason,
    ) -> Result<(), StoreError> {
        if run_id <= 0 {
            return Err(StoreError::Invalid);
        }
        self.call_write(Operation::CommunityAppend, move |conn| {
            let tx = conn.transaction()?;
            let at = now();
            if tx.execute("UPDATE community_runs SET ended_at_utc=?1,end_reason=?2 WHERE id=?3 AND ended_at_utc IS NULL", params![at,reason.code(),run_id])? != 1 { return Err(StoreError::Invalid); }
            let (outcome,code) = if reason==CommunityRunEndReason::ProcessInterrupted { ("interrupted","process_interrupted") } else { ("cancelled","cancelled") };
            tx.execute("UPDATE community_samples SET outcome=?3,failure_code=?4,completed_at_utc=?1 WHERE run_id=?2 AND outcome='pending'", params![at,run_id,outcome,code])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    /// This durable acknowledgement must precede dispatch to the native host.
    pub async fn begin_community_sample(
        &self,
        sample: NewCommunitySample,
    ) -> Result<i64, StoreError> {
        if sample.run_id <= 0 || sample.job_id == 0 || sample.job_id > i64::MAX as u64 {
            return Err(StoreError::Invalid);
        }
        let channel = normalize(&sample.channel)?;
        let started_at = stamp(&sample.started_at_utc)?;
        self.call_write(Operation::CommunityAppend, move |conn| {
            let changed = conn.execute("INSERT INTO community_samples(run_id,job_id,channel,started_at_utc,outcome) SELECT ?1,?2,?3,?4,'pending' WHERE EXISTS(SELECT 1 FROM community_runs WHERE id=?1 AND ended_at_utc IS NULL)", params![sample.run_id,sample.job_id as i64,channel,started_at])?;
            if changed != 1 { return Err(StoreError::Invalid); }
            Ok(conn.last_insert_rowid())
        }).await
    }

    /// A pending sample becomes terminal once. Members and success metadata share one transaction.
    pub async fn complete_community_sample(
        &self,
        sample_id: i64,
        completion: CommunityCompletion,
    ) -> Result<(), StoreError> {
        if sample_id <= 0 {
            return Err(StoreError::Invalid);
        }
        let (completed_at, success) = match &completion {
            CommunityCompletion::Success {
                received_at_utc,
                reported_chatters,
                members,
                categories,
                provenance,
            } => {
                provenance.validate()?;
                if reported_chatters.is_some_and(|n| n > i64::MAX as u64)
                    || members.len() > COMMUNITY_MAX_MEMBERS
                    || categories.values().into_iter().map(u64::from).sum::<u64>()
                        > COMMUNITY_MAX_MEMBERS as u64
                {
                    return Err(StoreError::Invalid);
                }
                let mut normalized = BTreeMap::<String, u32>::new();
                for member in members {
                    if member.roles == 0 || member.roles & !ROLE_MASK != 0 {
                        return Err(StoreError::Invalid);
                    }
                    *normalized.entry(normalize(&member.account)?).or_default() |= member.roles;
                }
                for (bit, reported) in categories.values().into_iter().enumerate() {
                    if normalized
                        .values()
                        .filter(|roles| **roles & (1 << bit) != 0)
                        .count()
                        > reported as usize
                    {
                        return Err(StoreError::Invalid);
                    }
                }
                (stamp(received_at_utc)?, Some(normalized))
            }
            CommunityCompletion::Failed {
                completed_at_utc, ..
            }
            | CommunityCompletion::Cancelled { completed_at_utc } => {
                (stamp(completed_at_utc)?, None)
            }
        };
        self.call_write(Operation::CommunityAppend, move |conn| {
            let tx = conn.transaction()?;
            let pending: Option<(String,String)> = tx.query_row("SELECT started_at_utc,channel FROM community_samples WHERE id=?1 AND outcome='pending' AND EXISTS(SELECT 1 FROM community_runs WHERE id=community_samples.run_id AND ended_at_utc IS NULL)", [sample_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            let (started,channel) = pending.ok_or(StoreError::Invalid)?;
            if completed_at<started { return Err(StoreError::Invalid); }
            match completion {
                CommunityCompletion::Success { reported_chatters, categories:c, provenance:p, .. } => {
                    let members = success.expect("validated success");
                    tx.execute("UPDATE community_samples SET outcome='success',completed_at_utc=?2,received_at_utc=?2,reported_chatters=?3,returned_accounts=?4,broadcasters=?5,staff=?6,moderators=?7,vips=?8,chatbots=?9,viewers=?10,request_after_trigger_verified=?11,request_after_trigger_ms=?12,request_to_response_ms=?13,request_to_finished_ms=?14,finished_minus_response_ms=?15,from_disk_cache=?16,from_service_worker=?17,from_prefetch_cache=?18,served_from_cache_event=?19,encoded_bytes=?20,transport=?21,server_freshness='unknown',http_status=?22 WHERE id=?1", params![sample_id,completed_at,reported_chatters.map(|n|n as i64),members.len() as i64,c.broadcasters,c.staff,c.moderators,c.vips,c.chatbots,c.viewers,p.request_after_trigger_verified,p.request_after_trigger_ms,p.request_to_response_ms,p.request_to_finished_ms,p.finished_minus_response_ms,p.from_disk_cache,p.from_service_worker,p.from_prefetch_cache,p.served_from_cache_event,p.encoded_bytes.map(|n|n as i64),p.transport.code(),p.http_status])?;
                    {
                        let mut insert = tx.prepare_cached("INSERT INTO community_members(sample_id,channel,account,roles) VALUES(?1,?2,?3,?4)")?;
                        for (account, roles) in members { insert.execute(params![sample_id,channel,account,roles])?; }
                    }
                }
                CommunityCompletion::Failed { code, .. } => { tx.execute("UPDATE community_samples SET outcome='failed',completed_at_utc=?2,failure_code=?3 WHERE id=?1", params![sample_id,completed_at,code.code()])?; }
                CommunityCompletion::Cancelled { .. } => { tx.execute("UPDATE community_samples SET outcome='cancelled',completed_at_utc=?2,failure_code='cancelled' WHERE id=?1", params![sample_id,completed_at])?; }
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn community_stats(&self) -> Result<CommunityStats, StoreError> {
        self.call_read(Operation::Query, |conn| {
            Ok(conn.query_row("SELECT (SELECT COUNT(*) FROM community_runs),COUNT(*),COALESCE(SUM(outcome='pending'),0),COALESCE(SUM(outcome='success'),0),COALESCE(SUM(outcome='failed'),0),COALESCE(SUM(outcome='cancelled'),0),COALESCE(SUM(outcome='interrupted'),0),(SELECT COUNT(*) FROM community_members),MAX(received_at_utc) FROM community_samples", [], |r| Ok(CommunityStats { schema_version:COMMUNITY_SCHEMA_VERSION,run_count:r.get(0)?,sample_count:r.get(1)?,pending_count:r.get(2)?,successful_count:r.get(3)?,failed_count:r.get(4)?,cancelled_count:r.get(5)?,interrupted_count:r.get(6)?,member_count:r.get(7)?,last_received_at_utc:r.get(8)? }))?)
        }).await
    }
}

// Choose a seekable source rather than OR-ing optional account/channel predicates.
// Both account and channel pagination seek directly below the independent member cursor.
// Sample metadata, including optional time bounds, is checked after each member seek.
fn receipt_sql(account: bool, channel: bool, from: bool, to: bool) -> String {
    let source = if account && channel {
        "community_members m INDEXED BY community_members_account_channel_member_sample CROSS JOIN community_samples s ON s.id=m.sample_id"
    } else if account {
        "community_members m INDEXED BY community_members_account_member_sample JOIN community_samples s ON s.id=m.sample_id"
    } else if channel {
        "community_members m INDEXED BY community_members_channel_member CROSS JOIN community_samples s ON s.id=m.sample_id"
    } else {
        "community_members m JOIN community_samples s ON s.id=m.sample_id"
    };
    let mut sql = format!("SELECT m.id,s.id,s.run_id,s.job_id,s.channel,m.account,m.roles,s.started_at_utc,s.received_at_utc,s.reported_chatters,s.returned_accounts,s.broadcasters,s.staff,s.moderators,s.vips,s.chatbots,s.viewers,s.request_after_trigger_verified,s.request_after_trigger_ms,s.request_to_response_ms,s.request_to_finished_ms,s.finished_minus_response_ms,s.from_disk_cache,s.from_service_worker,s.from_prefetch_cache,s.served_from_cache_event,s.encoded_bytes,s.transport,s.http_status FROM {source} WHERE s.outcome='success' AND m.id<?5");
    if account {
        sql.push_str(" AND m.account=?1");
    } else {
        sql.push_str(" AND ?1 IS NULL");
    }
    if channel {
        sql.push_str(" AND m.channel=?2");
    } else {
        sql.push_str(" AND ?2 IS NULL");
    }
    if from {
        sql.push_str(" AND s.received_at_utc>=?3");
    } else {
        sql.push_str(" AND ?3 IS NULL");
    }
    if to {
        sql.push_str(" AND s.received_at_utc<?4");
    } else {
        sql.push_str(" AND ?4 IS NULL");
    }
    sql.push_str(" ORDER BY m.id DESC LIMIT ?6");
    sql
}

fn receipt_row(r: &rusqlite::Row<'_>) -> Result<CommunityReceipt, rusqlite::Error> {
    let transport: String = r.get(27)?;
    let transport = match transport.as_str() {
        "network_response" => CommunityTransport::NetworkResponse,
        "cache_or_service_worker" => CommunityTransport::CacheOrServiceWorker,
        "unknown" => CommunityTransport::Unknown,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(CommunityReceipt {
        member_id: r.get(0)?,
        sample_id: r.get(1)?,
        run_id: r.get(2)?,
        job_id: r.get::<_, i64>(3)? as u64,
        source: "community",
        channel: r.get(4)?,
        account: r.get(5)?,
        roles: r.get(6)?,
        started_at_utc: r.get(7)?,
        received_at_utc: r.get(8)?,
        reported_chatters: r.get::<_, Option<i64>>(9)?.map(|n| n as u64),
        returned_accounts: r.get(10)?,
        categories: CommunityCategoryCounts {
            broadcasters: r.get(11)?,
            staff: r.get(12)?,
            moderators: r.get(13)?,
            vips: r.get(14)?,
            chatbots: r.get(15)?,
            viewers: r.get(16)?,
        },
        provenance: CommunityProvenance {
            request_after_trigger_verified: r.get(17)?,
            request_after_trigger_ms: r.get(18)?,
            request_to_response_ms: r.get(19)?,
            request_to_finished_ms: r.get(20)?,
            finished_minus_response_ms: r.get(21)?,
            from_disk_cache: r.get(22)?,
            from_service_worker: r.get(23)?,
            from_prefetch_cache: r.get(24)?,
            served_from_cache_event: r.get(25)?,
            encoded_bytes: r.get::<_, Option<i64>>(26)?.map(|n| n as u64),
            transport,
            server_freshness: CommunityServerFreshness::Unknown,
            http_status: r.get(28)?,
        },
    })
}

#[cfg(test)]
#[path = "store_community_tests.rs"]
mod tests;
