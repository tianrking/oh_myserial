//! Versioned, append-only serial event ledger.
//!
//! The ledger deliberately separates its bounded observation ring from its
//! optional append-only persistence. A disk failure marks persistence as
//! degraded but never prevents the just-created event from reaching the ring
//! and live subscribers. Callers can inspect [`LedgerError::recorded_event`]
//! to distinguish that case from a rejected append.

mod schema;
mod store;

use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Instant};

use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Notify};
use uuid::Uuid;

pub use schema::{
    BytesPayload, ConnectionPayload, ConnectionState, ControlPayload, EventEnvelope, EventFilter,
    EventPayload, EventType, GapCertainty, GapPayload, GapScope, SchemaError, TxPayload,
    DEFAULT_PORT_ID, EVENT_SCHEMA, EVENT_VERSION,
};
pub use store::{
    export_session_ndjson, read_session, read_verified, recover_stale_sessions, verify_session,
    ActiveSession, LedgerReadError, RecoveryFailure, RecoveryReport, SegmentVerification,
    SessionRead, StaleRecoveryScan, StoreOptions, VerificationReport,
};

use store::{OpenedStore, SegmentStore};

const DEFAULT_QUERY_LIMIT: usize = 1_000;
const MAX_QUERY_LIMIT: usize = 100_000;

#[derive(Clone, Debug)]
pub struct MemoryOptions {
    pub max_events: usize,
    pub max_bytes: usize,
}

impl Default for MemoryOptions {
    fn default() -> Self {
        Self {
            max_events: 65_536,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

impl MemoryOptions {
    fn validate(&self) -> Result<(), LedgerError> {
        if self.max_events == 0 {
            return Err(LedgerError::InvalidOptions(
                "memory max_events must be greater than zero".to_owned(),
            ));
        }
        if self.max_bytes == 0 {
            return Err(LedgerError::InvalidOptions(
                "memory max_bytes must be greater than zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LedgerOptions {
    /// Reopen this session and recover its stale `.open` segment when present.
    /// `None` creates a fresh session and never touches another session's files.
    pub session_id: Option<Uuid>,
    pub memory: MemoryOptions,
    pub stream_capacity: usize,
    pub store: Option<StoreOptions>,
}

impl Default for LedgerOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            memory: MemoryOptions::default(),
            stream_capacity: 4_096,
            store: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EventQuery {
    /// Return events with sequence numbers strictly greater than this cursor.
    pub after_seq: u64,
    /// Optional inclusive upper sequence bound.
    pub through_seq: Option<u64>,
    /// Zero uses the default; values above the hard maximum are clamped.
    pub limit: usize,
    pub filter: EventFilter,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPage {
    pub events: Vec<EventEnvelope>,
    /// True when the requested cursor predates the bounded in-memory prefix.
    /// HTTP adapters can map this condition to 410 Gone.
    pub incomplete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_through_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_available_seq: Option<u64>,
    pub newest_seq: u64,
    pub next_after_seq: u64,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceState {
    Disabled,
    Active,
    Degraded,
    Sealed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerStatus {
    pub session_id: Uuid,
    pub newest_seq: u64,
    pub oldest_available_seq: Option<u64>,
    pub retained_events: usize,
    pub retained_bytes: usize,
    pub evicted_events: u64,
    pub persistence: PersistenceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence_directory: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence_error: Option<String>,
    pub sealed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_recovery: Option<StaleRecoveryScan>,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("invalid ledger options: {0}")]
    InvalidOptions(String),
    #[error("event is invalid: {0}")]
    InvalidEvent(#[from] SchemaError),
    #[error("ledger has already been sealed")]
    Sealed,
    #[error("session sequence space is exhausted")]
    SequenceExhausted,
    #[error("could not open event store: {0}")]
    OpenStore(String),
    #[error("event {event_seq} is in the ring, but persistence is degraded: {detail}")]
    PersistenceDegraded {
        event_seq: u64,
        event: Box<EventEnvelope>,
        detail: String,
    },
    #[error("could not seal event store: {0}")]
    SealFailed(String),
    #[error("could not checkpoint event store: {0}")]
    CheckpointFailed(String),
}

impl LedgerError {
    /// Returns the event when append succeeded in memory/live delivery but its
    /// durable write failed or persistence had already degraded.
    pub fn recorded_event(&self) -> Option<&EventEnvelope> {
        match self {
            Self::PersistenceDegraded { event, .. } => Some(event),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Ledger {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    live: broadcast::Sender<EventEnvelope>,
    notify: Notify,
}

struct State {
    session_id: Uuid,
    started: Instant,
    next_seq: u64,
    newest_seq: u64,
    ring: VecDeque<RetainedEvent>,
    ring_bytes: usize,
    evicted_events: u64,
    memory: MemoryOptions,
    store: Option<SegmentStore>,
    persistence_state: PersistenceState,
    persistence_directory: Option<PathBuf>,
    persistence_error: Option<String>,
    recovery: Option<RecoveryReport>,
    stale_recovery: Option<StaleRecoveryScan>,
    sealed: bool,
}

struct RetainedEvent {
    event: EventEnvelope,
    bytes: usize,
}

impl Ledger {
    pub fn memory(memory: MemoryOptions) -> Result<Self, LedgerError> {
        Self::open(LedgerOptions {
            memory,
            ..LedgerOptions::default()
        })
    }

    pub fn open(options: LedgerOptions) -> Result<Self, LedgerError> {
        options.memory.validate()?;
        if options.stream_capacity == 0 {
            return Err(LedgerError::InvalidOptions(
                "stream_capacity must be greater than zero".to_owned(),
            ));
        }

        let session_id = options.session_id.unwrap_or_else(Uuid::new_v4);
        let (live, _) = broadcast::channel(options.stream_capacity);
        let mut ring = VecDeque::new();
        let mut ring_bytes = 0;
        let mut evicted_events = 0;
        let mut next_seq = 1;
        let mut newest_seq = 0;
        let mut persistence_directory = None;
        let mut recovery = None;
        let mut stale_recovery = None;

        let store = if let Some(store_options) = options.store {
            persistence_directory = Some(store_options.directory.clone());
            if options.session_id.is_none() {
                let scan = recover_stale_sessions(&store_options)
                    .map_err(|error| LedgerError::OpenStore(error.to_string()))?;
                if !scan.is_empty() {
                    stale_recovery = Some(scan);
                }
            }
            let OpenedStore {
                store,
                prior_events,
                recovery: opened_recovery,
            } = SegmentStore::open(session_id, store_options)
                .map_err(|error| LedgerError::OpenStore(error.to_string()))?;
            for event in prior_events {
                newest_seq = event.seq;
                next_seq = newest_seq.saturating_add(1);
                retain_event(
                    &mut ring,
                    &mut ring_bytes,
                    &mut evicted_events,
                    &options.memory,
                    event,
                );
            }
            recovery = opened_recovery;
            Some(store)
        } else {
            None
        };

        Ok(Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    session_id,
                    started: Instant::now(),
                    next_seq,
                    newest_seq,
                    ring,
                    ring_bytes,
                    evicted_events,
                    memory: options.memory,
                    store,
                    persistence_state: if persistence_directory.is_some() {
                        PersistenceState::Active
                    } else {
                        PersistenceState::Disabled
                    },
                    persistence_directory,
                    persistence_error: None,
                    recovery,
                    stale_recovery,
                    sealed: false,
                }),
                live,
                notify: Notify::new(),
            }),
        })
    }

    /// Append and serialize an event assignment, durable write, and ring update.
    ///
    /// The returned error may still contain a recorded event. See
    /// [`LedgerError::recorded_event`].
    pub fn append(
        &self,
        connection_epoch: u64,
        payload: EventPayload,
    ) -> Result<EventEnvelope, LedgerError> {
        let (event, persistence_error) = {
            let mut state = self.shared.state.lock();
            if state.sealed {
                return Err(LedgerError::Sealed);
            }
            if state.newest_seq == u64::MAX {
                return Err(LedgerError::SequenceExhausted);
            }

            let seq = state.next_seq;
            let mono_us = state
                .started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            let event = EventEnvelope::new(
                state.session_id,
                seq,
                Utc::now(),
                mono_us,
                connection_epoch,
                payload,
            );
            event.validate()?;
            state.next_seq = seq + 1;
            state.newest_seq = seq;

            let persistence_error = if state.persistence_state == PersistenceState::Degraded {
                state.persistence_error.clone()
            } else if let Some(store) = state.store.as_mut() {
                match store.append(&event) {
                    Ok(()) => None,
                    Err(error) => {
                        let detail = error.to_string();
                        store.mark_degraded();
                        state.persistence_state = PersistenceState::Degraded;
                        state.persistence_error = Some(detail.clone());
                        Some(detail)
                    }
                }
            } else {
                None
            };

            state.retain(event.clone());
            // broadcast::Sender::send is non-blocking. Keeping it in the same
            // serialized critical section guarantees subscribers see seq N
            // before seq N+1 even when append is called from many threads.
            let _ = self.shared.live.send(event.clone());
            (event, persistence_error)
        };

        // Broadcast lag is a delivery concern for that subscriber. It must not
        // fabricate a canonical ledger Gap event or consume a sequence number.
        self.shared.notify.notify_waiters();

        if let Some(detail) = persistence_error {
            Err(LedgerError::PersistenceDegraded {
                event_seq: event.seq,
                event: Box::new(event),
                detail,
            })
        } else {
            Ok(event)
        }
    }

    pub fn query(&self, query: EventQuery) -> QueryPage {
        let state = self.shared.state.lock();
        let limit = if query.limit == 0 {
            DEFAULT_QUERY_LIMIT
        } else {
            query.limit.min(MAX_QUERY_LIMIT)
        };
        let oldest = state.ring.front().map(|entry| entry.event.seq);
        let requested_next = query.after_seq.saturating_add(1);
        let incomplete = state.newest_seq >= requested_next
            && oldest.is_none_or(|oldest_seq| requested_next < oldest_seq);
        let missing_through_seq = incomplete
            .then(|| oldest.map_or(state.newest_seq, |oldest_seq| oldest_seq.saturating_sub(1)));

        let mut events = Vec::new();
        let mut cursor = query.after_seq;
        for retained in &state.ring {
            let event = &retained.event;
            if event.seq <= query.after_seq {
                continue;
            }
            if query.through_seq.is_some_and(|through| event.seq > through) {
                break;
            }
            cursor = event.seq;
            if query.filter.matches(event) {
                events.push(event.clone());
                if events.len() == limit {
                    break;
                }
            }
        }

        let upper = query.through_seq.unwrap_or(state.newest_seq);
        let has_more = state.ring.iter().any(|entry| {
            entry.event.seq > cursor
                && entry.event.seq <= upper
                && query.filter.matches(&entry.event)
        });

        QueryPage {
            events,
            incomplete,
            missing_through_seq,
            oldest_available_seq: oldest,
            newest_seq: state.newest_seq,
            next_after_seq: cursor,
            has_more,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.shared.live.subscribe()
    }

    /// Subscribe and snapshot the high-water mark while holding the ledger
    /// state lock. Adapters use this pair to avoid the race where an event is
    /// appended between an ordinary `subscribe()` and `status()` call.
    pub fn subscribe_with_status(&self) -> (broadcast::Receiver<EventEnvelope>, LedgerStatus) {
        let state = self.shared.state.lock();
        let receiver = self.shared.live.subscribe();
        let status = LedgerStatus {
            session_id: state.session_id,
            newest_seq: state.newest_seq,
            oldest_available_seq: state.ring.front().map(|entry| entry.event.seq),
            retained_events: state.ring.len(),
            retained_bytes: state.ring_bytes,
            evicted_events: state.evicted_events,
            persistence: state.persistence_state,
            persistence_directory: state.persistence_directory.clone(),
            persistence_error: state.persistence_error.clone(),
            sealed: state.sealed,
            recovery: state.recovery.clone(),
            stale_recovery: state.stale_recovery.clone(),
        };
        (receiver, status)
    }

    /// Wait until at least one canonical event exists after `seq` and return
    /// the current newest sequence. This double-check loop is race-free with
    /// respect to notifications arriving between observation and waiting.
    pub async fn wait_for_newer_than(&self, seq: u64) -> u64 {
        loop {
            let notified = self.shared.notify.notified();
            let newest = self.shared.state.lock().newest_seq;
            if newest > seq || self.shared.state.lock().sealed {
                return newest;
            }
            notified.await;
        }
    }

    pub fn status(&self) -> LedgerStatus {
        let state = self.shared.state.lock();
        LedgerStatus {
            session_id: state.session_id,
            newest_seq: state.newest_seq,
            oldest_available_seq: state.ring.front().map(|entry| entry.event.seq),
            retained_events: state.ring.len(),
            retained_bytes: state.ring_bytes,
            evicted_events: state.evicted_events,
            persistence: state.persistence_state,
            persistence_directory: state.persistence_directory.clone(),
            persistence_error: state.persistence_error.clone(),
            sealed: state.sealed,
            recovery: state.recovery.clone(),
            stale_recovery: state.stale_recovery.clone(),
        }
    }

    pub fn seal(&self) -> Result<(), LedgerError> {
        let result = {
            let mut state = self.shared.state.lock();
            if state.sealed {
                return Ok(());
            }
            state.sealed = true;
            let result = if let Some(store) = state.store.as_mut() {
                store.seal()
            } else {
                Ok(())
            };
            match &result {
                Ok(()) if state.persistence_state == PersistenceState::Active => {
                    state.persistence_state = PersistenceState::Sealed;
                }
                Err(error) => {
                    state.persistence_state = PersistenceState::Degraded;
                    state.persistence_error = Some(error.to_string());
                }
                _ => {}
            }
            result
        };
        self.shared.notify.notify_waiters();
        result.map_err(|error| LedgerError::SealFailed(error.to_string()))
    }

    /// Seal the current non-empty segment without closing the ledger. The next
    /// append lazily creates a new chained `.open` segment, making all events
    /// through this call available to independent readers/exporters.
    pub fn checkpoint(&self) -> Result<(), LedgerError> {
        let mut state = self.shared.state.lock();
        if state.sealed {
            return Err(LedgerError::Sealed);
        }
        if state.persistence_state == PersistenceState::Degraded {
            return Err(LedgerError::CheckpointFailed(
                state
                    .persistence_error
                    .clone()
                    .unwrap_or_else(|| "persistence is degraded".to_owned()),
            ));
        }
        if let Some(store) = state.store.as_mut() {
            if let Err(error) = store.checkpoint() {
                let detail = error.to_string();
                store.mark_degraded();
                state.persistence_state = PersistenceState::Degraded;
                state.persistence_error = Some(detail.clone());
                return Err(LedgerError::CheckpointFailed(detail));
            }
        }
        Ok(())
    }

    pub fn session_id(&self) -> Uuid {
        self.shared.state.lock().session_id
    }

    pub fn persistence_directory(&self) -> Option<PathBuf> {
        self.shared.state.lock().persistence_directory.clone()
    }

    pub fn read_persisted_session(&self) -> Result<SessionRead, LedgerReadError> {
        let (directory, session_id) = {
            let state = self.shared.state.lock();
            (state.persistence_directory.clone(), state.session_id)
        };
        let directory = directory.ok_or(LedgerReadError::PersistenceDisabled)?;
        read_session(directory, session_id)
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        if !state.sealed {
            if let Some(store) = state.store.as_mut() {
                let _ = store.seal();
            }
            state.sealed = true;
        }
    }
}

impl State {
    fn retain(&mut self, event: EventEnvelope) {
        let bytes = event.encoded_len();
        self.ring.push_back(RetainedEvent { event, bytes });
        self.ring_bytes = self.ring_bytes.saturating_add(bytes);
        while self.ring.len() > self.memory.max_events || self.ring_bytes > self.memory.max_bytes {
            if let Some(evicted) = self.ring.pop_front() {
                self.ring_bytes = self.ring_bytes.saturating_sub(evicted.bytes);
                self.evicted_events = self.evicted_events.saturating_add(1);
            } else {
                break;
            }
        }
    }
}

fn retain_event(
    ring: &mut VecDeque<RetainedEvent>,
    ring_bytes: &mut usize,
    evicted_events: &mut u64,
    limits: &MemoryOptions,
    event: EventEnvelope,
) {
    let bytes = event.encoded_len();
    ring.push_back(RetainedEvent { event, bytes });
    *ring_bytes = ring_bytes.saturating_add(bytes);
    while ring.len() > limits.max_events || *ring_bytes > limits.max_bytes {
        if let Some(evicted) = ring.pop_front() {
            *ring_bytes = ring_bytes.saturating_sub(evicted.bytes);
            *evicted_events = evicted_events.saturating_add(1);
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn tiny_ledger(max_events: usize) -> Ledger {
        Ledger::memory(MemoryOptions {
            max_events,
            max_bytes: 1024 * 1024,
        })
        .unwrap()
    }

    #[test]
    fn sequence_and_query_are_session_ordered() {
        let ledger = tiny_ledger(10);
        let first = ledger.append(2, EventPayload::rx(b"one")).unwrap();
        let second = ledger.append(2, EventPayload::rx(b"two")).unwrap();
        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(first.connection_epoch, 2);

        let page = ledger.query(EventQuery {
            after_seq: 1,
            ..EventQuery::default()
        });
        assert_eq!(page.events, vec![second]);
        assert!(!page.incomplete);
        assert_eq!(page.next_after_seq, 2);
    }

    #[test]
    fn dual_bounded_ring_exposes_an_incomplete_cursor() {
        let ledger = tiny_ledger(2);
        for byte in 0..3 {
            ledger.append(1, EventPayload::rx([byte])).unwrap();
        }
        let page = ledger.query(EventQuery::default());
        assert!(page.incomplete);
        assert_eq!(page.missing_through_seq, Some(1));
        assert_eq!(page.oldest_available_seq, Some(2));
        assert_eq!(page.events.len(), 2);
        assert_eq!(ledger.status().evicted_events, 1);
    }

    #[test]
    fn query_filter_advances_past_nonmatching_events() {
        let ledger = tiny_ledger(10);
        ledger.append(1, EventPayload::rx(b"rx")).unwrap();
        ledger.append(1, EventPayload::tx("writer", b"tx")).unwrap();
        let page = ledger.query(EventQuery {
            filter: EventFilter {
                event_types: BTreeSet::from([EventType::Tx]),
                connection_epoch: Some(1),
                ..EventFilter::default()
            },
            ..EventQuery::default()
        });
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.next_after_seq, 2);
    }

    #[tokio::test]
    async fn subscribers_and_waiters_observe_the_same_event() {
        let ledger = tiny_ledger(10);
        let mut rx = ledger.subscribe();
        let waiting = {
            let ledger = ledger.clone();
            tokio::spawn(async move { ledger.wait_for_newer_than(0).await })
        };
        let event = ledger.append(9, EventPayload::rx(b"hello")).unwrap();
        assert_eq!(waiting.await.unwrap(), 1);
        assert_eq!(rx.recv().await.unwrap(), event);
    }

    #[test]
    fn seal_is_idempotent_and_rejects_new_events() {
        let ledger = tiny_ledger(10);
        ledger.seal().unwrap();
        ledger.seal().unwrap();
        assert!(matches!(
            ledger.append(0, EventPayload::rx([])),
            Err(LedgerError::Sealed)
        ));
        assert!(ledger.status().sealed);
    }

    #[test]
    fn persistence_failure_records_to_ring_and_stays_degraded() {
        let temp = tempfile::TempDir::new().unwrap();
        let ledger = Ledger::open(LedgerOptions {
            store: Some(StoreOptions {
                directory: temp.path().to_path_buf(),
                ..StoreOptions::default()
            }),
            ..LedgerOptions::default()
        })
        .unwrap();
        ledger
            .shared
            .state
            .lock()
            .store
            .as_mut()
            .unwrap()
            .fail_next_append();

        let first_error = ledger.append(1, EventPayload::rx(b"one")).unwrap_err();
        assert_eq!(first_error.recorded_event().unwrap().seq, 1);
        let second_error = ledger.append(1, EventPayload::rx(b"two")).unwrap_err();
        assert_eq!(second_error.recorded_event().unwrap().seq, 2);
        assert_eq!(ledger.query(EventQuery::default()).events.len(), 2);
        assert_eq!(ledger.status().persistence, PersistenceState::Degraded);
    }

    #[test]
    fn concurrent_append_preserves_live_sequence_order() {
        let ledger = tiny_ledger(256);
        let mut live = ledger.subscribe();
        let mut threads = Vec::new();
        for worker in 0..8_u8 {
            let ledger = ledger.clone();
            threads.push(std::thread::spawn(move || {
                for item in 0..16_u8 {
                    ledger.append(1, EventPayload::rx([worker, item])).unwrap();
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        let observed: Vec<u64> = (0..128).map(|_| live.try_recv().unwrap().seq).collect();
        assert_eq!(observed, (1..=128).collect::<Vec<_>>());
    }
}
