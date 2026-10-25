//! Serialized SQLite access. Account logins are observations, not permanent account IDs.
use crate::diagnostics::{Diagnostics, JobClass, Operation};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

#[path = "store_suggestions.rs"]
mod suggestions;
pub use suggestions::SuggestionKind;

// Checkpoint less often than SQLite's default 1000 frames: the 50-room retained-history
// workload measured durable database-file flushes dominating commit time on Windows.
// At the normal 4096-byte page size this targets about 32 MiB of active WAL. FULL sync
// still durably commits each flush. The recycled-file limit applies on WAL reset; a
// large transaction or an external reader can temporarily exceed it.
// https://www.sqlite.org/pragma.html#pragma_wal_autocheckpoint
// https://www.sqlite.org/pragma.html#pragma_journal_size_limit
const WAL_CHECKPOINT_PAGES: i64 = 8192;
const WAL_RETAINED_LIMIT_BYTES: i64 = 64 * 1024 * 1024;

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "store_diagnostics_tests.rs"]
mod diagnostics_tests;

#[cfg(test)]
#[path = "store_batch_close_tests.rs"]
mod batch_close_tests;

#[cfg(test)]
#[path = "startup_schema_tests.rs"]
mod startup_schema_tests;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("storage worker is unavailable")]
    Unavailable,
    #[error("invalid channel or query configuration")]
    Invalid,
    #[error("at most 50 channels can be tracked")]
    ChannelLimit,
    #[error("channel was not found")]
    NotFound,
    #[error("database initialization failed")]
    Initialize,
    #[error("WAL checkpoint is blocked by another database reader or writer")]
    CheckpointBusy,
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;
#[derive(Clone)]
pub struct Store {
    sender: mpsc::Sender<Job>,
    path: Arc<PathBuf>,
    diagnostics: Diagnostics,
    #[cfg(test)]
    append_expiry: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
    #[cfg(test)]
    query_expiry: Arc<std::sync::Mutex<Option<Arc<AtomicBool>>>>,
    #[cfg(test)]
    append_interrupt: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub login: String,
    pub tracked: bool,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    pub active_session_id: Option<i64>,
    pub last_observed_at: Option<String>,
    pub collection_state: Option<String>,
    pub collection_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    pub id: i64,
    pub session_id: i64,
    pub channel: String,
    pub account: Option<String>,
    pub kind: String,
    pub received_at: String,
    pub received_at_utc: Option<String>,
    pub source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: i64,
    pub channel: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub end_reason: Option<String>,
    pub observation_count: i64,
    pub end_uncertain: bool,
    pub closed_recorded_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Relationship {
    pub channel: String,
    pub account: String,
    pub first_observed_at: String,
    pub last_observed_at: String,
    pub observation_count: i64,
}

#[derive(Debug, Clone)]
pub struct NewObservation {
    pub channel: String,
    pub account: Option<String>,
    pub kind: String,
    pub received_at: String,
}

#[derive(Debug, Clone)]
pub struct SessionEnd {
    pub session_id: i64,
    pub reason: String,
    /// The captured transport boundary, or an uncertain retained-receipt fallback when absent.
    pub source_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StorageStats {
    pub observation_count: i64,
    pub session_count: i64,
    pub storage_bytes: u64,
}

pub fn normalize(value: &str) -> Result<String, StoreError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 25
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(StoreError::Invalid);
    }
    Ok(value.to_ascii_lowercase())
}

fn close_session_in_transaction(
    conn: &Connection,
    id: i64,
    reason: &str,
    source_at: Option<String>,
    uncertain: bool,
) -> Result<(), StoreError> {
    let at = match source_at {
        Some(at) => at,
        None => {
            let retained:Option<String>=conn.query_row("SELECT COALESCE((SELECT MAX(received_at_utc) FROM observations WHERE session_id=?1),started_at) FROM sessions WHERE id=?1",[id],|row|row.get(0)).optional()?;
            let Some(retained) = retained else {
                return Ok(());
            };
            retained
        }
    };
    let changed=conn.execute(
        "UPDATE sessions SET ended_at=?1,closed_recorded_at=?4,end_reason=?2,end_uncertain=MAX(end_uncertain,?5 OR ?1<started_at OR EXISTS(SELECT 1 FROM history_visits v WHERE v.session_id=sessions.id AND v.end_basis IS NULL AND v.last_observed_at>?1) OR EXISTS(SELECT 1 FROM history_visits v WHERE v.session_id=sessions.id AND v.end_basis='coverage_gap' AND v.closed_at>?1 AND v.last_observed_at>?1)) WHERE id=?3 AND ended_at IS NULL",
        params![at,reason,id,now(),uncertain],
    )?;
    if changed > 0 {
        conn.execute("UPDATE history_visits SET end_basis='session_end',end_reason=?1,closed_at=?2,out_of_order=MAX(out_of_order,?2<last_observed_at OR ?2<(SELECT started_at FROM sessions WHERE id=?3)) WHERE session_id=?3 AND end_basis IS NULL",params![reason,at,id])?;
        conn.execute("UPDATE history_visits SET closed_at=?1,out_of_order=MAX(out_of_order,?1<last_observed_at OR ?1<(SELECT started_at FROM sessions WHERE id=?2)) WHERE session_id=?2 AND end_basis='coverage_gap' AND closed_at>?1",params![at,id])?;
    }
    Ok(())
}

fn initialize_schema(conn: &mut Connection) -> Result<(), StoreError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > 1 {
        return Err(StoreError::Initialize);
    }
    crate::history::checked_model_version(conn)?;
    // Keep durable schema creation and the legacy relationship import in one commit.
    // Connection PRAGMAs and history recomputation deliberately run outside this transaction.
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS channels(login TEXT PRIMARY KEY,tracked INTEGER NOT NULL,enabled INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
                    CREATE TABLE IF NOT EXISTS sessions(id INTEGER PRIMARY KEY AUTOINCREMENT,started_at TEXT NOT NULL,ended_at TEXT,end_reason TEXT,end_uncertain INTEGER NOT NULL DEFAULT 0,closed_recorded_at TEXT);
                    CREATE TABLE IF NOT EXISTS session_channels(session_id INTEGER NOT NULL REFERENCES sessions(id),channel TEXT NOT NULL REFERENCES channels(login),collection_state TEXT NOT NULL DEFAULT 'joining',collection_error TEXT,PRIMARY KEY(session_id,channel));
                    CREATE TABLE IF NOT EXISTS observations(id INTEGER PRIMARY KEY AUTOINCREMENT,session_id INTEGER NOT NULL,channel TEXT NOT NULL,account TEXT,kind TEXT NOT NULL CHECK(kind IN ('names','join','part','names_end')),received_at TEXT NOT NULL,FOREIGN KEY(session_id,channel) REFERENCES session_channels(session_id,channel));
                    CREATE INDEX IF NOT EXISTS observations_channel_id ON observations(channel,id);
                    CREATE INDEX IF NOT EXISTS observations_account_id ON observations(account,id);
                    CREATE INDEX IF NOT EXISTS observations_account_channel_id ON observations(account,channel,id);
                    CREATE INDEX IF NOT EXISTS observations_channel_time ON observations(channel,received_at,id);
                    CREATE INDEX IF NOT EXISTS observations_session ON observations(session_id,channel);
                    CREATE INDEX IF NOT EXISTS observations_relationship ON observations(channel,account,received_at);
                    CREATE INDEX IF NOT EXISTS session_channels_channel ON session_channels(channel,session_id);
                    CREATE TABLE IF NOT EXISTS collection_totals(id INTEGER PRIMARY KEY CHECK(id=1),observation_count INTEGER NOT NULL,session_count INTEGER NOT NULL);
                    INSERT OR IGNORE INTO collection_totals SELECT 1,(SELECT COUNT(*) FROM observations),(SELECT COUNT(*) FROM sessions);
                    CREATE TABLE IF NOT EXISTS relationships(channel TEXT NOT NULL,account TEXT NOT NULL,first_observed_at TEXT NOT NULL,last_observed_at TEXT NOT NULL,observation_count INTEGER NOT NULL,PRIMARY KEY(channel,account));
                    CREATE INDEX IF NOT EXISTS relationships_account ON relationships(account,channel);")?;
    if version == 0 {
        tx.execute_batch("INSERT OR IGNORE INTO relationships SELECT channel,account,MIN(received_at),MAX(received_at),COUNT(*) FROM observations WHERE account IS NOT NULL GROUP BY channel,account; PRAGMA user_version=1;")?;
    }
    tx.commit()?;
    Ok(())
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let diagnostics = Diagnostics::new(path.as_ref());
        Self::open_with_diagnostics(path, diagnostics).await
    }

    pub async fn open_with_diagnostics(
        path: impl AsRef<Path>,
        diagnostics: Diagnostics,
    ) -> Result<Self, StoreError> {
        let path = path.as_ref().to_owned();
        let (sender, mut receiver) = mpsc::channel::<Job>(64);
        let (ready_tx, ready_rx) = oneshot::channel();
        let worker_path = path.clone();
        let worker_diagnostics = diagnostics.clone();
        let initializing = std::time::Instant::now();
        std::thread::Builder::new().name("veylurk-storage".into()).spawn(move || {
            let init = || -> Result<(Connection,std::fs::File), StoreError> {
                if let Some(parent) = worker_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|_| StoreError::Initialize)?;
                }
                let lock_path=PathBuf::from(format!("{}.lock",worker_path.display()));
                let lock=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(|_|StoreError::Initialize)?;
                lock.try_lock().map_err(|_|StoreError::Initialize)?;
                let mut conn = Connection::open(&worker_path)?;
                conn.busy_timeout(Duration::from_secs(5))?;
                conn.pragma_update(None,"wal_autocheckpoint",WAL_CHECKPOINT_PAGES)?;
                conn.pragma_update(None,"journal_size_limit",WAL_RETAINED_LIMIT_BYTES)?;
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
                initialize_schema(&mut conn)?;
                crate::history::initialize(&mut conn)?;
                // A process restart closes any session that could not record its own end.
                conn.execute("UPDATE sessions SET ended_at=COALESCE((SELECT MAX(received_at_utc) FROM observations WHERE session_id=sessions.id),started_at),end_reason='process_interrupted',end_uncertain=1,closed_recorded_at=?1 WHERE ended_at IS NULL", [now()])?;
                conn.execute("UPDATE history_visits SET end_basis='session_end',end_reason='process_interrupted',closed_at=(SELECT ended_at FROM sessions WHERE id=history_visits.session_id) WHERE end_basis IS NULL AND EXISTS(SELECT 1 FROM sessions WHERE id=history_visits.session_id AND ended_at IS NOT NULL)",[])?;
                Ok((conn,lock))
            };
            match init() {
                Ok((mut conn,_lock)) => {
                    if ready_tx.send(Ok(())).is_err() { return; }
                    while let Some(job) = receiver.blocking_recv() {
                        job(&mut conn);
                    }
                    if let Err(error) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                        worker_diagnostics.storage_failure(Operation::Checkpoint, JobClass::DurableMutation, &error.into(), Duration::ZERO, false, Some(conn.is_autocommit()));
                    }
                }
                Err(error) => {
                    worker_diagnostics.storage_failure(Operation::Initialize, JobClass::DurableMutation, &error, initializing.elapsed(), false, None);
                    let _ = ready_tx.send(Err(error));
                }
            }
        }).map_err(|_| {
            diagnostics.storage_failure(Operation::Initialize, JobClass::DurableMutation, &StoreError::Initialize, initializing.elapsed(), false, None);
            StoreError::Initialize
        })?;
        ready_rx.await.map_err(|_| {
            diagnostics.storage_failure(
                Operation::Initialize,
                JobClass::DurableMutation,
                &StoreError::Unavailable,
                initializing.elapsed(),
                false,
                None,
            );
            StoreError::Unavailable
        })??;
        Ok(Self {
            sender,
            path: Arc::new(path),
            diagnostics,
            #[cfg(test)]
            append_expiry: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            query_expiry: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            append_interrupt: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn diagnostics(&self) -> crate::diagnostics::Snapshot {
        self.diagnostics.snapshot()
    }

    pub(crate) fn record_failure(&self, operation: Operation, error: &StoreError) {
        self.diagnostics.storage_failure(
            operation,
            JobClass::DurableMutation,
            error,
            Duration::ZERO,
            false,
            None,
        );
    }

    #[cfg(test)]
    pub(crate) fn expire_next_append_for_test(&self) -> Arc<AtomicBool> {
        let clock = Arc::new(AtomicBool::new(false));
        *self.append_expiry.lock().unwrap() = Some(clock.clone());
        clock
    }

    #[cfg(test)]
    pub(crate) fn interrupt_next_append_for_test(&self) {
        self.append_interrupt.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn expire_next_query_for_test(&self) {
        *self.query_expiry.lock().unwrap() = Some(Arc::new(AtomicBool::new(true)));
    }

    /// Compatibility for isolated legacy fixtures only. Production jobs declare their class.
    #[cfg(test)]
    pub async fn call<T, F>(&self, operation: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        self.call_read(Operation::Query, operation).await
    }

    /// A cancellable query, with a fresh five-second execution budget after dequeue.
    pub async fn call_read<T, F>(&self, name: Operation, operation: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        self.call_named(name, JobClass::Query, operation).await
    }

    /// Durable mutations retain SQLite's own errors and busy timeout, but are never aborted
    /// because filesystem or statement work consumed a query's elapsed-time budget.
    pub(crate) async fn call_write<T, F>(
        &self,
        name: Operation,
        operation: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        self.call_named(name, JobClass::DurableMutation, operation)
            .await
    }

    async fn call_named<T, F>(
        &self,
        name: Operation,
        class: JobClass,
        operation: F,
    ) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let diagnostics = self.diagnostics.clone();
        #[cfg(test)]
        let fake_expiry = if name == Operation::Append {
            self.append_expiry.lock().unwrap().take()
        } else if class == JobClass::Query {
            self.query_expiry.lock().unwrap().take()
        } else {
            None
        };
        let sent = self
            .sender
            .send(Box::new(move |conn| {
                let started = std::time::Instant::now();
                let deadline = started + Duration::from_secs(5);
                let interrupted = Arc::new(AtomicBool::new(false));
                let callback_interrupted = interrupted.clone();
                conn.progress_handler(0, None::<fn() -> bool>);
                if class == JobClass::Query {
                    conn.progress_handler(
                        10_000,
                        Some(move || {
                            let expired = std::time::Instant::now() >= deadline;
                            #[cfg(test)]
                            let expired = expired
                                || fake_expiry
                                    .as_ref()
                                    .is_some_and(|clock| clock.load(Ordering::Relaxed));
                            if expired {
                                callback_interrupted.store(true, Ordering::Relaxed);
                            }
                            expired
                        }),
                    );
                }
                let result = operation(conn);
                conn.progress_handler(0, None::<fn() -> bool>);
                if let Err(error) = &result {
                    diagnostics.storage_failure(
                        name,
                        class,
                        error,
                        started.elapsed(),
                        interrupted.load(Ordering::Relaxed),
                        Some(conn.is_autocommit()),
                    );
                }
                // Publish failure evidence before the collector can react to this result.
                let _ = tx.send(result);
            }))
            .await;
        if sent.is_err() {
            self.diagnostics.storage_failure(
                name,
                class,
                &StoreError::Unavailable,
                Duration::ZERO,
                false,
                None,
            );
            return Err(StoreError::Unavailable);
        }
        rx.await.map_err(|_| {
            self.diagnostics.storage_failure(
                name,
                class,
                &StoreError::Unavailable,
                Duration::ZERO,
                false,
                None,
            );
            StoreError::Unavailable
        })?
    }

    pub async fn channels(&self) -> Result<Vec<Channel>, StoreError> {
        self.call_read(Operation::Query, |conn| {
            let mut stmt = conn.prepare("SELECT c.login,c.tracked,c.enabled,c.created_at,c.updated_at,
                (SELECT MAX(s.id) FROM sessions s JOIN session_channels sc ON sc.session_id=s.id WHERE sc.channel=c.login AND s.ended_at IS NULL),
                (SELECT received_at FROM observations o WHERE o.channel=c.login AND o.account IS NOT NULL ORDER BY id DESC LIMIT 1),
                (SELECT sc.collection_state FROM session_channels sc JOIN sessions s ON s.id=sc.session_id WHERE sc.channel=c.login AND s.ended_at IS NULL ORDER BY s.id DESC LIMIT 1),
                (SELECT sc.collection_error FROM session_channels sc JOIN sessions s ON s.id=sc.session_id WHERE sc.channel=c.login AND s.ended_at IS NULL ORDER BY s.id DESC LIMIT 1)
                FROM channels c ORDER BY c.tracked DESC,c.login")?;
            let rows = stmt.query_map([], |r| Ok(Channel { login:r.get(0)?,tracked:r.get(1)?,enabled:r.get(2)?,created_at:r.get(3)?,updated_at:r.get(4)?,active_session_id:r.get(5)?,last_observed_at:r.get(6)?,collection_state:r.get(7)?,collection_error:r.get(8)? }))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        }).await
    }

    pub async fn enabled(&self) -> Result<Vec<String>, StoreError> {
        self.call_read(Operation::Query, |conn| {
            let mut stmt = conn.prepare(
                "SELECT login FROM channels WHERE tracked=1 AND enabled=1 ORDER BY login",
            )?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
    }

    pub async fn track(&self, logins: Vec<String>, enabled: bool) -> Result<(), StoreError> {
        if logins.is_empty() || logins.len() > 50 {
            return Err(StoreError::ChannelLimit);
        }
        let mut normalized = logins
            .iter()
            .map(|s| normalize(s))
            .collect::<Result<Vec<_>, _>>()?;
        normalized.sort();
        normalized.dedup();
        self.call_write(Operation::ChannelConfiguration, move |conn| {
            let tx=conn.transaction()?;
            let stamp=now();
            for login in normalized {
                tx.execute("INSERT INTO channels(login,tracked,enabled,created_at,updated_at) VALUES (?1,1,?2,?3,?3) ON CONFLICT(login) DO UPDATE SET tracked=1,enabled=excluded.enabled,updated_at=excluded.updated_at",params![login,enabled,stamp])?;
            }
            let count:i64=tx.query_row("SELECT COUNT(*) FROM channels WHERE tracked=1",[],|r|r.get(0))?;
            if count>50 { return Err(StoreError::ChannelLimit); }
            tx.commit()?; Ok(())
        }).await
    }

    pub async fn update_channel(
        &self,
        login: String,
        enabled: bool,
        remove: bool,
    ) -> Result<(), StoreError> {
        let login = normalize(&login)?;
        self.call_write(Operation::ChannelConfiguration, move |conn| {
            let changed=conn.execute("UPDATE channels SET enabled=?1,tracked=?2,updated_at=?3 WHERE login=?4 AND tracked=1",params![enabled,!remove,now(),login])?;
            if changed==0 { return Err(StoreError::NotFound); }
            Ok(())
        }).await
    }

    pub async fn begin_session(&self, channels: Vec<String>) -> Result<i64, StoreError> {
        self.begin_session_at(channels, now()).await
    }

    /// Starts a room/session at its transport receipt boundary, even when persistence is delayed.
    pub async fn begin_session_at(
        &self,
        channels: Vec<String>,
        at: String,
    ) -> Result<i64, StoreError> {
        let at = crate::history::utc(&at)
            .inspect_err(|error| self.record_failure(Operation::SessionBegin, error))?;
        self.call_write(Operation::SessionBegin, move |conn| {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO sessions(started_at) VALUES (?1)", [at])?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "UPDATE collection_totals SET session_count=session_count+1 WHERE id=1",
                [],
            )?;
            for channel in channels {
                tx.execute(
                    "INSERT INTO session_channels(session_id,channel) VALUES (?1,?2)",
                    params![id, channel],
                )?;
            }
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    pub async fn end_session(&self, id: i64, reason: String) -> Result<(), StoreError> {
        self.end_session_at(id, reason, now()).await
    }

    /// Closes at the observed transport boundary; closure persistence has its own timestamp.
    pub async fn end_session_at(
        &self,
        id: i64,
        reason: String,
        at: String,
    ) -> Result<(), StoreError> {
        self.end_sessions(vec![SessionEnd {
            session_id: id,
            reason,
            source_at: Some(at),
        }])
        .await
    }

    /// Fallback only when a failed transport cannot provide its terminal receipt clock.
    /// The last retained receipt (or start for an empty session) is an uncertain cutoff.
    pub async fn end_session_uncertain(&self, id: i64, reason: String) -> Result<(), StoreError> {
        self.end_sessions(vec![SessionEnd {
            session_id: id,
            reason,
            source_at: None,
        }])
        .await
    }

    /// Atomically closes at most 50 room sessions with one durable commit. Each transport cutoff
    /// is validated before the job is queued; a missing cutoff is resolved separately per session.
    /// Missing or already-closed sessions are harmless; the first close of a repeated ID wins.
    pub async fn end_sessions(&self, mut sessions: Vec<SessionEnd>) -> Result<(), StoreError> {
        if sessions.len() > 50 {
            self.record_failure(Operation::SessionEnd, &StoreError::Invalid);
            return Err(StoreError::Invalid);
        }
        if sessions.is_empty() {
            return Ok(());
        }
        for end in &mut sessions {
            end.source_at = end
                .source_at
                .as_deref()
                .map(crate::history::utc)
                .transpose()
                .inspect_err(|error| self.record_failure(Operation::SessionEnd, error))?;
        }
        self.call_write(Operation::SessionEnd, move |conn| {
            let tx = conn.transaction()?;
            for end in sessions {
                let uncertain = end.source_at.is_none();
                close_session_in_transaction(
                    &tx,
                    end.session_id,
                    &end.reason,
                    end.source_at,
                    uncertain,
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn room_state(
        &self,
        session_id: i64,
        channel: String,
        ready: bool,
    ) -> Result<(), StoreError> {
        self.call_write(Operation::RoomState, move |conn| {
            let tx=conn.transaction()?;
            let state=if ready {"collecting"} else {"unavailable"};
            let error=if ready {None} else {Some("Membership unavailable for this room; healthy rooms continue collecting.")};
            tx.execute("UPDATE session_channels SET collection_state=?1,collection_error=?2 WHERE session_id=?3 AND channel=?4",params![state,error,session_id,channel])?;
            if !ready {crate::history::gap(&tx,session_id,&channel,"membership_unavailable")?;}
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn append(
        &self,
        session_id: i64,
        observations: Vec<NewObservation>,
    ) -> Result<(), StoreError> {
        self.append_sessions(vec![(session_id, observations)]).await
    }

    /// Commits a bounded flush across independent room sessions atomically. Session identity and
    /// per-room receipt order are preserved without one FULL-sync commit for every room.
    pub async fn append_sessions(
        &self,
        sessions: Vec<(i64, Vec<NewObservation>)>,
    ) -> Result<(), StoreError> {
        self.append_sessions_timed(sessions).await.map(|_| ())
    }

    // Shared with the opt-in workload so CI can distinguish statement work from durable commit
    // and automatic WAL checkpoint costs. Timing overhead is constant per flush.
    pub(crate) async fn append_sessions_timed(
        &self,
        sessions: Vec<(i64, Vec<NewObservation>)>,
    ) -> Result<(Duration, Duration), StoreError> {
        let count = sessions
            .iter()
            .try_fold(0usize, |count, (_, rows)| count.checked_add(rows.len()))
            .ok_or(StoreError::Invalid)
            .inspect_err(|error| self.record_failure(Operation::Append, error))?;
        if count > 10_000 {
            self.record_failure(Operation::Append, &StoreError::Invalid);
            return Err(StoreError::Invalid);
        }
        if sessions.is_empty() {
            return Ok((Duration::ZERO, Duration::ZERO));
        }
        #[cfg(test)]
        let fake_expiry = self.append_expiry.lock().unwrap().clone();
        #[cfg(test)]
        let inject_interrupt = self.append_interrupt.swap(false, Ordering::Relaxed);
        self.call_write(Operation::Append, move |conn| {
            let started=std::time::Instant::now();
            let tx=conn.transaction()?;
            { let mut stmt=tx.prepare_cached("INSERT INTO observations(session_id,channel,account,kind,received_at,received_at_utc) VALUES (?1,?2,?3,?4,?5,?6)")?;
              let mut relationship=tx.prepare_cached("INSERT INTO relationships(channel,account,first_observed_at,last_observed_at,observation_count) VALUES (?1,?2,?3,?3,1) ON CONFLICT(channel,account) DO UPDATE SET first_observed_at=MIN(first_observed_at,excluded.first_observed_at),last_observed_at=MAX(last_observed_at,excluded.last_observed_at),observation_count=observation_count+1")?;
              let mut session_open=tx.prepare_cached("SELECT ended_at IS NULL FROM sessions WHERE id=?1")?;
              for (session_id,observations) in sessions {
              let open:Option<bool>=session_open.query_row([session_id],|r|r.get(0)).optional()?;
              if open!=Some(true) { return Err(StoreError::Invalid); }
              for event in observations {
                  let stamp=crate::history::utc(&event.received_at)?;
                  let channel=normalize(&event.channel)?;
                  let account=event.account.as_deref().map(normalize).transpose()?;
                  stmt.execute(params![session_id,channel,account,event.kind,event.received_at,stamp])?;
                  let id=tx.last_insert_rowid();
                  if let Some(ref account)=account {crate::history::derive(&tx,id,session_id,&channel,account,&event.kind,&stamp)?;}
                  if let Some(account)=account {relationship.execute(params![channel,account,stamp])?;}
              } } }
            tx.execute("UPDATE collection_totals SET observation_count=observation_count+?1 WHERE id=1",[count as i64])?;
            #[cfg(test)]
            {
                if let Some(clock) = &fake_expiry {
                    // Advance only after actual raw, derived and aggregate writes began.
                    // If a query budget were incorrectly installed on this durable job,
                    // the SQL below would reach its expired production callback.
                    clock.store(true, Ordering::Relaxed);
                }
                if inject_interrupt {
                    // Independent fault injection: cause a real SQLite interruption while
                    // executing SQL, without claiming the production query budget requested it.
                    tx.progress_handler(10_000, Some(|| true));
                }
                if fake_expiry.is_some() || inject_interrupt {
                    tx.query_row("WITH RECURSIVE work(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM work WHERE x<100000) SELECT SUM(x) FROM work", [], |row| row.get::<_, i64>(0))?;
                }
            }
            let sql_time=started.elapsed();let committing=std::time::Instant::now();
            tx.commit()?; Ok((sql_time,committing.elapsed()))
        }).await
    }

    pub async fn search(
        &self,
        account: Option<String>,
        channel: Option<String>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Observation>, StoreError> {
        self.search_range(
            account,
            channel,
            before,
            limit,
            crate::history::TimeRange::default(),
        )
        .await
    }

    pub async fn search_range(
        &self,
        account: Option<String>,
        channel: Option<String>,
        before: Option<i64>,
        limit: usize,
        range: crate::history::TimeRange,
    ) -> Result<Vec<Observation>, StoreError> {
        let account = account.map(|a| normalize(&a)).transpose()?;
        let channel = channel.map(|c| normalize(&c)).transpose()?;
        self.call_read(Operation::Query, move |conn| {
            let mut sql="SELECT id,session_id,channel,account,kind,received_at,received_at_utc FROM observations WHERE id<?".to_owned();
            let mut values=vec![rusqlite::types::Value::Integer(before.unwrap_or(i64::MAX))];
            if let Some(account)=account {sql.push_str(" AND account=?");values.push(account.into());}
            if let Some(channel)=channel {sql.push_str(" AND channel=?");values.push(channel.into());}
            if let Some(from)=range.from {sql.push_str(" AND received_at_utc>=?");values.push(from.into());}
            if let Some(to)=range.to {sql.push_str(" AND received_at_utc<?");values.push(to.into());}
            sql.push_str(" ORDER BY id DESC LIMIT ?");values.push((limit.min(501) as i64).into());
            let mut stmt=conn.prepare(&sql)?;
            let rows=stmt.query_map(rusqlite::params_from_iter(values),|r|Ok(Observation {id:r.get(0)?,session_id:r.get(1)?,channel:r.get(2)?,account:r.get(3)?,kind:r.get(4)?,received_at:r.get(5)?,received_at_utc:r.get(6)?,source:"twitch_irc"}))?;
            Ok(rows.collect::<Result<Vec<_>,_>>()?)
        }).await
    }

    pub async fn history(
        &self,
        channel: String,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Session>, StoreError> {
        self.history_range(channel, before, limit, crate::history::TimeRange::default())
            .await
    }
    pub async fn history_range(
        &self,
        channel: String,
        before: Option<i64>,
        limit: usize,
        range: crate::history::TimeRange,
    ) -> Result<Vec<Session>, StoreError> {
        let channel = normalize(&channel)?;
        self.call_read(Operation::Query, move |conn| {
            let mut stmt=conn.prepare("SELECT s.id,sc.channel,s.started_at,s.ended_at,s.end_reason,(SELECT COUNT(*) FROM observations o WHERE o.session_id=s.id AND o.channel=sc.channel),s.end_uncertain,s.closed_recorded_at FROM sessions s JOIN session_channels sc ON s.id=sc.session_id WHERE sc.channel=?1 AND s.id<?2 AND (?4 IS NULL OR COALESCE(s.ended_at,'9999')>?4) AND (?5 IS NULL OR s.started_at<?5) ORDER BY s.id DESC LIMIT ?3")?;
            let rows=stmt.query_map(params![channel,before.unwrap_or(i64::MAX),limit.min(501) as i64,range.from,range.to],|r|Ok(Session{id:r.get(0)?,channel:r.get(1)?,started_at:r.get(2)?,ended_at:r.get(3)?,end_reason:r.get(4)?,observation_count:r.get(5)?,end_uncertain:r.get(6)?,closed_recorded_at:r.get(7)?}))?;
            Ok(rows.collect::<Result<Vec<_>,_>>()?)
        }).await
    }

    pub async fn relationships(
        &self,
        account: Option<String>,
        channel: Option<String>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Relationship>, StoreError> {
        self.relationships_range(
            account,
            channel,
            offset,
            limit,
            crate::history::TimeRange::default(),
        )
        .await
    }
    pub async fn relationships_range(
        &self,
        account: Option<String>,
        channel: Option<String>,
        offset: usize,
        limit: usize,
        range: crate::history::TimeRange,
    ) -> Result<Vec<Relationship>, StoreError> {
        let account = account.map(|a| normalize(&a)).transpose()?;
        let channel = channel.map(|c| normalize(&c)).transpose()?;
        self.call_read(Operation::Query, move |conn| {
            let filtered=range.from.is_some()||range.to.is_some();
            let mut sql=if filtered {"SELECT channel,account,MIN(received_at_utc),MAX(received_at_utc),COUNT(*) FROM observations WHERE account IS NOT NULL"}else{"SELECT channel,account,first_observed_at,last_observed_at,observation_count FROM relationships WHERE 1=1"}.to_owned();
            let mut values=Vec::<rusqlite::types::Value>::new();
            if let Some(account)=account {sql.push_str(" AND account=?");values.push(account.into());}
            if let Some(channel)=channel {sql.push_str(" AND channel=?");values.push(channel.into());}
            if let Some(from)=range.from {sql.push_str(" AND received_at_utc>=?");values.push(from.into());}
            if let Some(to)=range.to {sql.push_str(" AND received_at_utc<?");values.push(to.into());}
            if filtered {sql.push_str(" GROUP BY channel,account");}
            sql.push_str(" ORDER BY channel,account LIMIT ? OFFSET ?");values.push((limit.min(501) as i64).into());values.push((offset.min(1_000_000) as i64).into());
            let mut stmt=conn.prepare(&sql)?;
            let rows=stmt.query_map(rusqlite::params_from_iter(values),|r|Ok(Relationship{channel:r.get(0)?,account:r.get(1)?,first_observed_at:r.get(2)?,last_observed_at:r.get(3)?,observation_count:r.get(4)?}))?;
            Ok(rows.collect::<Result<Vec<_>,_>>()?)
        }).await
    }

    pub async fn stats(&self) -> Result<StorageStats, StoreError> {
        let path = self.path.clone();
        self.call_read(Operation::Query, move |conn| {
            let (observation_count, session_count) = conn.query_row(
                "SELECT observation_count,session_count FROM collection_totals WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let storage_bytes = [
                path.as_path().to_path_buf(),
                PathBuf::from(format!("{}-wal", path.display())),
            ]
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
            Ok(StorageStats {
                observation_count,
                session_count,
                storage_bytes,
            })
        })
        .await
    }

    pub async fn checkpoint(&self) -> Result<(), StoreError> {
        self.call_write(Operation::Checkpoint, |conn| {
            let state: (i64, i64, i64) =
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
            if state != (0, 0, 0) {
                return Err(StoreError::CheckpointBusy);
            }
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delayed_transport_boundaries_preserve_source_times_and_do_not_bridge_a_known_gap() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("source-boundaries.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store
            .begin_session_at(vec!["example".into()], "2026-01-01T02:00:00+02:00".into())
            .await
            .unwrap();
        let raw_receipt = "2026-01-01T00:00:30Z";
        store
            .append(
                session,
                vec![NewObservation {
                    channel: "example".into(),
                    account: Some("alice".into()),
                    kind: "join".into(),
                    received_at: raw_receipt.into(),
                }],
            )
            .await
            .unwrap();
        let recorded_after = now();
        store
            .end_session_at(
                session,
                "transport_gap".into(),
                "2026-01-01T02:01:00+02:00".into(),
            )
            .await
            .unwrap();
        let history = store.history("example".into(), None, 10).await.unwrap();
        assert_eq!(history[0].started_at, "2026-01-01T00:00:00.000Z");
        assert_eq!(
            history[0].ended_at.as_deref(),
            Some("2026-01-01T00:01:00.000Z")
        );
        assert!(!history[0].end_uncertain);
        let recorded = history[0].closed_recorded_at.as_deref().unwrap();
        assert!(recorded >= recorded_after.as_str());
        assert_ne!(recorded, history[0].ended_at.as_deref().unwrap());
        let closed = store
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT closed_at FROM history_visits WHERE session_id=?1",
                    [session],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(closed, "2026-01-01T00:01:00.000Z");
        let range = crate::history::TimeRange::parse(
            Some("2026-01-01T00:00:15Z".into()),
            Some("2026-01-01T00:00:45Z".into()),
        )
        .unwrap();
        assert_eq!(
            store
                .history_range("example".into(), None, 10, range)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .presence("example".into(), None, "2026-01-01T00:00:45Z".into(), 0, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        for at in ["2026-01-01T00:01:00Z", "2026-01-01T00:01:30Z"] {
            assert!(store
                .presence("example".into(), None, at.into(), 0, 10)
                .await
                .unwrap()
                .is_empty());
        }
        let next = store
            .begin_session_at(vec!["example".into()], "2026-01-01T00:02:00Z".into())
            .await
            .unwrap();
        store
            .append(
                next,
                vec![NewObservation {
                    channel: "example".into(),
                    account: Some("alice".into()),
                    kind: "join".into(),
                    received_at: "2026-01-01T00:02:01Z".into(),
                }],
            )
            .await
            .unwrap();
        // A repeated cleanup call cannot move an already recorded source boundary.
        store
            .end_session_at(
                session,
                "later_cleanup".into(),
                "2026-01-01T00:10:00Z".into(),
            )
            .await
            .unwrap();
        store
            .call_write(Operation::Initialize, crate::history::recompute)
            .await
            .unwrap();
        assert!(store
            .presence("example".into(), None, "2026-01-01T00:01:30Z".into(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        let history = store.history("example".into(), None, 10).await.unwrap();
        let original = history.iter().find(|row| row.id == session).unwrap();
        assert_eq!(
            original.ended_at.as_deref(),
            Some("2026-01-01T00:01:00.000Z")
        );
        assert_eq!(original.end_reason.as_deref(), Some("transport_gap"));
        let raw = store.search(None, None, None, 10).await.unwrap();
        assert_eq!(
            raw.iter()
                .find(|row| row.session_id == session)
                .unwrap()
                .received_at,
            raw_receipt
        );
        assert!(matches!(
            store
                .begin_session_at(vec!["example".into()], "invalid".into())
                .await,
            Err(StoreError::Invalid)
        ));
        assert!(matches!(
            store
                .end_session_at(next, "gap".into(), "invalid".into())
                .await,
            Err(StoreError::Invalid)
        ));
    }

    #[tokio::test]
    async fn backwards_transport_end_is_retained_and_flagged_uncertain_after_recomputation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("backwards-boundary.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store
            .begin_session_at(vec!["example".into()], "2026-01-01T00:10:00Z".into())
            .await
            .unwrap();
        store
            .append(
                session,
                vec![NewObservation {
                    channel: "example".into(),
                    account: Some("alice".into()),
                    kind: "join".into(),
                    received_at: "2026-01-01T00:12:00Z".into(),
                }],
            )
            .await
            .unwrap();
        store
            .end_session_at(
                session,
                "transport_clock_reversal".into(),
                "2026-01-01T00:11:00Z".into(),
            )
            .await
            .unwrap();
        let history = store.history("example".into(), None, 10).await.unwrap();
        assert_eq!(
            history[0].ended_at.as_deref(),
            Some("2026-01-01T00:11:00.000Z")
        );
        assert!(history[0].end_uncertain);
        let query = || crate::history::VisitQuery {
            limit: 10,
            ..crate::history::VisitQuery::default()
        };
        assert!(store.visits(query()).await.unwrap()[0].out_of_order);
        assert!(store
            .presence("example".into(), None, "2026-01-01T00:12:01Z".into(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        store
            .call_write(Operation::Initialize, crate::history::recompute)
            .await
            .unwrap();
        assert!(store.visits(query()).await.unwrap()[0].out_of_order);
        let empty = store
            .begin_session_at(vec!["example".into()], "2026-01-01T00:10:00Z".into())
            .await
            .unwrap();
        store
            .end_session_at(
                empty,
                "transport_clock_reversal".into(),
                "2026-01-01T00:09:00Z".into(),
            )
            .await
            .unwrap();
        assert!(store.history("example".into(), None, 10).await.unwrap()[0].end_uncertain);
    }

    #[tokio::test]
    async fn delayed_source_end_clamps_a_previously_persisted_gap_without_rewriting_raw_boundaries()
    {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("delayed-gap.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store
            .begin_session_at(vec!["example".into()], "2026-01-01T00:00:00Z".into())
            .await
            .unwrap();
        store
            .append(
                session,
                [
                    ("alice", "join", "00:00:30"),
                    ("bob", "join", "00:00:10"),
                    ("bob", "part", "00:00:45"),
                ]
                .into_iter()
                .map(|(account, kind, at)| NewObservation {
                    channel: "example".into(),
                    account: Some(account.into()),
                    kind: kind.into(),
                    received_at: format!("2026-01-01T{at}Z"),
                })
                .collect(),
            )
            .await
            .unwrap();
        store
            .room_state(session, "example".into(), false)
            .await
            .unwrap();
        let raw_gap = store
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT recorded_at FROM history_boundaries WHERE session_id=?1",
                    [session],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
        store
            .end_session_at(
                session,
                "membership_unavailable".into(),
                "2026-01-01T00:01:00Z".into(),
            )
            .await
            .unwrap();
        for recompute in [false, true] {
            if recompute {
                store
                    .call_write(Operation::Initialize, crate::history::recompute)
                    .await
                    .unwrap();
            }
            let (closed,retained_gap,part)=store.call(move|conn|Ok((conn.query_row("SELECT closed_at FROM history_visits WHERE session_id=?1 AND account='alice'",[session],|row|row.get::<_,String>(0))?,conn.query_row("SELECT recorded_at FROM history_boundaries WHERE session_id=?1",[session],|row|row.get::<_,String>(0))?,conn.query_row("SELECT end_observed_at FROM history_visits WHERE session_id=?1 AND account='bob'",[session],|row|row.get::<_,String>(0))?))).await.unwrap();
            assert_eq!(closed, "2026-01-01T00:01:00.000Z");
            assert_eq!(retained_gap, raw_gap);
            assert_eq!(part, "2026-01-01T00:00:45.000Z");
            assert!(store
                .presence("example".into(), None, "2026-01-01T00:01:30Z".into(), 0, 10)
                .await
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test]
    async fn missing_terminal_clock_uses_an_uncertain_retained_cutoff_instead_of_persistence_time()
    {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("uncertain-terminal.sqlite3"))
            .await
            .unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store
            .begin_session_at(vec!["example".into()], "2026-01-01T00:00:00Z".into())
            .await
            .unwrap();
        store
            .append(
                session,
                vec![
                    NewObservation {
                        channel: "example".into(),
                        account: Some("alice".into()),
                        kind: "join".into(),
                        received_at: "2026-01-01T00:00:30Z".into(),
                    },
                    NewObservation {
                        channel: "example".into(),
                        account: None,
                        kind: "names_end".into(),
                        received_at: "2026-01-01T00:00:40Z".into(),
                    },
                ],
            )
            .await
            .unwrap();
        store
            .room_state(session, "example".into(), false)
            .await
            .unwrap();
        store
            .end_session_uncertain(session, "transport_worker_failed".into())
            .await
            .unwrap();
        let rows = store.history("example".into(), None, 10).await.unwrap();
        assert_eq!(
            rows[0].ended_at.as_deref(),
            Some("2026-01-01T00:00:40.000Z")
        );
        assert!(rows[0].end_uncertain);
        assert_ne!(rows[0].ended_at, rows[0].closed_recorded_at);
        let closed = store
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT closed_at FROM history_visits WHERE session_id=?1",
                    [session],
                    |row| row.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(closed, "2026-01-01T00:00:40.000Z");
        assert!(store
            .presence("example".into(), None, "2026-01-01T00:00:41Z".into(), 0, 10)
            .await
            .unwrap()
            .is_empty());
        let empty = store
            .begin_session_at(vec!["example".into()], "2026-01-01T00:01:00Z".into())
            .await
            .unwrap();
        store
            .end_session_uncertain(empty, "transport_worker_failed".into())
            .await
            .unwrap();
        let rows = store.history("example".into(), None, 10).await.unwrap();
        assert_eq!(
            rows[0].ended_at.as_deref(),
            Some(rows[0].started_at.as_str())
        );
        assert!(rows[0].end_uncertain);
        assert_eq!(store.stats().await.unwrap().observation_count, 2);
    }

    #[tokio::test]
    async fn retained_evidence_has_session_boundaries_and_exact_filters() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("test.sqlite")).await.unwrap();
        store.track(vec!["Test".into()], true).await.unwrap();
        let first = store.begin_session(vec!["test".into()]).await.unwrap();
        store
            .append(
                first,
                vec![NewObservation {
                    channel: "test".into(),
                    account: Some("alice".into()),
                    kind: "names".into(),
                    received_at: now(),
                }],
            )
            .await
            .unwrap();
        store
            .end_session(first, "transport_failed".into())
            .await
            .unwrap();
        let second = store.begin_session(vec!["test".into()]).await.unwrap();
        store
            .append(
                second,
                vec![NewObservation {
                    channel: "test".into(),
                    account: Some("alice".into()),
                    kind: "join".into(),
                    received_at: now(),
                }],
            )
            .await
            .unwrap();
        store.end_session(second, "shutdown".into()).await.unwrap();
        store
            .update_channel("test".into(), false, true)
            .await
            .unwrap();
        let rows = store
            .search(Some("ALICE".into()), None, None, 100)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].session_id, rows[1].session_id);
        assert_eq!(
            store
                .search(None, None, Some(rows[0].id), 100)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .search(Some("ali".into()), None, None, 100)
            .await
            .unwrap()
            .is_empty());
        assert!(!store.channels().await.unwrap()[0].tracked);
        assert_eq!(
            store
                .relationships(Some("alice".into()), None, 0, 100)
                .await
                .unwrap()[0]
                .observation_count,
            2
        );
        assert!(store.append(first, vec![]).await.is_err());
        assert_eq!(store.stats().await.unwrap().observation_count, 2);
        let open = store.begin_session(vec!["test".into()]).await.unwrap();
        let valid = NewObservation {
            channel: "test".into(),
            account: Some("alice".into()),
            kind: "names".into(),
            received_at: now(),
        };
        let invalid = NewObservation {
            channel: "not_in_session".into(),
            account: Some("alice".into()),
            kind: "names".into(),
            received_at: now(),
        };
        assert!(store.append(open, vec![valid, invalid]).await.is_err());
        assert_eq!(store.stats().await.unwrap().observation_count, 2);
        assert_eq!(
            store
                .relationships(Some("alice".into()), None, 0, 100)
                .await
                .unwrap()[0]
                .observation_count,
            2
        );
    }

    #[tokio::test]
    async fn wal_checkpoint_policy_keeps_full_durability_and_explicit_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal-policy.sqlite3");
        let store = Store::open(&path).await.unwrap();
        let policy = store
            .call(|conn| {
                Ok((
                    conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))?,
                    conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))?,
                    conn.pragma_query_value(None, "wal_autocheckpoint", |r| r.get::<_, i64>(0))?,
                    conn.pragma_query_value(None, "journal_size_limit", |r| r.get::<_, i64>(0))?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(policy, ("wal".into(), 2, 8192, 64 * 1024 * 1024));
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        store
            .append(
                session,
                vec![NewObservation {
                    channel: "example".into(),
                    account: Some("alice".into()),
                    kind: "join".into(),
                    received_at: now(),
                }],
            )
            .await
            .unwrap();
        assert!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .unwrap()
                .len()
                > 0
        );
        store.checkpoint().await.unwrap();
        assert_eq!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .map(|file| file.len())
                .unwrap_or(0),
            0
        );
        assert_eq!(
            store
                .search(Some("alice".into()), None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.stats().await.unwrap().observation_count, 1);
    }

    #[tokio::test]
    async fn checkpoint_reports_a_pinned_external_reader_and_retains_committed_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pinned-reader.sqlite3");
        let store = Store::open(&path).await.unwrap();
        store.track(vec!["example".into()], true).await.unwrap();
        let session = store.begin_session(vec!["example".into()]).await.unwrap();
        store
            .call(|conn| {
                conn.busy_timeout(Duration::from_millis(20))?;
                Ok(())
            })
            .await
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT * FROM channels;")
            .unwrap();
        store
            .append(
                session,
                vec![NewObservation {
                    channel: "example".into(),
                    account: Some("alice".into()),
                    kind: "join".into(),
                    received_at: now(),
                }],
            )
            .await
            .unwrap();
        assert!(matches!(
            store.checkpoint().await,
            Err(StoreError::CheckpointBusy)
        ));
        assert!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .unwrap()
                .len()
                > 0
        );
        reader.execute_batch("COMMIT;").unwrap();
        drop(reader);
        store.checkpoint().await.unwrap();
        assert_eq!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .map(|file| file.len())
                .unwrap_or(0),
            0
        );
        assert_eq!(
            store
                .search(Some("alice".into()), None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.stats().await.unwrap().observation_count, 1);
    }

    #[tokio::test]
    async fn multiroom_flush_commits_once_preserves_order_and_rolls_back_every_room_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("multiroom.sqlite3"))
            .await
            .unwrap();
        store
            .track(vec!["first".into(), "second".into()], true)
            .await
            .unwrap();
        let first = store.begin_session(vec!["first".into()]).await.unwrap();
        let second = store.begin_session(vec!["second".into()]).await.unwrap();
        let event = |channel: &str, kind: &str| NewObservation {
            channel: channel.into(),
            account: Some("alice".into()),
            kind: kind.into(),
            received_at: now(),
        };
        let commits = Arc::new(AtomicUsize::new(0));
        let counted = commits.clone();
        store
            .call(move |conn| {
                conn.commit_hook(Some(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    false
                }));
                Ok(())
            })
            .await
            .unwrap();
        store
            .append_sessions(vec![
                (first, vec![event("first", "join"), event("first", "part")]),
                (second, vec![event("second", "names")]),
                (first, vec![event("first", "join")]),
            ])
            .await
            .unwrap();
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        let rows = store.search(None, None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!((rows[0].session_id, rows[0].kind.as_str()), (first, "join"));
        assert_eq!(rows[1].session_id, second);
        assert_eq!(rows[2].kind, "part");
        assert_eq!(rows[3].kind, "join");
        // A later invalid session or a room/session mismatch must roll back raw rows,
        // relationship totals and derived visits already written for earlier rooms.
        assert!(matches!(
            store
                .append_sessions(vec![
                    (second, vec![event("second", "join")]),
                    (999999, vec![event("first", "join")])
                ])
                .await,
            Err(StoreError::Invalid)
        ));
        assert!(store
            .append_sessions(vec![
                (first, vec![event("first", "join")]),
                (second, vec![event("first", "join")])
            ])
            .await
            .is_err());
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert_eq!(store.stats().await.unwrap().observation_count, 4);
        let relationships = store.relationships(None, None, 0, 10).await.unwrap();
        assert_eq!(
            relationships
                .iter()
                .map(|row| row.observation_count)
                .sum::<i64>(),
            4
        );
        assert_eq!(
            store
                .visits(crate::history::VisitQuery {
                    limit: 10,
                    ..crate::history::VisitQuery::default()
                })
                .await
                .unwrap()
                .len(),
            3
        );
        assert!(matches!(
            store
                .append_sessions(vec![
                    (first, vec![event("first", "join"); 6000]),
                    (second, vec![event("second", "join"); 6000])
                ])
                .await,
            Err(StoreError::Invalid)
        ));
        assert_eq!(commits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn channel_limit_is_atomic_and_interrupted_session_is_closed_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let store = Store::open(&path).await.unwrap();
        store
            .track((0..50).map(|n| format!("channel{n}")).collect(), true)
            .await
            .unwrap();
        assert!(matches!(
            store.track(vec!["extra".into()], true).await,
            Err(StoreError::ChannelLimit)
        ));
        assert_eq!(store.channels().await.unwrap().len(), 50);
        let session = store.begin_session(vec!["channel0".into()]).await.unwrap();
        assert!(Store::open(&path).await.is_err());
        drop(store);
        let mut reopened = None;
        for _ in 0..50 {
            match Store::open(&path).await {
                Ok(store) => {
                    reopened = Some(store);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let reopened = reopened.expect("storage worker releases its database lock after closing");
        let history = reopened.history("channel0".into(), None, 10).await.unwrap();
        assert_eq!(history[0].id, session);
        assert!(history[0].end_uncertain);
        assert_eq!(
            history[0].ended_at.as_deref(),
            Some(history[0].started_at.as_str())
        );
        assert!(history[0].closed_recorded_at.is_some());
        assert_eq!(
            history[0].end_reason.as_deref(),
            Some("process_interrupted")
        );
    }
}
