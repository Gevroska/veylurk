//! Bounded, typed failure evidence. Backend messages and inputs never enter this module's records.
use crate::store::StoreError;
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

const JOURNAL_BYTES: u64 = 64 * 1024;
const RECORD_BYTES: usize = 1024;
const RUN_RECORDS: usize = 64;
const QUEUED_RECORDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Initialize,
    Generic,
    Append,
    SessionBegin,
    SessionEnd,
    RoomState,
    Checkpoint,
    SignalRegistration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Initialization,
    Storage,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    StorageFailure,
    SignalUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    SqliteInterrupt,
    SqliteBusy,
    SqliteLocked,
    SqliteIo,
    SqliteFull,
    SqliteConstraint,
    SqliteOther,
    WorkerUnavailable,
    InvalidInput,
    ChannelLimit,
    NotFound,
    Initialization,
    CheckpointBusy,
    SignalUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Record {
    pub schema_version: u8,
    pub sequence: u64,
    pub process_id: u32,
    pub occurred_at_utc: String,
    pub event: Event,
    pub phase: Phase,
    pub operation: Operation,
    pub category: Category,
    pub elapsed_ms: u64,
    pub sqlite_primary_code: Option<i32>,
    pub sqlite_extended_code: Option<i32>,
    pub os_error_code: Option<i32>,
    /// True only when this job's actual progress callback requested interruption.
    pub progress_interrupt_requested: bool,
    /// Observed after the operation returned; this alone does not establish rollback.
    pub autocommit_after: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceState {
    NotAttempted,
    Pending,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Persistence {
    pub state: PersistenceState,
    pub failure_count: u64,
    pub dropped_count: u64,
    pub pending_records: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// First storage failure in this process, retained when cleanup also fails.
    /// A recoverable signal-listener warning does not occupy this field.
    pub first_failure: Option<Record>,
    pub latest: Option<Record>,
    pub persistence: Persistence,
}

struct State {
    snapshot: Snapshot,
    sequence: u64,
    emitted: usize,
}

enum WriteRequest {
    Record(Record),
    Flush(mpsc::SyncSender<()>),
}

#[derive(Clone)]
pub struct Diagnostics {
    state: Arc<Mutex<State>>,
    writer: mpsc::SyncSender<WriteRequest>,
}

fn sidecar(database: &Path) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push(".diagnostics.jsonl");
    PathBuf::from(name)
}

impl Diagnostics {
    /// Construct before opening SQLite so initialization failure can be retained independently.
    pub fn new(database: &Path) -> Self {
        let state = Arc::new(Mutex::new(State {
            snapshot: Snapshot {
                first_failure: None,
                latest: None,
                persistence: Persistence {
                    state: PersistenceState::NotAttempted,
                    failure_count: 0,
                    dropped_count: 0,
                    pending_records: 0,
                },
            },
            sequence: 0,
            emitted: 0,
        }));
        let (writer, receiver) = mpsc::sync_channel(QUEUED_RECORDS);
        let worker_state = state.clone();
        let path = sidecar(database);
        let started = std::thread::Builder::new()
            .name("veylurk-diagnostics".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    match request {
                        WriteRequest::Record(record) => {
                            let saved = append_record(&path, &record);
                            let mut state = worker_state.lock().unwrap_or_else(|e| e.into_inner());
                            let persistence = &mut state.snapshot.persistence;
                            persistence.pending_records =
                                persistence.pending_records.saturating_sub(1);
                            if saved.is_err() {
                                persistence.failure_count =
                                    persistence.failure_count.saturating_add(1);
                                persistence.state = PersistenceState::Failed;
                            } else if persistence.pending_records > 0 {
                                persistence.state = PersistenceState::Pending;
                            } else {
                                persistence.state = PersistenceState::Ready;
                            }
                        }
                        WriteRequest::Flush(acknowledge) => {
                            // Each record was opened, written and sync_data'd before this acknowledgement.
                            let _ = acknowledge.send(());
                        }
                    }
                }
            });
        if started.is_err() {
            let mut current = state.lock().unwrap_or_else(|e| e.into_inner());
            current.snapshot.persistence.state = PersistenceState::Failed;
            current.snapshot.persistence.failure_count = 1;
        }
        Self { state, writer }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }

    /// Bounded acknowledgement attempt. A true result means queued attempts finished, not that
    /// every write succeeded; callers must inspect persistence.failure_count as well.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (acknowledge, received) = mpsc::sync_channel(1);
        self.writer
            .try_send(WriteRequest::Flush(acknowledge))
            .is_ok()
            && received.recv_timeout(timeout).is_ok()
    }

    pub(crate) fn storage_failure(
        &self,
        operation: Operation,
        error: &StoreError,
        elapsed: Duration,
        progress_interrupt_requested: bool,
        autocommit_after: Option<bool>,
    ) {
        // Do not use Display, Debug, SQL, conversion-error text, or sqlite_error's message.
        let sqlite_extended_code = match error {
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(code, _)) => Some(code.extended_code),
            _ => None,
        };
        let sqlite_primary_code = sqlite_extended_code.map(|code| code & 0xff);
        let category = match error {
            StoreError::Sqlite(_) => match sqlite_primary_code {
                Some(9) => Category::SqliteInterrupt,
                Some(5) => Category::SqliteBusy,
                Some(6) => Category::SqliteLocked,
                Some(10) => Category::SqliteIo,
                Some(13) => Category::SqliteFull,
                Some(19) => Category::SqliteConstraint,
                _ => Category::SqliteOther,
            },
            StoreError::Unavailable => Category::WorkerUnavailable,
            StoreError::Invalid => Category::InvalidInput,
            StoreError::ChannelLimit => Category::ChannelLimit,
            StoreError::NotFound => Category::NotFound,
            StoreError::Initialize => Category::Initialization,
            StoreError::CheckpointBusy => Category::CheckpointBusy,
        };
        self.record(Record {
            schema_version: 1,
            sequence: 0,
            process_id: std::process::id(),
            occurred_at_utc: crate::store::now(),
            event: Event::StorageFailure,
            phase: if operation == Operation::Initialize {
                Phase::Initialization
            } else {
                Phase::Storage
            },
            operation,
            category,
            elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            sqlite_primary_code,
            sqlite_extended_code,
            os_error_code: None,
            progress_interrupt_requested,
            autocommit_after,
        });
    }

    pub fn signal_unavailable(&self, os_error_code: Option<i32>) {
        self.record(Record {
            schema_version: 1,
            sequence: 0,
            process_id: std::process::id(),
            occurred_at_utc: crate::store::now(),
            event: Event::SignalUnavailable,
            phase: Phase::Shutdown,
            operation: Operation::SignalRegistration,
            category: Category::SignalUnavailable,
            elapsed_ms: 0,
            sqlite_primary_code: None,
            sqlite_extended_code: None,
            os_error_code,
            progress_interrupt_requested: false,
            autocommit_after: None,
        });
    }

    fn record(&self, mut record: Record) {
        let mut current = self.state.lock().unwrap_or_else(|e| e.into_inner());
        current.sequence = current.sequence.saturating_add(1);
        record.sequence = current.sequence;
        if record.event == Event::StorageFailure && current.snapshot.first_failure.is_none() {
            current.snapshot.first_failure = Some(record.clone());
        }
        current.snapshot.latest = Some(record.clone());
        if current.emitted >= RUN_RECORDS {
            current.snapshot.persistence.dropped_count =
                current.snapshot.persistence.dropped_count.saturating_add(1);
            return;
        }
        current.emitted += 1;
        current.snapshot.persistence.pending_records += 1;
        match self.writer.try_send(WriteRequest::Record(record)) {
            Ok(()) => current.snapshot.persistence.state = PersistenceState::Pending,
            Err(error) => {
                current.snapshot.persistence.pending_records -= 1;
                current.snapshot.persistence.dropped_count =
                    current.snapshot.persistence.dropped_count.saturating_add(1);
                if matches!(error, mpsc::TrySendError::Disconnected(_)) {
                    current.snapshot.persistence.failure_count =
                        current.snapshot.persistence.failure_count.saturating_add(1);
                    current.snapshot.persistence.state = PersistenceState::Failed;
                }
            }
        }
    }
}

fn ordinary_file(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            let reparse = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let reparse = false;
            if !metadata.is_file() || metadata.file_type().is_symlink() || reparse {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn append_record(path: &Path, record: &Record) -> io::Result<()> {
    let mut bytes =
        serde_json::to_vec(record).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    bytes.push(b'\n');
    if bytes.len() > RECORD_BYTES {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let size = ordinary_file(path)?.map_or(0, |file| file.len());
    if size.saturating_add(bytes.len() as u64) > JOURNAL_BYTES {
        let mut previous = path.as_os_str().to_owned();
        previous.push(".previous");
        let previous = PathBuf::from(previous);
        if ordinary_file(&previous)?.is_some() {
            fs::remove_file(&previous)?;
        }
        fs::rename(path, previous)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_data()
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
