//! Read-only replay of verified event-ledger sessions.
//!
//! Replay emits the original [`EventEnvelope`] values. It has no device or
//! client-routing handle, so loading a capture cannot write to live hardware.

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ledger::{read_verified, EventEnvelope, SchemaError};

/// Slowest accepted original-timing replay speed.
pub const MIN_REPLAY_SPEED: f64 = 0.01;
/// Fastest accepted original-timing replay speed.
pub const MAX_REPLAY_SPEED: f64 = 100.0;
const MAX_SLEEP_CHUNK: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    /// Emit all events without adding delays.
    #[default]
    Immediate,
    /// Preserve monotonic intervals from the capture, scaled by `speed`.
    Original,
    /// Emit events only through [`ReplayCursor::step`].
    Manual,
}

impl fmt::Display for ReplayMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Immediate => "immediate",
            Self::Original => "original",
            Self::Manual => "manual",
        })
    }
}

impl FromStr for ReplayMode {
    type Err = ReplayError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "immediate" => Ok(Self::Immediate),
            "original" => Ok(Self::Original),
            "manual" => Ok(Self::Manual),
            _ => Err(ReplayError::InvalidMode(value.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayOptions {
    pub mode: ReplayMode,
    /// Timing multiplier for `original` mode. Values are bounded so a typo
    /// cannot silently create a near-infinite pause or an unbounded busy run.
    pub speed: f64,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            mode: ReplayMode::Immediate,
            speed: 1.0,
        }
    }
}

impl ReplayOptions {
    pub fn validate(self) -> Result<Self, ReplayError> {
        if !self.speed.is_finite() || !(MIN_REPLAY_SPEED..=MAX_REPLAY_SPEED).contains(&self.speed) {
            return Err(ReplayError::InvalidSpeed {
                speed: self.speed,
                min: MIN_REPLAY_SPEED,
                max: MAX_REPLAY_SPEED,
            });
        }
        Ok(self)
    }
}

/// An immutable, validated capture that can create independent replay cursors.
#[derive(Clone, Debug)]
pub struct ReplaySession {
    source: Option<PathBuf>,
    session_id: Uuid,
    first_seq: u64,
    last_seq: u64,
    events: Arc<[EventEnvelope]>,
}

impl ReplaySession {
    /// Load a segment or session directory through the ledger's verified
    /// reader. The reader verifies segment hashes; this layer additionally
    /// verifies the envelope schema and whole-input ordering invariants.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        let path = path.as_ref();
        let session = read_verified(path).map_err(|error| ReplayError::Read {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
        Self::from_validated_events(session.events, Some(path.to_path_buf()))
    }

    /// Construct a replay from envelopes already held by a trusted caller.
    ///
    /// This validates schema, session identity, sequence continuity, and the
    /// monotonic replay clock. It does not claim disk-integrity verification;
    /// use [`Self::load`] for persisted captures.
    pub fn from_envelopes(events: Vec<EventEnvelope>) -> Result<Self, ReplayError> {
        Self::from_validated_events(events, None)
    }

    fn from_validated_events(
        events: Vec<EventEnvelope>,
        source: Option<PathBuf>,
    ) -> Result<Self, ReplayError> {
        let first = events.first().ok_or(ReplayError::EmptySession)?;
        first.validate()?;
        let session_id = first.session_id;
        let first_seq = first.seq;

        let mut previous_seq = first.seq;
        let mut previous_mono_us = first.mono_us;
        for event in events.iter().skip(1) {
            event.validate()?;
            if event.session_id != session_id {
                return Err(ReplayError::MixedSessions {
                    expected: session_id,
                    actual: event.session_id,
                    seq: event.seq,
                });
            }

            let expected = previous_seq
                .checked_add(1)
                .ok_or(ReplayError::SequenceOverflow { seq: previous_seq })?;
            if event.seq != expected {
                return Err(ReplayError::SequenceDiscontinuity {
                    expected,
                    actual: event.seq,
                });
            }
            if event.mono_us < previous_mono_us {
                return Err(ReplayError::MonotonicClockRegression {
                    previous_seq,
                    previous_mono_us,
                    seq: event.seq,
                    mono_us: event.mono_us,
                });
            }
            previous_seq = event.seq;
            previous_mono_us = event.mono_us;
        }

        Ok(Self {
            source,
            session_id,
            first_seq,
            last_seq: previous_seq,
            events: events.into(),
        })
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn first_seq(&self) -> u64 {
        self.first_seq
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn events(&self) -> &[EventEnvelope] {
        &self.events
    }

    pub fn cursor(&self, options: ReplayOptions) -> Result<ReplayCursor, ReplayError> {
        Ok(ReplayCursor {
            events: Arc::clone(&self.events),
            options: options.validate()?,
            next_index: 0,
        })
    }

    pub fn manual_cursor(&self) -> ReplayCursor {
        ReplayCursor {
            events: Arc::clone(&self.events),
            options: ReplayOptions {
                mode: ReplayMode::Manual,
                speed: 1.0,
            },
            next_index: 0,
        }
    }

    /// Replay immediate/original timing into a callback. The callback receives
    /// an unchanged clone of each verified envelope in sequence order.
    pub async fn play<F>(
        &self,
        options: ReplayOptions,
        mut emit: F,
    ) -> Result<ReplayReport, ReplayError>
    where
        F: FnMut(EventEnvelope),
    {
        if options.mode == ReplayMode::Manual {
            return Err(ReplayError::ManualPlayRequiresStep);
        }

        let started = Instant::now();
        let mut cursor = self.cursor(options)?;
        let mut emitted = 0usize;
        while let Some(event) = cursor.next_event().await? {
            emit(event);
            emitted += 1;
        }

        Ok(ReplayReport {
            emitted,
            first_seq: (emitted != 0).then_some(self.first_seq),
            last_seq: (emitted != 0).then_some(self.last_seq),
            elapsed: started.elapsed(),
        })
    }
}

/// An independent position in a replay session.
#[derive(Clone, Debug)]
pub struct ReplayCursor {
    events: Arc<[EventEnvelope]>,
    options: ReplayOptions,
    next_index: usize,
}

impl ReplayCursor {
    pub fn mode(&self) -> ReplayMode {
        self.options.mode
    }

    pub fn speed(&self) -> f64 {
        self.options.speed
    }

    pub fn position(&self) -> usize {
        self.next_index
    }

    pub fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.next_index)
    }

    pub fn is_finished(&self) -> bool {
        self.next_index == self.events.len()
    }

    /// Return the delay that [`Self::next_event`] will apply. `None` means the
    /// cursor is complete. Manual cursors must use [`Self::step`].
    pub fn delay_before_next(&self) -> Result<Option<Duration>, ReplayError> {
        if self.options.mode == ReplayMode::Manual {
            return Err(ReplayError::ManualStepRequired);
        }
        if self.is_finished() {
            return Ok(None);
        }
        if self.options.mode == ReplayMode::Immediate || self.next_index == 0 {
            return Ok(Some(Duration::ZERO));
        }

        let previous = &self.events[self.next_index - 1];
        let next = &self.events[self.next_index];
        let delta_us = next.mono_us.checked_sub(previous.mono_us).ok_or(
            ReplayError::MonotonicClockRegression {
                previous_seq: previous.seq,
                previous_mono_us: previous.mono_us,
                seq: next.seq,
                mono_us: next.mono_us,
            },
        )?;
        Ok(Some(
            Duration::from_micros(delta_us).div_f64(self.options.speed),
        ))
    }

    /// Yield the next event after applying immediate/original pacing.
    /// Dropping this future cancels a pending delay without advancing the
    /// cursor.
    pub async fn next_event(&mut self) -> Result<Option<EventEnvelope>, ReplayError> {
        let Some(delay) = self.delay_before_next()? else {
            return Ok(None);
        };
        sleep_in_safe_chunks(delay).await;
        let event = self.events[self.next_index].clone();
        self.next_index += 1;
        Ok(Some(event))
    }

    /// Emit up to `count` events from a manual cursor without timing delays.
    pub fn step(&mut self, count: usize) -> Result<Vec<EventEnvelope>, ReplayError> {
        if self.options.mode != ReplayMode::Manual {
            return Err(ReplayError::StepRequiresManualMode);
        }
        let end = self.next_index.saturating_add(count).min(self.events.len());
        let events = self.events[self.next_index..end].to_vec();
        self.next_index = end;
        Ok(events)
    }
}

async fn sleep_in_safe_chunks(mut remaining: Duration) {
    // A valid but adversarial capture can contain a very large monotonic gap.
    // Chunking avoids overflowing the runtime's platform Instant while keeping
    // the future cancellable and preserving the requested total delay.
    while !remaining.is_zero() {
        let chunk = remaining.min(MAX_SLEEP_CHUNK);
        tokio::time::sleep(chunk).await;
        remaining = remaining.saturating_sub(chunk);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayReport {
    pub emitted: usize,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub elapsed: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("could not read verified replay source {path}: {detail}")]
    Read { path: PathBuf, detail: String },
    #[error("replay source contains no events")]
    EmptySession,
    #[error("event schema validation failed: {0}")]
    Schema(#[from] SchemaError),
    #[error("replay contains session {actual} at sequence {seq}, expected {expected}")]
    MixedSessions {
        expected: Uuid,
        actual: Uuid,
        seq: u64,
    },
    #[error("replay sequence is discontinuous: expected {expected}, got {actual}")]
    SequenceDiscontinuity { expected: u64, actual: u64 },
    #[error("replay sequence cannot continue after {seq}")]
    SequenceOverflow { seq: u64 },
    #[error(
        "replay monotonic clock regressed: seq {previous_seq} was {previous_mono_us}us, seq {seq} is {mono_us}us"
    )]
    MonotonicClockRegression {
        previous_seq: u64,
        previous_mono_us: u64,
        seq: u64,
        mono_us: u64,
    },
    #[error("replay speed {speed} is invalid; expected a finite value in {min}..={max}")]
    InvalidSpeed { speed: f64, min: f64, max: f64 },
    #[error("unknown replay mode {0:?}; expected immediate, original, or manual")]
    InvalidMode(String),
    #[error("manual replay does not auto-run; use ReplayCursor::step")]
    ManualPlayRequiresStep,
    #[error("manual replay requires ReplayCursor::step")]
    ManualStepRequired,
    #[error("ReplayCursor::step is available only in manual mode")]
    StepRequiresManualMode,
}
