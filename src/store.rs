//! Serialized SQLite access. Account logins are observations, not permanent account IDs.
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

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
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;
#[derive(Clone)]
pub struct Store {
    sender: mpsc::Sender<Job>,
    path: Arc<PathBuf>,
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

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_owned();
        let (sender, mut receiver) = mpsc::channel::<Job>(64);
        let (ready_tx, ready_rx) = oneshot::channel();
        let worker_path = path.clone();
        std::thread::Builder::new().name("veylurk-storage".into()).spawn(move || {
            let init = || -> Result<(Connection,std::fs::File), StoreError> {
                if let Some(parent) = worker_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).map_err(|_| StoreError::Initialize)?;
                }
                let lock_path=PathBuf::from(format!("{}.lock",worker_path.display()));
                let lock=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(|_|StoreError::Initialize)?;
                lock.try_lock().map_err(|_|StoreError::Initialize)?;
                let conn = Connection::open(&worker_path)?;
                conn.busy_timeout(Duration::from_secs(5))?;
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
                    CREATE TABLE IF NOT EXISTS channels(login TEXT PRIMARY KEY,tracked INTEGER NOT NULL,enabled INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
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
                let version:i64=conn.pragma_query_value(None,"user_version",|row|row.get(0))?;
                if version>1 {return Err(StoreError::Initialize);}
                if version==0 {
                    conn.execute_batch("BEGIN; INSERT OR IGNORE INTO relationships SELECT channel,account,MIN(received_at),MAX(received_at),COUNT(*) FROM observations WHERE account IS NOT NULL GROUP BY channel,account; PRAGMA user_version=1; COMMIT;")?;
                }
                // A process restart closes any session that could not record its own end.
                conn.execute("UPDATE sessions SET ended_at=COALESCE((SELECT MAX(received_at) FROM observations WHERE session_id=sessions.id),started_at),end_reason='process_interrupted',end_uncertain=1,closed_recorded_at=?1 WHERE ended_at IS NULL", [now()])?;
                Ok((conn,lock))
            };
            match init() {
                Ok((mut conn,_lock)) => {
                    if ready_tx.send(Ok(())).is_err() { return; }
                    while let Some(job) = receiver.blocking_recv() {
                        let deadline=std::time::Instant::now()+Duration::from_secs(5);
                        conn.progress_handler(10_000,Some(move ||std::time::Instant::now()>=deadline));
                        job(&mut conn);
                        conn.progress_handler(0,None::<fn()->bool>);
                    }
                    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                }
                Err(error) => { let _ = ready_tx.send(Err(error)); }
            }
        }).map_err(|_| StoreError::Initialize)?;
        ready_rx.await.map_err(|_| StoreError::Unavailable)??;
        Ok(Self {
            sender,
            path: Arc::new(path),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn call<T, F>(&self, operation: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(Box::new(move |conn| {
                let _ = tx.send(operation(conn));
            }))
            .await
            .map_err(|_| StoreError::Unavailable)?;
        rx.await.map_err(|_| StoreError::Unavailable)?
    }

    pub async fn channels(&self) -> Result<Vec<Channel>, StoreError> {
        self.call(|conn| {
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
        self.call(|conn| {
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
        self.call(move |conn| {
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
        self.call(move |conn| {
            let changed=conn.execute("UPDATE channels SET enabled=?1,tracked=?2,updated_at=?3 WHERE login=?4 AND tracked=1",params![enabled,!remove,now(),login])?;
            if changed==0 { return Err(StoreError::NotFound); }
            Ok(())
        }).await
    }

    pub async fn begin_session(&self, channels: Vec<String>) -> Result<i64, StoreError> {
        self.call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO sessions(started_at) VALUES (?1)", [now()])?;
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
        self.call(move |conn| {
            conn.execute(
                "UPDATE sessions SET ended_at=?1,closed_recorded_at=?1,end_reason=?2 WHERE id=?3 AND ended_at IS NULL",
                params![now(), reason, id],
            )?;
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
        self.call(move |conn| {
            let state=if ready {"collecting"} else {"unavailable"};
            let error=if ready {None} else {Some("Membership unavailable for this room; healthy rooms continue collecting.")};
            conn.execute("UPDATE session_channels SET collection_state=?1,collection_error=?2 WHERE session_id=?3 AND channel=?4",params![state,error,session_id,channel])?;
            Ok(())
        }).await
    }

    pub async fn append(
        &self,
        session_id: i64,
        observations: Vec<NewObservation>,
    ) -> Result<(), StoreError> {
        if observations.len() > 10_000 {
            return Err(StoreError::Invalid);
        }
        self.call(move |conn| {
            let tx=conn.transaction()?;
            let open: bool=tx.query_row("SELECT ended_at IS NULL FROM sessions WHERE id=?1",[session_id],|r|r.get(0))?;
            if !open { return Err(StoreError::Invalid); }
            let count=observations.len() as i64;
            { let mut stmt=tx.prepare_cached("INSERT INTO observations(session_id,channel,account,kind,received_at) VALUES (?1,?2,?3,?4,?5)")?;
              let mut relationship=tx.prepare_cached("INSERT INTO relationships(channel,account,first_observed_at,last_observed_at,observation_count) VALUES (?1,?2,?3,?3,1) ON CONFLICT(channel,account) DO UPDATE SET first_observed_at=MIN(first_observed_at,excluded.first_observed_at),last_observed_at=MAX(last_observed_at,excluded.last_observed_at),observation_count=observation_count+1")?;
              for event in observations {
                  stmt.execute(params![session_id,event.channel,event.account,event.kind,event.received_at])?;
                  if let Some(account)=event.account {relationship.execute(params![event.channel,account,event.received_at])?;}
              } }
            tx.execute("UPDATE collection_totals SET observation_count=observation_count+?1 WHERE id=1",[count])?;
            tx.commit()?; Ok(())
        }).await
    }

    pub async fn search(
        &self,
        account: Option<String>,
        channel: Option<String>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Observation>, StoreError> {
        let account = account.map(|a| normalize(&a)).transpose()?;
        let channel = channel.map(|c| normalize(&c)).transpose()?;
        self.call(move |conn| {
            let mut sql="SELECT id,session_id,channel,account,kind,received_at FROM observations WHERE id<?".to_owned();
            let mut values=vec![rusqlite::types::Value::Integer(before.unwrap_or(i64::MAX))];
            if let Some(account)=account {sql.push_str(" AND account=?");values.push(account.into());}
            if let Some(channel)=channel {sql.push_str(" AND channel=?");values.push(channel.into());}
            sql.push_str(" ORDER BY id DESC LIMIT ?");values.push((limit.min(501) as i64).into());
            let mut stmt=conn.prepare(&sql)?;
            let rows=stmt.query_map(rusqlite::params_from_iter(values),|r|Ok(Observation {id:r.get(0)?,session_id:r.get(1)?,channel:r.get(2)?,account:r.get(3)?,kind:r.get(4)?,received_at:r.get(5)?,source:"twitch_irc"}))?;
            Ok(rows.collect::<Result<Vec<_>,_>>()?)
        }).await
    }

    pub async fn history(
        &self,
        channel: String,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Session>, StoreError> {
        let channel = normalize(&channel)?;
        self.call(move |conn| {
            let mut stmt=conn.prepare("SELECT s.id,sc.channel,s.started_at,s.ended_at,s.end_reason,(SELECT COUNT(*) FROM observations o WHERE o.session_id=s.id AND o.channel=sc.channel),s.end_uncertain,s.closed_recorded_at FROM sessions s JOIN session_channels sc ON s.id=sc.session_id WHERE sc.channel=?1 AND s.id<?2 ORDER BY s.id DESC LIMIT ?3")?;
            let rows=stmt.query_map(params![channel,before.unwrap_or(i64::MAX),limit.min(501) as i64],|r|Ok(Session{id:r.get(0)?,channel:r.get(1)?,started_at:r.get(2)?,ended_at:r.get(3)?,end_reason:r.get(4)?,observation_count:r.get(5)?,end_uncertain:r.get(6)?,closed_recorded_at:r.get(7)?}))?;
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
        let account = account.map(|a| normalize(&a)).transpose()?;
        let channel = channel.map(|c| normalize(&c)).transpose()?;
        self.call(move |conn| {
            let mut sql="SELECT channel,account,first_observed_at,last_observed_at,observation_count FROM relationships WHERE 1=1".to_owned();
            let mut values=Vec::<rusqlite::types::Value>::new();
            if let Some(account)=account {sql.push_str(" AND account=?");values.push(account.into());}
            if let Some(channel)=channel {sql.push_str(" AND channel=?");values.push(channel.into());}
            sql.push_str(" ORDER BY channel,account LIMIT ? OFFSET ?");values.push((limit.min(501) as i64).into());values.push((offset.min(1_000_000) as i64).into());
            let mut stmt=conn.prepare(&sql)?;
            let rows=stmt.query_map(rusqlite::params_from_iter(values),|r|Ok(Relationship{channel:r.get(0)?,account:r.get(1)?,first_observed_at:r.get(2)?,last_observed_at:r.get(3)?,observation_count:r.get(4)?}))?;
            Ok(rows.collect::<Result<Vec<_>,_>>()?)
        }).await
    }

    pub async fn stats(&self) -> Result<StorageStats, StoreError> {
        let path = self.path.clone();
        self.call(move |conn| {
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
        self.call(|conn| {
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
