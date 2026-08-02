use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex as StdMutex, OnceLock},
};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::EventEnvelope;

const SEGMENT_SCHEMA: &str = "ohmyserial.segment";
const SEGMENT_VERSION: u16 = 1;

#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub directory: PathBuf,
    pub segment_max_bytes: u64,
    pub segment_max_events: u64,
    /// Flush the userspace writer after this many appended events.
    pub flush_every_events: u64,
    /// Call `sync_data` whenever the configured flush threshold is reached.
    pub fsync_on_flush: bool,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("ohmyserial-ledger"),
            segment_max_bytes: 64 * 1024 * 1024,
            segment_max_events: 100_000,
            flush_every_events: 1,
            fsync_on_flush: false,
        }
    }
}

impl StoreOptions {
    fn validate(&self) -> Result<(), StoreError> {
        if self.segment_max_bytes == 0 {
            return Err(StoreError::InvalidOptions(
                "segment_max_bytes must be greater than zero".to_owned(),
            ));
        }
        if self.segment_max_events == 0 {
            return Err(StoreError::InvalidOptions(
                "segment_max_events must be greater than zero".to_owned(),
            ));
        }
        if self.flush_every_events == 0 {
            return Err(StoreError::InvalidOptions(
                "flush_every_events must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub source: PathBuf,
    pub preserved_source: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_segment: Option<PathBuf>,
    pub recovered_events: u64,
    pub discarded_tail_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleRecoveryScan {
    pub recovered: Vec<RecoveryReport>,
    pub active_sessions: Vec<ActiveSession>,
    pub failures: Vec<RecoveryFailure>,
}

impl StaleRecoveryScan {
    pub fn is_empty(&self) -> bool {
        self.recovered.is_empty() && self.active_sessions.is_empty() && self.failures.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSession {
    pub session_id: Uuid,
    pub sources: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryFailure {
    pub session_id: Uuid,
    pub sources: Vec<PathBuf>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentVerification {
    pub path: PathBuf,
    pub session_id: Uuid,
    pub segment_index: u64,
    pub event_count: u64,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub content_sha256: String,
    pub segment_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_segment_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRead {
    pub session_id: Uuid,
    pub events: Vec<EventEnvelope>,
    pub segments: Vec<SegmentVerification>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub session_id: Uuid,
    pub segment_count: u64,
    pub event_count: u64,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_head_sha256: Option<String>,
    pub segments: Vec<SegmentVerification>,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerReadError {
    #[error("persistence is disabled for this ledger")]
    PersistenceDisabled,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid ledger segment {path}: {detail}")]
    InvalidSegment { path: PathBuf, detail: String },
    #[error("no sealed ledger segments found at {0}")]
    NoSegments(PathBuf),
    #[error("directory contains more than one session; choose a session explicitly")]
    MultipleSessions,
}

impl LedgerReadError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    fn invalid(path: impl Into<PathBuf>, detail: impl Into<String>) -> Self {
        Self::InvalidSegment {
            path: path.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("invalid store options: {0}")]
    InvalidOptions(String),
    #[error("session {session_id} is already locked at {path}")]
    SessionLocked { session_id: Uuid, path: PathBuf },
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Read(#[from] LedgerReadError),
    #[error("event serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event session/sequence mismatch: {0}")]
    Sequence(String),
    #[error("store is degraded and its .open source has been preserved")]
    Degraded,
    #[error("store is already sealed")]
    Sealed,
    #[error("unsafe recovery state: {0}")]
    Recovery(String),
    #[cfg(test)]
    #[error("injected persistence failure")]
    InjectedFailure,
}

impl StoreError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum DiskRecord {
    Header {
        schema: String,
        version: u16,
        session_id: Uuid,
        segment_index: u64,
        created_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_segment_sha256: Option<String>,
    },
    Event {
        event: EventEnvelope,
    },
    Footer {
        schema: String,
        version: u16,
        session_id: Uuid,
        segment_index: u64,
        event_count: u64,
        first_seq: Option<u64>,
        last_seq: Option<u64>,
        content_sha256: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_segment_sha256: Option<String>,
    },
}

pub(crate) struct OpenedStore {
    pub store: SegmentStore,
    pub prior_events: Vec<EventEnvelope>,
    pub recovery: Option<RecoveryReport>,
}

pub(crate) struct SegmentStore {
    session_id: Uuid,
    options: StoreOptions,
    _lock: SessionLock,
    current: Option<OpenSegment>,
    next_segment_index: u64,
    expected_next_seq: u64,
    previous_segment_sha256: Option<String>,
    degraded: bool,
    sealed: bool,
    #[cfg(test)]
    fail_next_append: bool,
}

struct OpenSegment {
    path: PathBuf,
    writer: BufWriter<File>,
    segment_index: u64,
    hasher: Sha256,
    bytes_written: u64,
    event_count: u64,
    first_seq: Option<u64>,
    last_seq: Option<u64>,
    events_since_flush: u64,
}

struct SessionLock {
    file: File,
    path: PathBuf,
    process_key: PathBuf,
}

static PROCESS_LOCKS: OnceLock<StdMutex<BTreeSet<PathBuf>>> = OnceLock::new();

impl SessionLock {
    fn acquire(directory: &Path, session_id: Uuid) -> Result<Self, StoreError> {
        let path = directory.join(format!("session-{session_id}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| StoreError::io(&path, error))?;
        let process_key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        {
            let mut locks = PROCESS_LOCKS
                .get_or_init(|| StdMutex::new(BTreeSet::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !locks.insert(process_key.clone()) {
                return Err(StoreError::SessionLocked {
                    session_id,
                    path: path.clone(),
                });
            }
        }
        let lock_result = FileExt::try_lock_exclusive(&file).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                StoreError::SessionLocked {
                    session_id,
                    path: path.clone(),
                }
            } else {
                StoreError::io(&path, error)
            }
        });
        if let Err(error) = lock_result {
            PROCESS_LOCKS
                .get_or_init(|| StdMutex::new(BTreeSet::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&process_key);
            return Err(error);
        }

        let mut lock = Self {
            file,
            path,
            process_key,
        };

        lock.file
            .set_len(0)
            .map_err(|error| StoreError::io(&lock.path, error))?;
        let owner = format!("pid={} opened_at={}\n", std::process::id(), Utc::now());
        lock.file
            .write_all(owner.as_bytes())
            .and_then(|()| lock.file.sync_data())
            .map_err(|error| StoreError::io(&lock.path, error))?;
        Ok(lock)
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
        PROCESS_LOCKS
            .get_or_init(|| StdMutex::new(BTreeSet::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.process_key);
        // The tiny lock file is deliberately retained. Removing it after
        // unlocking races with another process locking the same inode.
    }
}

impl SegmentStore {
    pub(crate) fn open(session_id: Uuid, options: StoreOptions) -> Result<OpenedStore, StoreError> {
        options.validate()?;
        fs::create_dir_all(&options.directory)
            .map_err(|error| StoreError::io(&options.directory, error))?;
        let lock = SessionLock::acquire(&options.directory, session_id)?;

        let before = read_session_allow_empty(&options.directory, session_id)?;
        let mut next_segment_index = before
            .segments
            .last()
            .map_or(0, |segment| segment.segment_index.saturating_add(1));
        let previous_hash = before
            .segments
            .last()
            .map(|segment| segment.segment_sha256.clone());
        let stale = list_open_segments(&options.directory, session_id)?;
        if stale.len() > 1 {
            return Err(StoreError::Recovery(format!(
                "found {} .open segments for session {session_id}; refusing ambiguous recovery",
                stale.len()
            )));
        }

        let recovery = if let Some((index, path)) = stale.into_iter().next() {
            if index != next_segment_index {
                return Err(StoreError::Recovery(format!(
                    "stale segment index {index} does not follow sealed index {}",
                    next_segment_index.saturating_sub(1)
                )));
            }
            let expected_next_seq = before
                .events
                .last()
                .map_or(1, |event| event.seq.saturating_add(1));
            let report = recover_open_segment(
                &path,
                &options.directory,
                session_id,
                index,
                previous_hash.as_deref(),
                expected_next_seq,
            )?;
            next_segment_index = next_segment_index.saturating_add(1);
            Some(report)
        } else {
            None
        };

        let after = read_session_allow_empty(&options.directory, session_id)?;
        let expected_next_seq = after
            .events
            .last()
            .map_or(1, |event| event.seq.saturating_add(1));
        let previous_segment_sha256 = after
            .segments
            .last()
            .map(|segment| segment.segment_sha256.clone());
        if let Some(last) = after.segments.last() {
            next_segment_index = last.segment_index.saturating_add(1);
        }

        Ok(OpenedStore {
            prior_events: after.events,
            recovery,
            store: Self {
                session_id,
                options,
                _lock: lock,
                current: None,
                next_segment_index,
                expected_next_seq,
                previous_segment_sha256,
                degraded: false,
                sealed: false,
                #[cfg(test)]
                fail_next_append: false,
            },
        })
    }

    pub(crate) fn append(&mut self, event: &EventEnvelope) -> Result<(), StoreError> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_append) {
            return Err(StoreError::InjectedFailure);
        }
        if self.degraded {
            return Err(StoreError::Degraded);
        }
        if self.sealed {
            return Err(StoreError::Sealed);
        }
        if event.session_id != self.session_id || event.seq != self.expected_next_seq {
            return Err(StoreError::Sequence(format!(
                "expected session {} seq {}, got session {} seq {}",
                self.session_id, self.expected_next_seq, event.session_id, event.seq
            )));
        }

        let record = DiskRecord::Event {
            event: event.clone(),
        };
        let line = json_line(&record)?;
        if self.current.as_ref().is_some_and(|segment| {
            segment.event_count > 0
                && (segment.event_count >= self.options.segment_max_events
                    || segment.bytes_written.saturating_add(line.len() as u64)
                        > self.options.segment_max_bytes)
        }) {
            self.seal_current()?;
        }
        self.ensure_current()?;
        let segment = self.current.as_mut().expect("opened above");
        segment
            .writer
            .write_all(&line)
            .map_err(|error| StoreError::io(&segment.path, error))?;
        segment.hasher.update(&line);
        segment.bytes_written = segment.bytes_written.saturating_add(line.len() as u64);
        segment.event_count = segment.event_count.saturating_add(1);
        segment.first_seq.get_or_insert(event.seq);
        segment.last_seq = Some(event.seq);
        segment.events_since_flush = segment.events_since_flush.saturating_add(1);
        if segment.events_since_flush >= self.options.flush_every_events {
            flush_segment(segment, self.options.fsync_on_flush)?;
        }
        self.expected_next_seq = self.expected_next_seq.saturating_add(1);
        Ok(())
    }

    pub(crate) fn mark_degraded(&mut self) {
        self.degraded = true;
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append(&mut self) {
        self.fail_next_append = true;
    }

    pub(crate) fn checkpoint(&mut self) -> Result<(), StoreError> {
        if self.degraded {
            return Err(StoreError::Degraded);
        }
        if self.sealed {
            return Err(StoreError::Sealed);
        }
        self.seal_current()
    }

    pub(crate) fn seal(&mut self) -> Result<(), StoreError> {
        if self.sealed {
            return Ok(());
        }
        if self.degraded {
            return Err(StoreError::Degraded);
        }
        self.seal_current()?;
        self.sealed = true;
        Ok(())
    }

    fn ensure_current(&mut self) -> Result<(), StoreError> {
        if self.current.is_some() {
            return Ok(());
        }
        let index = self.next_segment_index;
        let path = segment_path(&self.options.directory, self.session_id, index, "open");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| StoreError::io(&path, error))?;
        let header = DiskRecord::Header {
            schema: SEGMENT_SCHEMA.to_owned(),
            version: SEGMENT_VERSION,
            session_id: self.session_id,
            segment_index: index,
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            previous_segment_sha256: self.previous_segment_sha256.clone(),
        };
        let line = json_line(&header)?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&line)
            .and_then(|()| writer.flush())
            .map_err(|error| StoreError::io(&path, error))?;
        if self.options.fsync_on_flush {
            writer
                .get_ref()
                .sync_data()
                .map_err(|error| StoreError::io(&path, error))?;
        }
        let mut hasher = Sha256::new();
        hasher.update(&line);
        self.current = Some(OpenSegment {
            path,
            writer,
            segment_index: index,
            hasher,
            bytes_written: line.len() as u64,
            event_count: 0,
            first_seq: None,
            last_seq: None,
            events_since_flush: 0,
        });
        Ok(())
    }

    fn seal_current(&mut self) -> Result<(), StoreError> {
        let Some(mut segment) = self.current.take() else {
            return Ok(());
        };
        let content_sha256 = hex_digest(segment.hasher.finalize());
        let footer = DiskRecord::Footer {
            schema: SEGMENT_SCHEMA.to_owned(),
            version: SEGMENT_VERSION,
            session_id: self.session_id,
            segment_index: segment.segment_index,
            event_count: segment.event_count,
            first_seq: segment.first_seq,
            last_seq: segment.last_seq,
            content_sha256,
            previous_segment_sha256: self.previous_segment_sha256.clone(),
        };
        let footer_line = json_line(&footer)?;
        segment
            .writer
            .write_all(&footer_line)
            .and_then(|()| segment.writer.flush())
            .and_then(|()| segment.writer.get_ref().sync_all())
            .map_err(|error| StoreError::io(&segment.path, error))?;
        drop(segment.writer);

        let sealed_path = segment_path(
            &self.options.directory,
            self.session_id,
            segment.segment_index,
            "omslog",
        );
        fs::rename(&segment.path, &sealed_path)
            .map_err(|error| StoreError::io(&segment.path, error))?;
        self.previous_segment_sha256 = Some(hash_file(&sealed_path)?);
        self.next_segment_index = segment.segment_index.saturating_add(1);
        Ok(())
    }
}

/// Recover every stale session found in `options.directory` before a fresh hub
/// session starts. A held advisory lock is reported as active and skipped;
/// malformed or ambiguous sessions are reported without deleting their files.
pub fn recover_stale_sessions(
    options: &StoreOptions,
) -> Result<StaleRecoveryScan, LedgerReadError> {
    options
        .validate()
        .map_err(|error| LedgerReadError::invalid(&options.directory, error.to_string()))?;
    fs::create_dir_all(&options.directory)
        .map_err(|error| LedgerReadError::io(&options.directory, error))?;
    let mut sessions: BTreeMap<Uuid, Vec<PathBuf>> = BTreeMap::new();
    let entries = fs::read_dir(&options.directory)
        .map_err(|error| LedgerReadError::io(&options.directory, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| LedgerReadError::io(&options.directory, error))?;
        let name = entry.file_name();
        if let Some((session_id, _)) = parse_any_open_name(&name.to_string_lossy()) {
            sessions.entry(session_id).or_default().push(entry.path());
        }
    }

    let mut report = StaleRecoveryScan::default();
    for (session_id, sources) in sessions {
        match SegmentStore::open(session_id, options.clone()) {
            Ok(opened) => {
                if let Some(recovery) = opened.recovery.clone() {
                    report.recovered.push(recovery);
                }
                drop(opened);
            }
            Err(StoreError::SessionLocked { .. }) => report.active_sessions.push(ActiveSession {
                session_id,
                sources,
            }),
            Err(error) => report.failures.push(RecoveryFailure {
                session_id,
                sources,
                error: error.to_string(),
            }),
        }
    }
    Ok(report)
}

impl Drop for SegmentStore {
    fn drop(&mut self) {
        if !self.degraded && !self.sealed {
            let _ = self.seal();
        }
        // A degraded writer intentionally leaves `.open` untouched for the
        // next locked recovery. No source data is deleted or truncated.
    }
}

fn flush_segment(segment: &mut OpenSegment, fsync: bool) -> Result<(), StoreError> {
    segment
        .writer
        .flush()
        .map_err(|error| StoreError::io(&segment.path, error))?;
    if fsync {
        segment
            .writer
            .get_ref()
            .sync_data()
            .map_err(|error| StoreError::io(&segment.path, error))?;
    }
    segment.events_since_flush = 0;
    Ok(())
}

fn json_line(record: &DiskRecord) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn hash_file(path: &Path) -> Result<String, StoreError> {
    let mut file = File::open(path).map_err(|error| StoreError::io(path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| StoreError::io(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn segment_path(directory: &Path, session_id: Uuid, index: u64, extension: &str) -> PathBuf {
    directory.join(format!(
        "session-{session_id}-segment-{index:020}.{extension}"
    ))
}

fn parse_segment_name(name: &str, session_id: Uuid, extension: &str) -> Option<u64> {
    let prefix = format!("session-{session_id}-segment-");
    let suffix = format!(".{extension}");
    name.strip_prefix(&prefix)?
        .strip_suffix(&suffix)?
        .parse()
        .ok()
}

fn parse_any_open_name(name: &str) -> Option<(Uuid, u64)> {
    let body = name.strip_prefix("session-")?.strip_suffix(".open")?;
    let (session, index) = body.rsplit_once("-segment-")?;
    Some((Uuid::parse_str(session).ok()?, index.parse().ok()?))
}

fn list_open_segments(
    directory: &Path,
    session_id: Uuid,
) -> Result<Vec<(u64, PathBuf)>, StoreError> {
    let entries = fs::read_dir(directory).map_err(|error| StoreError::io(directory, error))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| StoreError::io(directory, error))?;
        let name = entry.file_name();
        if let Some(index) = parse_segment_name(&name.to_string_lossy(), session_id, "open") {
            found.push((index, entry.path()));
        }
    }
    found.sort_by_key(|(index, _)| *index);
    Ok(found)
}

fn recover_open_segment(
    source: &Path,
    directory: &Path,
    session_id: Uuid,
    segment_index: u64,
    expected_previous: Option<&str>,
    mut expected_seq: u64,
) -> Result<RecoveryReport, StoreError> {
    let bytes = fs::read(source).map_err(|error| StoreError::io(source, error))?;
    let mut valid_lines: Vec<&[u8]> = Vec::new();
    let mut events = Vec::new();
    let mut stopped_at_error = None;
    let mut parsed_bytes = 0_usize;
    let mut saw_footer = false;

    for (line_index, line) in complete_lines(&bytes).enumerate() {
        let parsed: DiskRecord = match serde_json::from_slice(trim_line_ending(line)) {
            Ok(record) => record,
            Err(error) => {
                stopped_at_error = Some(format!("line {}: {error}", line_index + 1));
                break;
            }
        };
        match parsed {
            DiskRecord::Header {
                schema,
                version,
                session_id: found_session,
                segment_index: found_index,
                previous_segment_sha256,
                ..
            } if line_index == 0 => {
                if schema != SEGMENT_SCHEMA
                    || version != SEGMENT_VERSION
                    || found_session != session_id
                    || found_index != segment_index
                    || previous_segment_sha256.as_deref() != expected_previous
                {
                    stopped_at_error = Some("header does not match session/chain".to_owned());
                    break;
                }
                valid_lines.push(line);
                parsed_bytes += line.len();
            }
            DiskRecord::Event { event } if !valid_lines.is_empty() && !saw_footer => {
                if let Err(error) = event.validate() {
                    stopped_at_error = Some(format!("line {}: {error}", line_index + 1));
                    break;
                }
                if event.session_id != session_id || event.seq != expected_seq {
                    stopped_at_error = Some(format!(
                        "line {}: expected seq {expected_seq}, got session {} seq {}",
                        line_index + 1,
                        event.session_id,
                        event.seq
                    ));
                    break;
                }
                expected_seq = expected_seq.saturating_add(1);
                events.push(event);
                valid_lines.push(line);
                parsed_bytes += line.len();
            }
            DiskRecord::Footer { .. } if !valid_lines.is_empty() && !saw_footer => {
                saw_footer = true;
                parsed_bytes += line.len();
                // A crash can happen after the footer is synced but before the
                // rename. Rebuilding from the verified prefix is deterministic.
                break;
            }
            _ => {
                stopped_at_error =
                    Some(format!("line {}: unexpected record order", line_index + 1));
                break;
            }
        }
    }

    if valid_lines.is_empty() && stopped_at_error.is_none() {
        stopped_at_error = Some("missing complete valid header line".to_owned());
    }
    let trailing_bytes = bytes.len().saturating_sub(parsed_bytes) as u64;
    let preserved_source = unique_recovery_source(source);
    fs::rename(source, &preserved_source).map_err(|error| StoreError::io(source, error))?;

    let recovered_segment = if valid_lines.is_empty() {
        None
    } else {
        let target = segment_path(directory, session_id, segment_index, "omslog");
        if target.exists() {
            return Err(StoreError::Recovery(format!(
                "recovery target already exists: {}",
                target.display()
            )));
        }
        let temporary = target.with_extension(format!("recovering-{}", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| StoreError::io(&temporary, error))?;
        let mut hasher = Sha256::new();
        for line in &valid_lines {
            file.write_all(line)
                .map_err(|error| StoreError::io(&temporary, error))?;
            hasher.update(line);
        }
        let footer = DiskRecord::Footer {
            schema: SEGMENT_SCHEMA.to_owned(),
            version: SEGMENT_VERSION,
            session_id,
            segment_index,
            event_count: events.len() as u64,
            first_seq: events.first().map(|event| event.seq),
            last_seq: events.last().map(|event| event.seq),
            content_sha256: hex_digest(hasher.finalize()),
            previous_segment_sha256: expected_previous.map(str::to_owned),
        };
        let footer_line = json_line(&footer)?;
        file.write_all(&footer_line)
            .and_then(|()| file.sync_all())
            .map_err(|error| StoreError::io(&temporary, error))?;
        drop(file);
        fs::rename(&temporary, &target).map_err(|error| StoreError::io(&temporary, error))?;
        Some(target)
    };

    Ok(RecoveryReport {
        source: source.to_path_buf(),
        preserved_source,
        recovered_segment,
        recovered_events: events.len() as u64,
        discarded_tail_bytes: trailing_bytes,
        stopped_at_error: if saw_footer { None } else { stopped_at_error },
    })
}

fn unique_recovery_source(source: &Path) -> PathBuf {
    let file_name = source.file_name().unwrap_or_default().to_string_lossy();
    source.with_file_name(format!("{file_name}.recovery-source-{}", Uuid::new_v4()))
}

fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|line| line.ends_with(b"\n"))
}

fn trim_line_ending(mut line: &[u8]) -> &[u8] {
    if line.ends_with(b"\n") {
        line = &line[..line.len() - 1];
    }
    if line.ends_with(b"\r") {
        line = &line[..line.len() - 1];
    }
    line
}

/// Read a sealed session and verify every schema, sequence, content hash, and
/// cross-segment hash-chain link before returning events.
pub fn read_session(
    directory: impl AsRef<Path>,
    session_id: Uuid,
) -> Result<SessionRead, LedgerReadError> {
    read_session_allow_empty(directory.as_ref(), session_id)
}

fn read_session_allow_empty(
    directory: &Path,
    session_id: Uuid,
) -> Result<SessionRead, LedgerReadError> {
    let mut paths = list_sealed_segments(directory, session_id)?;
    paths.sort_by_key(|(index, _)| *index);

    let mut events = Vec::new();
    let mut segments = Vec::new();
    let mut expected_index = 0_u64;
    let mut expected_seq = 1_u64;
    let mut previous_hash: Option<String> = None;
    for (index, path) in paths {
        if index != expected_index {
            return Err(LedgerReadError::invalid(
                &path,
                format!("expected segment index {expected_index}, got {index}"),
            ));
        }
        let verified = verify_segment_file(&path)?;
        if verified.verification.session_id != session_id {
            return Err(LedgerReadError::invalid(&path, "session id mismatch"));
        }
        if verified.verification.segment_index != index {
            return Err(LedgerReadError::invalid(
                &path,
                "filename/header index mismatch",
            ));
        }
        if verified.verification.previous_segment_sha256 != previous_hash {
            return Err(LedgerReadError::invalid(
                &path,
                "previous segment hash does not match chain",
            ));
        }
        for event in &verified.events {
            if event.seq != expected_seq {
                return Err(LedgerReadError::invalid(
                    &path,
                    format!("expected canonical seq {expected_seq}, got {}", event.seq),
                ));
            }
            expected_seq = expected_seq.saturating_add(1);
        }
        previous_hash = Some(verified.verification.segment_sha256.clone());
        events.extend(verified.events);
        segments.push(verified.verification);
        expected_index = expected_index.saturating_add(1);
    }
    Ok(SessionRead {
        session_id,
        events,
        segments,
    })
}

/// Read either one sealed `.omslog` or a directory containing exactly one
/// session. Directories are verified across segments, including hash links and
/// globally contiguous canonical sequence numbers.
pub fn read_verified(path: impl AsRef<Path>) -> Result<SessionRead, LedgerReadError> {
    let path = path.as_ref();
    if path.is_file() {
        let verified = verify_segment_file(path)?;
        return Ok(SessionRead {
            session_id: verified.verification.session_id,
            events: verified.events,
            segments: vec![verified.verification],
        });
    }
    if !path.is_dir() {
        return Err(LedgerReadError::NoSegments(path.to_path_buf()));
    }
    let sessions = discover_sessions(path)?;
    if sessions.is_empty() {
        return Err(LedgerReadError::NoSegments(path.to_path_buf()));
    }
    if sessions.len() != 1 {
        return Err(LedgerReadError::MultipleSessions);
    }
    read_session(path, *sessions.first().expect("checked above"))
}

pub fn verify_session(
    directory: impl AsRef<Path>,
    session_id: Uuid,
) -> Result<VerificationReport, LedgerReadError> {
    let read = read_session(directory, session_id)?;
    Ok(VerificationReport {
        session_id,
        segment_count: read.segments.len() as u64,
        event_count: read.events.len() as u64,
        first_seq: read.events.first().map(|event| event.seq),
        last_seq: read.events.last().map(|event| event.seq),
        chain_head_sha256: read
            .segments
            .last()
            .map(|segment| segment.segment_sha256.clone()),
        segments: read.segments,
    })
}

pub fn export_session_ndjson(
    directory: impl AsRef<Path>,
    session_id: Uuid,
    mut destination: impl Write,
) -> Result<u64, LedgerReadError> {
    let read = read_session(directory, session_id)?;
    for event in &read.events {
        serde_json::to_writer(&mut destination, event)
            .map_err(|error| LedgerReadError::invalid("<export>", error.to_string()))?;
        destination
            .write_all(b"\n")
            .map_err(|error| LedgerReadError::io("<export>", error))?;
    }
    Ok(read.events.len() as u64)
}

struct VerifiedSegment {
    verification: SegmentVerification,
    events: Vec<EventEnvelope>,
}

fn verify_segment_file(path: &Path) -> Result<VerifiedSegment, LedgerReadError> {
    let bytes = fs::read(path).map_err(|error| LedgerReadError::io(path, error))?;
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(LedgerReadError::invalid(
            path,
            "segment is empty or has an incomplete final line",
        ));
    }
    let lines: Vec<&[u8]> = complete_lines(&bytes).collect();
    if lines.len() < 2 {
        return Err(LedgerReadError::invalid(path, "missing header or footer"));
    }
    let header: DiskRecord = serde_json::from_slice(trim_line_ending(lines[0]))
        .map_err(|error| LedgerReadError::invalid(path, format!("invalid header: {error}")))?;
    let (session_id, segment_index, created_at, previous_segment_sha256) = match header {
        DiskRecord::Header {
            schema,
            version,
            session_id,
            segment_index,
            created_at,
            previous_segment_sha256,
        } if schema == SEGMENT_SCHEMA && version == SEGMENT_VERSION => (
            session_id,
            segment_index,
            created_at,
            previous_segment_sha256,
        ),
        _ => return Err(LedgerReadError::invalid(path, "invalid segment header")),
    };
    chrono::DateTime::parse_from_rfc3339(&created_at)
        .map_err(|_| LedgerReadError::invalid(path, "invalid header timestamp"))?;

    let mut hasher = Sha256::new();
    hasher.update(lines[0]);
    let mut events = Vec::new();
    for (offset, line) in lines[1..lines.len() - 1].iter().enumerate() {
        let record: DiskRecord =
            serde_json::from_slice(trim_line_ending(line)).map_err(|error| {
                LedgerReadError::invalid(
                    path,
                    format!("invalid event line {}: {error}", offset + 2),
                )
            })?;
        let DiskRecord::Event { event } = record else {
            return Err(LedgerReadError::invalid(
                path,
                format!("non-event record at line {}", offset + 2),
            ));
        };
        event
            .validate()
            .map_err(|error| LedgerReadError::invalid(path, error.to_string()))?;
        if event.session_id != session_id {
            return Err(LedgerReadError::invalid(path, "event session id mismatch"));
        }
        if let Some(previous) = events.last() {
            let previous: &EventEnvelope = previous;
            if event.seq != previous.seq.saturating_add(1) {
                return Err(LedgerReadError::invalid(
                    path,
                    "event sequence is not contiguous within segment",
                ));
            }
        }
        hasher.update(line);
        events.push(event);
    }

    let footer: DiskRecord = serde_json::from_slice(trim_line_ending(lines[lines.len() - 1]))
        .map_err(|error| LedgerReadError::invalid(path, format!("invalid footer: {error}")))?;
    let (
        footer_session,
        footer_index,
        event_count,
        first_seq,
        last_seq,
        content_sha256,
        footer_previous,
    ) = match footer {
        DiskRecord::Footer {
            schema,
            version,
            session_id,
            segment_index,
            event_count,
            first_seq,
            last_seq,
            content_sha256,
            previous_segment_sha256,
        } if schema == SEGMENT_SCHEMA && version == SEGMENT_VERSION => (
            session_id,
            segment_index,
            event_count,
            first_seq,
            last_seq,
            content_sha256,
            previous_segment_sha256,
        ),
        _ => return Err(LedgerReadError::invalid(path, "invalid segment footer")),
    };
    let computed_content = hex_digest(hasher.finalize());
    let actual_first = events.first().map(|event| event.seq);
    let actual_last = events.last().map(|event| event.seq);
    if footer_session != session_id
        || footer_index != segment_index
        || event_count != events.len() as u64
        || first_seq != actual_first
        || last_seq != actual_last
        || content_sha256 != computed_content
        || footer_previous != previous_segment_sha256
    {
        return Err(LedgerReadError::invalid(
            path,
            "footer metadata or content hash mismatch",
        ));
    }
    let segment_sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex_digest(hasher.finalize())
    };
    Ok(VerifiedSegment {
        verification: SegmentVerification {
            path: path.to_path_buf(),
            session_id,
            segment_index,
            event_count,
            first_seq,
            last_seq,
            content_sha256,
            segment_sha256,
            previous_segment_sha256,
        },
        events,
    })
}

fn list_sealed_segments(
    directory: &Path,
    session_id: Uuid,
) -> Result<Vec<(u64, PathBuf)>, LedgerReadError> {
    let entries = fs::read_dir(directory).map_err(|error| LedgerReadError::io(directory, error))?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| LedgerReadError::io(directory, error))?;
        let name = entry.file_name();
        if let Some(index) = parse_segment_name(&name.to_string_lossy(), session_id, "omslog") {
            found.push((index, entry.path()));
        }
    }
    Ok(found)
}

fn discover_sessions(directory: &Path) -> Result<BTreeSet<Uuid>, LedgerReadError> {
    let entries = fs::read_dir(directory).map_err(|error| LedgerReadError::io(directory, error))?;
    let mut sessions = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|error| LedgerReadError::io(directory, error))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("omslog") {
            continue;
        }
        let verified = verify_segment_file(&path)?;
        sessions.insert(verified.verification.session_id);
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use chrono::{DateTime, Utc};
    use tempfile::TempDir;

    use super::*;
    use crate::ledger::{EventPayload, Ledger, LedgerOptions, MemoryOptions};

    fn options(temp: &TempDir) -> StoreOptions {
        StoreOptions {
            directory: temp.path().to_path_buf(),
            segment_max_bytes: 1024 * 1024,
            segment_max_events: 2,
            flush_every_events: 1,
            fsync_on_flush: true,
        }
    }

    fn ledger(temp: &TempDir, session_id: Option<Uuid>) -> Ledger {
        Ledger::open(LedgerOptions {
            session_id,
            memory: MemoryOptions {
                max_events: 100,
                max_bytes: 1024 * 1024,
            },
            stream_capacity: 16,
            store: Some(options(temp)),
        })
        .unwrap()
    }

    fn event(session_id: Uuid, seq: u64) -> EventEnvelope {
        EventEnvelope::new(
            session_id,
            seq,
            DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            seq,
            1,
            EventPayload::rx([seq as u8]),
        )
    }

    #[test]
    fn rotation_hash_chain_reader_and_export_round_trip() {
        let temp = TempDir::new().unwrap();
        let ledger = ledger(&temp, None);
        let session_id = ledger.session_id();
        for seq in 1..=5 {
            let appended = ledger.append(7, EventPayload::rx([seq])).unwrap();
            assert_eq!(appended.seq, u64::from(seq));
        }
        ledger.checkpoint().unwrap();

        let read = read_session(temp.path(), session_id).unwrap();
        assert_eq!(read.events.len(), 5);
        assert_eq!(read.segments.len(), 3);
        assert_eq!(read.segments[0].previous_segment_sha256, None);
        assert_eq!(
            read.segments[1].previous_segment_sha256.as_deref(),
            Some(read.segments[0].segment_sha256.as_str())
        );
        let verification = verify_session(temp.path(), session_id).unwrap();
        assert_eq!(verification.event_count, 5);
        assert_eq!(verification.last_seq, Some(5));

        let mut exported = Vec::new();
        assert_eq!(
            export_session_ndjson(temp.path(), session_id, &mut exported).unwrap(),
            5
        );
        let exported_events: Vec<EventEnvelope> = exported
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(exported_events, read.events);
        assert_eq!(read_verified(temp.path()).unwrap(), read);
    }

    #[test]
    fn tampering_is_detected_before_events_are_returned() {
        let temp = TempDir::new().unwrap();
        let ledger = ledger(&temp, None);
        let session_id = ledger.session_id();
        ledger.append(1, EventPayload::rx(b"secret")).unwrap();
        ledger.checkpoint().unwrap();
        let path = read_session(temp.path(), session_id).unwrap().segments[0]
            .path
            .clone();
        let mut bytes = fs::read(&path).unwrap();
        let position = bytes.windows(8).position(|v| v == b"c2VjcmV0").unwrap();
        bytes[position] = b'd';
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            read_session(temp.path(), session_id),
            Err(LedgerReadError::InvalidSegment { .. })
        ));
    }

    #[test]
    fn a_second_writer_cannot_lock_the_same_session() {
        let temp = TempDir::new().unwrap();
        let session_id = Uuid::new_v4();
        let first = ledger(&temp, Some(session_id));
        let second = Ledger::open(LedgerOptions {
            session_id: Some(session_id),
            store: Some(options(&temp)),
            ..LedgerOptions::default()
        });
        assert!(matches!(
            second,
            Err(crate::ledger::LedgerError::OpenStore(_))
        ));
        drop(first);
        assert!(Ledger::open(LedgerOptions {
            session_id: Some(session_id),
            store: Some(options(&temp)),
            ..LedgerOptions::default()
        })
        .is_ok());
    }

    #[test]
    fn recovery_preserves_bad_source_and_seals_complete_prefix() {
        let temp = TempDir::new().unwrap();
        let session_id = Uuid::new_v4();
        let opened = SegmentStore::open(session_id, options(&temp)).unwrap();
        let mut store = opened.store;
        store.append(&event(session_id, 1)).unwrap();
        store.mark_degraded();
        drop(store);
        let open_path = segment_path(temp.path(), session_id, 0, "open");
        let mut file = OpenOptions::new().append(true).open(&open_path).unwrap();
        file.write_all(b"{truncated").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let original = fs::read(&open_path).unwrap();

        let scan = recover_stale_sessions(&options(&temp)).unwrap();
        assert_eq!(scan.recovered.len(), 1);
        let recovery = &scan.recovered[0];
        assert_eq!(recovery.recovered_events, 1);
        assert_eq!(recovery.discarded_tail_bytes, 10);
        assert_eq!(fs::read(&recovery.preserved_source).unwrap(), original);
        assert!(recovery.recovered_segment.as_ref().unwrap().exists());
        let read = read_session(temp.path(), session_id).unwrap();
        assert_eq!(read.events, vec![event(session_id, 1)]);
    }

    #[test]
    fn stale_scan_skips_a_live_locked_session() {
        let temp = TempDir::new().unwrap();
        let session_id = Uuid::new_v4();
        let mut store = SegmentStore::open(session_id, options(&temp))
            .unwrap()
            .store;
        store.append(&event(session_id, 1)).unwrap();
        let scan = recover_stale_sessions(&options(&temp)).unwrap();
        assert_eq!(scan.active_sessions.len(), 1);
        assert_eq!(scan.active_sessions[0].session_id, session_id);
        store.mark_degraded();
    }
}
