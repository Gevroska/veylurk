//! Bounded, typed storage activity and failure evidence. Backend messages and inputs never enter records.
use crate::store::StoreError;
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

const JOURNAL_BYTES: u64 = 64 * 1024;
const RECORD_BYTES: usize = 1024;
const RUN_RECORDS: usize = 64;
const QUEUED_RECORDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Initialize,
    Query,
    ChannelConfiguration,
    MetadataInitialize,
    MetadataWrite,
    SettingsInitialize,
    SettingsWrite,
    CommunityInitialize,
    CommunityAppend,
    Append,
    SessionBegin,
    SessionEnd,
    RoomState,
    Checkpoint,
    SignalRegistration,
    CommunityHost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobClass {
    Query,
    DurableMutation,
}

pub(crate) const STORAGE_QUEUE_CAPACITY: usize = 64;
pub(crate) const QUERY_EXECUTION_BUDGET: Duration = Duration::from_secs(5);
pub(crate) const READ_RESPONSE_BUDGET: Duration = Duration::from_secs(10);
// The serialized worker can hold one dequeued job in addition to the bounded channel.
const STORAGE_ACTIVITY_SLOTS: usize = STORAGE_QUEUE_CAPACITY + 1;

fn milliseconds(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DurationSummary {
    pub count: u64,
    pub sum_ms: u64,
    pub max_ms: u64,
}

impl DurationSummary {
    fn observe(&mut self, elapsed: Duration) {
        let elapsed = milliseconds(elapsed);
        self.count = self.count.saturating_add(1);
        self.sum_ms = self.sum_ms.saturating_add(elapsed);
        self.max_ms = self.max_ms.max(elapsed);
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ActivityTotals {
    pub requested: u64,
    pub admitted: u64,
    pub started: u64,
    pub succeeded: u64,
    /// Operations that returned an error, or an active job lost with the worker.
    pub failed: u64,
    pub canceled_before_start: u64,
    /// Call futures dropped before they obtained an actor-channel permit.
    pub abandoned_admission: u64,
    pub admission_unavailable: u64,
    pub worker_dropped_before_start: u64,
    /// A response deadline is not a storage failure; execution may still be finishing.
    pub read_response_timeouts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityOutcome {
    Succeeded,
    Failed,
    CanceledBeforeStart,
    WorkerUnavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveStorageJob {
    pub operation: Operation,
    pub job_class: JobClass,
    pub requested_at_ms: u64,
    pub admitted_at_ms: u64,
    pub started_at_ms: u64,
    pub admission_wait_ms: u64,
    pub queue_wait_ms: u64,
    pub active_age_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletedStorageJob {
    pub operation: Operation,
    pub job_class: JobClass,
    pub outcome: ActivityOutcome,
    pub requested_at_ms: u64,
    pub admitted_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: u64,
    pub admission_wait_ms: u64,
    pub queue_wait_ms: u64,
    pub execution_ms: Option<u64>,
}

/// All stamps use this Store's monotonic origin, not UTC. All totals saturate at u64::MAX.
/// This fixed-size snapshot contains no SQL, paths, arguments, backend messages or account data.
#[derive(Debug, Clone, Serialize)]
pub struct StorageActivitySnapshot {
    pub schema_version: u8,
    pub sampled_at_ms: u64,
    pub actor_queue_capacity: usize,
    pub admitted_job_capacity: usize,
    pub query_execution_budget_ms: u64,
    pub read_response_budget_ms: u64,
    pub admission_waiting: u64,
    /// Admitted jobs that have not started, including a synchronous dequeue/send handoff.
    /// This can transiently exceed actor_queue_capacity by one, but never admitted_job_capacity.
    pub waiting_to_start: usize,
    pub oldest_queued_age_ms: Option<u64>,
    pub active: Option<ActiveStorageJob>,
    pub totals: ActivityTotals,
    /// Successful admissions only; abandoned and unavailable admissions are counted separately.
    pub admission_wait_ms: DurationSummary,
    /// Started, canceled and worker-dropped queued jobs; excludes pending admissions.
    pub queue_wait_ms: DurationSummary,
    /// Returned operations only, including failures; excludes queue/admission and response delivery.
    pub execution_ms: DurationSummary,
    pub last_completed: Option<CompletedStorageJob>,
}

#[derive(Clone, Copy)]
struct ActivityEntry {
    operation: Operation,
    job_class: JobClass,
    requested: Instant,
    admitted: Instant,
    started: Option<Instant>,
}

struct ActivityState {
    origin: Instant,
    admission_waiting: u64,
    entries: [Option<ActivityEntry>; STORAGE_ACTIVITY_SLOTS],
    totals: ActivityTotals,
    admission_wait_ms: DurationSummary,
    queue_wait_ms: DurationSummary,
    execution_ms: DurationSummary,
    last_completed: Option<CompletedStorageJob>,
}

#[derive(Clone)]
pub(crate) struct StorageActivity(Arc<Mutex<ActivityState>>);

impl StorageActivity {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(ActivityState {
            origin: Instant::now(),
            admission_waiting: 0,
            entries: [None; STORAGE_ACTIVITY_SLOTS],
            totals: ActivityTotals::default(),
            admission_wait_ms: DurationSummary::default(),
            queue_wait_ms: DurationSummary::default(),
            execution_ms: DurationSummary::default(),
            last_completed: None,
        })))
    }

    pub(crate) fn request(&self) -> StorageAdmission {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.admission_waiting = state.admission_waiting.saturating_add(1);
        state.totals.requested = state.totals.requested.saturating_add(1);
        StorageAdmission {
            activity: self.clone(),
            requested: Instant::now(),
            resolved: false,
        }
    }

    pub(crate) fn read_response_timeout(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.totals.read_response_timeouts = state.totals.read_response_timeouts.saturating_add(1);
    }

    pub(crate) fn snapshot(&self) -> StorageActivitySnapshot {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let sampled = Instant::now();
        let origin = state.origin;
        let stamp = |at: Instant| milliseconds(at.duration_since(origin));
        let waiting_to_start = state
            .entries
            .iter()
            .flatten()
            .filter(|job| job.started.is_none())
            .count();
        let oldest_queued_age_ms = state
            .entries
            .iter()
            .flatten()
            .filter(|job| job.started.is_none())
            .map(|job| milliseconds(sampled.duration_since(job.admitted)))
            .max();
        let active = state.entries.iter().flatten().find_map(|job| {
            job.started.map(|started| ActiveStorageJob {
                operation: job.operation,
                job_class: job.job_class,
                requested_at_ms: stamp(job.requested),
                admitted_at_ms: stamp(job.admitted),
                started_at_ms: stamp(started),
                admission_wait_ms: milliseconds(job.admitted.duration_since(job.requested)),
                queue_wait_ms: milliseconds(started.duration_since(job.admitted)),
                active_age_ms: milliseconds(sampled.duration_since(started)),
            })
        });
        StorageActivitySnapshot {
            schema_version: 1,
            sampled_at_ms: stamp(sampled),
            actor_queue_capacity: STORAGE_QUEUE_CAPACITY,
            admitted_job_capacity: STORAGE_ACTIVITY_SLOTS,
            query_execution_budget_ms: milliseconds(QUERY_EXECUTION_BUDGET),
            read_response_budget_ms: milliseconds(READ_RESPONSE_BUDGET),
            admission_waiting: state.admission_waiting,
            waiting_to_start,
            oldest_queued_age_ms,
            active,
            totals: state.totals,
            admission_wait_ms: state.admission_wait_ms,
            queue_wait_ms: state.queue_wait_ms,
            execution_ms: state.execution_ms,
            last_completed: state.last_completed.clone(),
        }
    }
}

pub(crate) struct StorageAdmission {
    activity: StorageActivity,
    requested: Instant,
    resolved: bool,
}

impl StorageAdmission {
    /// Called only with a reserved channel permit; the following send has no await point.
    pub(crate) fn admit(mut self, operation: Operation, job_class: JobClass) -> StorageJobActivity {
        let mut state = self
            .activity
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let admitted = Instant::now();
        // At most 64 reserved/enqueued jobs plus the worker's single dequeued job exist.
        let slot = state
            .entries
            .iter()
            .position(Option::is_none)
            .expect("bounded storage activity slots");
        state.entries[slot] = Some(ActivityEntry {
            operation,
            job_class,
            requested: self.requested,
            admitted,
            started: None,
        });
        state.admission_waiting = state.admission_waiting.saturating_sub(1);
        state.totals.admitted = state.totals.admitted.saturating_add(1);
        state
            .admission_wait_ms
            .observe(admitted.duration_since(self.requested));
        self.resolved = true;
        StorageJobActivity {
            activity: self.activity.clone(),
            slot,
            finished: false,
        }
    }

    pub(crate) fn unavailable(mut self) {
        let mut state = self
            .activity
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.admission_waiting = state.admission_waiting.saturating_sub(1);
        state.totals.admission_unavailable = state.totals.admission_unavailable.saturating_add(1);
        self.resolved = true;
    }
}

impl Drop for StorageAdmission {
    fn drop(&mut self) {
        if !self.resolved {
            let mut state = self
                .activity
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.admission_waiting = state.admission_waiting.saturating_sub(1);
            state.totals.abandoned_admission = state.totals.abandoned_admission.saturating_add(1);
        }
    }
}

pub(crate) struct StorageJobActivity {
    activity: StorageActivity,
    slot: usize,
    finished: bool,
}

impl StorageJobActivity {
    pub(crate) fn start(&mut self) {
        let mut state = self
            .activity
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let started = Instant::now();
        let entry = state.entries[self.slot]
            .as_mut()
            .expect("admitted storage job");
        entry.started = Some(started);
        let wait = started.duration_since(entry.admitted);
        state.totals.started = state.totals.started.saturating_add(1);
        state.queue_wait_ms.observe(wait);
    }

    pub(crate) fn finish(&mut self, outcome: ActivityOutcome) {
        let mut state = self
            .activity
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(entry) = state.entries[self.slot].take() else {
            return;
        };
        let completed = Instant::now();
        match outcome {
            ActivityOutcome::Succeeded => {
                state.totals.succeeded = state.totals.succeeded.saturating_add(1)
            }
            ActivityOutcome::Failed => state.totals.failed = state.totals.failed.saturating_add(1),
            ActivityOutcome::CanceledBeforeStart => {
                state.totals.canceled_before_start =
                    state.totals.canceled_before_start.saturating_add(1)
            }
            ActivityOutcome::WorkerUnavailable if entry.started.is_some() => {
                state.totals.failed = state.totals.failed.saturating_add(1)
            }
            ActivityOutcome::WorkerUnavailable => {
                state.totals.worker_dropped_before_start =
                    state.totals.worker_dropped_before_start.saturating_add(1)
            }
        }
        let queue_wait = entry
            .started
            .unwrap_or(completed)
            .duration_since(entry.admitted);
        if let Some(started) = entry.started {
            if outcome != ActivityOutcome::WorkerUnavailable {
                state
                    .execution_ms
                    .observe(completed.duration_since(started));
            }
        } else {
            state.queue_wait_ms.observe(queue_wait);
        }
        let origin = state.origin;
        let stamp = |at: Instant| milliseconds(at.duration_since(origin));
        state.last_completed = Some(CompletedStorageJob {
            operation: entry.operation,
            job_class: entry.job_class,
            outcome,
            requested_at_ms: stamp(entry.requested),
            admitted_at_ms: stamp(entry.admitted),
            started_at_ms: entry.started.map(stamp),
            completed_at_ms: stamp(completed),
            admission_wait_ms: milliseconds(entry.admitted.duration_since(entry.requested)),
            queue_wait_ms: milliseconds(queue_wait),
            execution_ms: entry
                .started
                .map(|started| milliseconds(completed.duration_since(started))),
        });
        self.finished = true;
    }
}

impl Drop for StorageJobActivity {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(ActivityOutcome::WorkerUnavailable);
        }
    }
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
    CommunityHostTermination,
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
    CommunityHostTermination,
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
    pub job_class: Option<JobClass>,
    pub category: Category,
    pub elapsed_ms: u64,
    pub sqlite_primary_code: Option<i32>,
    pub sqlite_extended_code: Option<i32>,
    pub os_error_code: Option<i32>,
    /// True only when the production query-budget callback requested interruption.
    /// An independently injected SQLite interruption does not set this flag.
    pub progress_interrupt_requested: bool,
    /// Observed after the operation returned; this alone does not establish rollback.
    pub autocommit_after: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_termination: Option<community_source::host::HostTermination>,
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
        job_class: JobClass,
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
            // Response waiting is not an SQLite/durability failure and must never latch one.
            StoreError::ReadWaitTimeout => return,
        };
        self.record(Record {
            schema_version: 1,
            sequence: 0,
            process_id: std::process::id(),
            occurred_at_utc: crate::store::now(),
            event: Event::StorageFailure,
            phase: if matches!(
                operation,
                Operation::Initialize
                    | Operation::MetadataInitialize
                    | Operation::SettingsInitialize
                    | Operation::CommunityInitialize
            ) {
                Phase::Initialization
            } else {
                Phase::Storage
            },
            operation,
            job_class: Some(job_class),
            category,
            elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            sqlite_primary_code,
            sqlite_extended_code,
            os_error_code: None,
            progress_interrupt_requested,
            autocommit_after,
            host_termination: None,
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
            job_class: None,
            category: Category::SignalUnavailable,
            elapsed_ms: 0,
            sqlite_primary_code: None,
            sqlite_extended_code: None,
            os_error_code,
            progress_interrupt_requested: false,
            autocommit_after: None,
            host_termination: None,
        });
    }

    /// Called by the coordinator after receiving the reserved terminal report.
    /// The native STA never performs journal I/O or publishes arbitrary strings.
    pub(crate) fn community_host_termination(
        &self,
        termination: community_source::host::HostTermination,
    ) {
        self.record(Record {
            schema_version: 1,
            sequence: 0,
            process_id: std::process::id(),
            occurred_at_utc: crate::store::now(),
            event: Event::CommunityHostTermination,
            phase: Phase::Shutdown,
            operation: Operation::CommunityHost,
            job_class: None,
            category: Category::CommunityHostTermination,
            elapsed_ms: 0,
            sqlite_primary_code: None,
            sqlite_extended_code: None,
            os_error_code: None,
            progress_interrupt_requested: false,
            autocommit_after: None,
            host_termination: Some(termination),
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

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn activity_summaries_and_totals_saturate_without_growing_records() {
        let mut duration = DurationSummary {
            count: u64::MAX,
            sum_ms: u64::MAX - 1,
            max_ms: 0,
        };
        duration.observe(Duration::from_secs(u64::MAX));
        assert_eq!(duration.count, u64::MAX);
        assert_eq!(duration.sum_ms, u64::MAX);
        assert_eq!(duration.max_ms, u64::MAX);
        let activity = StorageActivity::new();
        {
            let mut state = activity.0.lock().unwrap();
            state.totals.requested = u64::MAX;
            state.totals.admitted = u64::MAX;
            state.totals.started = u64::MAX;
            state.totals.succeeded = u64::MAX;
            state.totals.read_response_timeouts = u64::MAX;
        }
        for _ in 0..128 {
            let admission = activity.request();
            let mut job = admission.admit(Operation::Query, JobClass::Query);
            job.start();
            job.finish(ActivityOutcome::Succeeded);
            activity.read_response_timeout();
        }
        let snapshot = activity.snapshot();
        assert_eq!(snapshot.totals.requested, u64::MAX);
        assert_eq!(snapshot.totals.admitted, u64::MAX);
        assert_eq!(snapshot.totals.started, u64::MAX);
        assert_eq!(snapshot.totals.succeeded, u64::MAX);
        assert_eq!(snapshot.totals.read_response_timeouts, u64::MAX);
        assert_eq!(snapshot.admission_waiting, 0);
        assert_eq!(snapshot.waiting_to_start, 0);
        assert!(snapshot.active.is_none());
        assert_eq!(snapshot.execution_ms.count, 128);
        assert!(serde_json::to_vec(&snapshot).unwrap().len() < 4096);
    }
}
