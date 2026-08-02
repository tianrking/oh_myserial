//! Bounded, linear device workflows.
//!
//! A workflow is deliberately smaller than a scripting language. It can lease,
//! send, wait, expect, assert, and issue a named control operation. There are
//! no loops, branches, retries, variables, network calls, or file access. The
//! runner consumes the canonical event ledger and fails closed when its
//! evidence cursor cannot prove continuity.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::broker::{Broker, ControlCommand};
use crate::ledger::{ConnectionState, EventEnvelope, EventPayload, GapScope, LedgerStatus};

pub const DEFAULT_MAX_STEPS: usize = 32;
pub const DEFAULT_MAX_DURATION_MS: u64 = 30_000;
pub const DEFAULT_MAX_PATTERN_BYTES: usize = 4_096;
pub const DEFAULT_MAX_CAPTURE_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_EVIDENCE_ITEMS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ByteValue {
    Text { text: String },
    Hex { hex: String },
    Base64 { base64: String },
}

impl ByteValue {
    pub fn decode(&self) -> Result<Vec<u8>, WorkflowError> {
        match self {
            Self::Text { text } => Ok(text.as_bytes().to_vec()),
            Self::Hex { hex } => decode_hex(hex),
            Self::Base64 { base64 } => STANDARD
                .decode(base64)
                .map_err(|_| WorkflowError::InvalidBytes("invalid standard base64".into())),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowStep {
    /// Validate/acquire the caller's write lease through the runtime.
    Lease,
    /// Send one atomic byte sequence. The runtime must confirm host write.
    Send {
        bytes: ByteValue,
    },
    /// Wait for one exact byte pattern in the canonical RX stream.
    Expect {
        pattern: ByteValue,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        capture: Option<String>,
    },
    /// Assert a small, explicit runtime fact. This is not an expression.
    Assert {
        assertion: WorkflowAssertion,
    },
    Wait {
        duration_ms: u64,
    },
    /// Named device control, interpreted by the serial owner.
    Control {
        name: String,
        #[serde(default)]
        value: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowAssertion {
    PortConnected,
    ConnectionEpoch { equals: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowLimits {
    pub max_steps: usize,
    pub max_duration_ms: u64,
    pub max_pattern_bytes: usize,
    pub max_capture_bytes: usize,
    pub max_evidence_items: usize,
}

impl Default for WorkflowLimits {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_STEPS,
            max_duration_ms: DEFAULT_MAX_DURATION_MS,
            max_pattern_bytes: DEFAULT_MAX_PATTERN_BYTES,
            max_capture_bytes: DEFAULT_MAX_CAPTURE_BYTES,
            max_evidence_items: DEFAULT_MAX_EVIDENCE_ITEMS,
        }
    }
}

impl WorkflowLimits {
    fn validate(&self) -> Result<(), WorkflowError> {
        if self.max_steps == 0
            || self.max_duration_ms == 0
            || self.max_pattern_bytes == 0
            || self.max_capture_bytes == 0
            || self.max_evidence_items == 0
        {
            return Err(WorkflowError::InvalidLimits);
        }
        Ok(())
    }
}

impl WorkflowDefinition {
    pub fn validate(&self, limits: &WorkflowLimits) -> Result<(), WorkflowError> {
        limits.validate()?;
        if self.id.trim().is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control)
        {
            return Err(WorkflowError::InvalidDefinition(
                "id must be 1..=128 non-control characters".into(),
            ));
        }
        if self.steps.is_empty() || self.steps.len() > limits.max_steps {
            return Err(WorkflowError::StepLimit {
                count: self.steps.len(),
                max: limits.max_steps,
            });
        }
        let mut total_wait = 0_u64;
        for step in &self.steps {
            match step {
                WorkflowStep::Send { bytes } => {
                    let data = bytes.decode()?;
                    if data.is_empty() {
                        return Err(WorkflowError::InvalidDefinition(
                            "send bytes cannot be empty".into(),
                        ));
                    }
                    if data.len() > limits.max_capture_bytes {
                        return Err(WorkflowError::BytesLimit {
                            size: data.len(),
                            max: limits.max_capture_bytes,
                        });
                    }
                }
                WorkflowStep::Expect {
                    pattern,
                    timeout_ms,
                    capture,
                } => {
                    let data = pattern.decode()?;
                    if data.is_empty() {
                        return Err(WorkflowError::InvalidDefinition(
                            "expect pattern cannot be empty".into(),
                        ));
                    }
                    if data.len() > limits.max_pattern_bytes {
                        return Err(WorkflowError::PatternLimit {
                            size: data.len(),
                            max: limits.max_pattern_bytes,
                        });
                    }
                    if let Some(timeout) = timeout_ms {
                        total_wait = total_wait.saturating_add(*timeout);
                    }
                    if capture.as_ref().is_some_and(|name| {
                        name.is_empty() || name.len() > 64 || name.chars().any(char::is_control)
                    }) {
                        return Err(WorkflowError::InvalidDefinition(
                            "capture name is invalid".into(),
                        ));
                    }
                }
                WorkflowStep::Wait { duration_ms } => {
                    total_wait = total_wait.saturating_add(*duration_ms);
                }
                WorkflowStep::Control { name, value } => {
                    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                        return Err(WorkflowError::InvalidDefinition(
                            "control name is invalid".into(),
                        ));
                    }
                    if value.as_ref().is_some_and(|value| value.len() > 1024) {
                        return Err(WorkflowError::InvalidDefinition(
                            "control value is too long".into(),
                        ));
                    }
                }
                WorkflowStep::Lease | WorkflowStep::Assert { .. } => {}
            }
        }
        if total_wait > limits.max_duration_ms {
            return Err(WorkflowError::DurationLimit {
                duration_ms: total_wait,
                max: limits.max_duration_ms,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceCursor {
    pub session_id: Uuid,
    pub port_id: String,
    pub connection_epoch: u64,
    pub seq: u64,
    pub byte_offset: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPortState {
    pub connected: bool,
    pub connection_epoch: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowAuthorization {
    pub can_read: bool,
    pub can_write: bool,
    pub can_control: bool,
    /// Opaque to the workflow and never copied into evidence.
    #[serde(skip)]
    pub lease_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReceipt {
    pub bytes: usize,
    pub tx_seq: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaseReceipt {
    pub expires_ms: u64,
    #[serde(skip)]
    pub lease_token: Option<String>,
}

pub type WorkflowFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, WorkflowError>> + Send + 'a>>;

/// Runtime adapter implemented by the API/hub integration. The adapter owns
/// the real lease and serial command semantics; the runner only supplies one
/// already-authorized operation at a time.
pub trait WorkflowRuntime: Send + Sync {
    fn ledger_status(&self) -> LedgerStatus;
    fn subscribe_with_cursor(
        &self,
    ) -> Result<(broadcast::Receiver<EventEnvelope>, EvidenceCursor), WorkflowError>;
    fn port_state(&self) -> WorkflowPortState;
    fn lease<'a>(
        &'a self,
        actor: &'a str,
        current_token: Option<&'a str>,
    ) -> WorkflowFuture<'a, LeaseReceipt>;
    fn send<'a>(
        &'a self,
        actor: &'a str,
        token: Option<&'a str>,
        bytes: Vec<u8>,
    ) -> WorkflowFuture<'a, SendReceipt>;
    fn control<'a>(
        &'a self,
        actor: &'a str,
        token: Option<&'a str>,
        name: &'a str,
        value: Option<&'a str>,
    ) -> WorkflowFuture<'a, ()>;
}

/// Adapter for the existing broker. It exposes confirmed writes and lease
/// acquisition while keeping opaque credentials out of workflow definitions
/// and evidence. Device control steps are translated into the serial-owner
/// command channel and receive an explicit OS-driver acknowledgement.
#[derive(Clone)]
pub struct BrokerWorkflowRuntime {
    broker: Broker,
}

impl BrokerWorkflowRuntime {
    pub fn new(broker: Broker) -> Self {
        Self { broker }
    }
}

impl WorkflowRuntime for BrokerWorkflowRuntime {
    fn ledger_status(&self) -> LedgerStatus {
        self.broker.ledger().status()
    }

    fn subscribe_with_cursor(
        &self,
    ) -> Result<(broadcast::Receiver<EventEnvelope>, EvidenceCursor), WorkflowError> {
        let ledger = self.broker.ledger();
        let (receiver, status) = ledger.subscribe_with_status();
        let port = self.broker.snapshot().port;
        Ok((
            receiver,
            EvidenceCursor {
                session_id: status.session_id,
                port_id: "default".into(),
                connection_epoch: port.epoch,
                seq: status.newest_seq,
                byte_offset: 0,
            },
        ))
    }

    fn port_state(&self) -> WorkflowPortState {
        let port = self.broker.snapshot().port;
        WorkflowPortState {
            connected: port.connected,
            connection_epoch: port.epoch,
        }
    }

    fn lease<'a>(
        &'a self,
        actor: &'a str,
        current_token: Option<&'a str>,
    ) -> WorkflowFuture<'a, LeaseReceipt> {
        Box::pin(async move {
            let lock = if let Some(token) = current_token {
                self.broker
                    .renew_lock(token)
                    .map_err(WorkflowError::Runtime)?
            } else {
                self.broker
                    .acquire_lock(actor)
                    .map_err(WorkflowError::Runtime)?
            };
            Ok(LeaseReceipt {
                expires_ms: lock.expires_ms,
                lease_token: Some(lock.lease_token),
            })
        })
    }

    fn send<'a>(
        &'a self,
        actor: &'a str,
        token: Option<&'a str>,
        bytes: Vec<u8>,
    ) -> WorkflowFuture<'a, SendReceipt> {
        Box::pin(async move {
            let count = bytes.len();
            self.broker
                .api_write_confirmed_with_lease(actor, Bytes::from(bytes), token)
                .await
                .map_err(WorkflowError::Runtime)?;
            Ok(SendReceipt {
                bytes: count,
                tx_seq: None,
            })
        })
    }

    fn control<'a>(
        &'a self,
        actor: &'a str,
        token: Option<&'a str>,
        name: &'a str,
        value: Option<&'a str>,
    ) -> WorkflowFuture<'a, ()> {
        Box::pin(async move {
            let command = parse_control_command(name, value).map_err(WorkflowError::Runtime)?;
            self.broker
                .serial_control(actor, token, command)
                .await
                .map_err(WorkflowError::Runtime)
        })
    }
}

fn parse_control_command(name: &str, value: Option<&str>) -> Result<ControlCommand, String> {
    let name = name.trim().to_ascii_lowercase();
    match name.as_str() {
        "dtr" => parse_level(value, "dtr").map(ControlCommand::Dtr),
        "rts" => parse_level(value, "rts").map(ControlCommand::Rts),
        "break" => {
            let raw = value.ok_or_else(|| "break control requires duration_ms".to_owned())?;
            let raw = raw.strip_prefix("duration_ms=").unwrap_or(raw).trim();
            let duration_ms = raw
                .parse::<u64>()
                .map_err(|_| "break duration_ms must be an integer".to_owned())?;
            let command = ControlCommand::Break { duration_ms };
            command.validate()?;
            Ok(command)
        }
        _ => Err(format!(
            "unknown control operation '{name}'; expected dtr, rts, or break"
        )),
    }
}

fn parse_level(value: Option<&str>, name: &str) -> Result<bool, String> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("on" | "true" | "1" | "high") => Ok(true),
        Some("off" | "false" | "0" | "low") => Ok(false),
        _ => Err(format!(
            "{name} control requires value on/off (or true/false)"
        )),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepEvidence {
    pub step: usize,
    pub op: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_base64: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowResult {
    pub request_id: String,
    pub run_id: Uuid,
    pub actor: String,
    pub status: String,
    pub cursor: EvidenceCursor,
    pub evidence: Vec<StepEvidence>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    #[error("workflow definition is invalid: {0}")]
    InvalidDefinition(String),
    #[error("workflow limits are invalid")]
    InvalidLimits,
    #[error("workflow has {count} steps; maximum is {max}")]
    StepLimit { count: usize, max: usize },
    #[error("workflow duration {duration_ms}ms exceeds maximum {max}ms")]
    DurationLimit { duration_ms: u64, max: u64 },
    #[error("workflow bytes {size} exceed maximum {max}")]
    BytesLimit { size: usize, max: usize },
    #[error("workflow pattern {size} bytes exceed maximum {max}")]
    PatternLimit { size: usize, max: usize },
    #[error("invalid workflow bytes: {0}")]
    InvalidBytes(String),
    #[error("workflow requires read capability")]
    ReadDenied,
    #[error("workflow requires write capability and a valid lease")]
    WriteDenied,
    #[error("workflow requires control capability and a valid lease")]
    ControlDenied,
    #[error("workflow request_id is already running")]
    RequestInProgress,
    #[error("workflow request_id cannot be empty or contain control characters")]
    InvalidRequestId,
    #[error("workflow was cancelled")]
    Cancelled,
    #[error("workflow timed out")]
    Timeout,
    #[error("workflow evidence cursor is unavailable: {0}")]
    CursorUnavailable(String),
    #[error("workflow evidence cursor gap: expected seq {expected}, got {actual}")]
    EvidenceGap { expected: u64, actual: u64 },
    #[error("workflow event belongs to another ledger session or port")]
    WrongSession,
    #[error("workflow connection epoch changed from {expected} to {actual}")]
    EpochChanged { expected: u64, actual: u64 },
    #[error("workflow connection is not usable")]
    Disconnected,
    #[error("workflow RX observation became uncertain: {0}")]
    RxGap(String),
    #[error("workflow expectation timed out")]
    ExpectTimeout,
    #[error("workflow assertion failed: {0}")]
    Assertion(String),
    #[error("workflow evidence limit exceeded")]
    EvidenceLimit,
    #[error("workflow runtime error: {0}")]
    Runtime(String),
}

#[derive(Clone)]
pub struct WorkflowRunner {
    completed: Arc<parking_lot::Mutex<HashMap<String, WorkflowResult>>>,
    active: Arc<parking_lot::Mutex<std::collections::HashSet<String>>>,
    limits: WorkflowLimits,
}

impl WorkflowRunner {
    pub fn new(limits: WorkflowLimits) -> Result<Self, WorkflowError> {
        limits.validate()?;
        Ok(Self {
            completed: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            active: Arc::new(parking_lot::Mutex::new(std::collections::HashSet::new())),
            limits,
        })
    }

    pub fn limits(&self) -> &WorkflowLimits {
        &self.limits
    }

    pub async fn run<R: WorkflowRuntime + ?Sized>(
        &self,
        runtime: &R,
        request_id: &str,
        definition: &WorkflowDefinition,
        auth: WorkflowAuthorization,
        cancellation: CancellationToken,
    ) -> Result<WorkflowResult, WorkflowError> {
        if request_id.trim().is_empty()
            || request_id.len() > 128
            || request_id.chars().any(char::is_control)
        {
            return Err(WorkflowError::InvalidRequestId);
        }
        definition.validate(&self.limits)?;
        if let Some(previous) = self.completed.lock().get(request_id).cloned() {
            return Ok(previous);
        }
        {
            let mut active = self.active.lock();
            if !active.insert(request_id.to_owned()) {
                return Err(WorkflowError::RequestInProgress);
            }
        }

        let result = self
            .run_inner(runtime, request_id, definition, auth, cancellation)
            .await;
        self.active.lock().remove(request_id);
        if let Ok(ref value) = result {
            self.completed
                .lock()
                .insert(request_id.to_owned(), value.clone());
        }
        result
    }

    async fn run_inner<R: WorkflowRuntime + ?Sized>(
        &self,
        runtime: &R,
        request_id: &str,
        definition: &WorkflowDefinition,
        mut auth: WorkflowAuthorization,
        cancellation: CancellationToken,
    ) -> Result<WorkflowResult, WorkflowError> {
        let (mut events, mut cursor) = runtime.subscribe_with_cursor()?;
        let initial_state = runtime.port_state();
        if cursor.connection_epoch != initial_state.connection_epoch {
            return Err(WorkflowError::EpochChanged {
                expected: cursor.connection_epoch,
                actual: initial_state.connection_epoch,
            });
        }
        let started = Instant::now();
        let run_id = Uuid::new_v4();
        // The actor is generated by the service, never accepted from the DSL.
        let actor = format!("workflow:{run_id}");
        let mut evidence = Vec::new();
        let mut captures = 0usize;

        for (index, step) in definition.steps.iter().enumerate() {
            check_cancel(&cancellation, started, self.limits.max_duration_ms)?;
            let start_seq = Some(cursor.seq);
            let (op, capture) = match step {
                WorkflowStep::Lease => {
                    if !auth.can_write {
                        return Err(WorkflowError::WriteDenied);
                    }
                    let lease = runtime.lease(&actor, auth.lease_token.as_deref()).await?;
                    if lease.lease_token.is_some() {
                        auth.lease_token = lease.lease_token;
                    }
                    ("lease".to_owned(), None)
                }
                WorkflowStep::Send { bytes } => {
                    if !auth.can_write || auth.lease_token.is_none() {
                        return Err(WorkflowError::WriteDenied);
                    }
                    let data = bytes.decode()?;
                    let _ = runtime
                        .send(&actor, auth.lease_token.as_deref(), data)
                        .await?;
                    ("send".to_owned(), None)
                }
                WorkflowStep::Expect {
                    pattern,
                    timeout_ms,
                    capture,
                } => {
                    if !auth.can_read {
                        return Err(WorkflowError::ReadDenied);
                    }
                    let pattern = pattern.decode()?;
                    let timeout = timeout_ms.unwrap_or(self.limits.max_duration_ms);
                    let captured = expect_bytes(
                        &mut events,
                        &mut cursor,
                        runtime,
                        &pattern,
                        timeout,
                        &cancellation,
                        started,
                        self.limits.max_duration_ms,
                    )
                    .await?;
                    let capture = if let Some(name) = capture {
                        let _ = name;
                        captures = captures.saturating_add(captured.len());
                        if captures > self.limits.max_capture_bytes {
                            return Err(WorkflowError::BytesLimit {
                                size: captures,
                                max: self.limits.max_capture_bytes,
                            });
                        }
                        Some(STANDARD.encode(captured))
                    } else {
                        None
                    };
                    ("expect".to_owned(), capture)
                }
                WorkflowStep::Assert { assertion } => {
                    let state = runtime.port_state();
                    match assertion {
                        WorkflowAssertion::PortConnected if !state.connected => {
                            return Err(WorkflowError::Assertion("port is disconnected".into()))
                        }
                        WorkflowAssertion::ConnectionEpoch { equals }
                            if state.connection_epoch != *equals =>
                        {
                            return Err(WorkflowError::Assertion(format!(
                                "expected epoch {equals}, got {}",
                                state.connection_epoch
                            )))
                        }
                        _ => {}
                    }
                    ("assert".to_owned(), None)
                }
                WorkflowStep::Wait { duration_ms } => {
                    sleep_checked(
                        *duration_ms,
                        &cancellation,
                        started,
                        self.limits.max_duration_ms,
                    )
                    .await?;
                    ("wait".to_owned(), None)
                }
                WorkflowStep::Control { name, value } => {
                    if !auth.can_control || auth.lease_token.is_none() {
                        return Err(WorkflowError::ControlDenied);
                    }
                    runtime
                        .control(&actor, auth.lease_token.as_deref(), name, value.as_deref())
                        .await?;
                    ("control".to_owned(), None)
                }
            };
            if evidence.len() >= self.limits.max_evidence_items {
                return Err(WorkflowError::EvidenceLimit);
            }
            evidence.push(StepEvidence {
                step: index,
                op,
                status: "ok".into(),
                start_seq,
                end_seq: Some(cursor.seq),
                capture_base64: capture,
            });
        }

        Ok(WorkflowResult {
            request_id: request_id.to_owned(),
            run_id,
            actor,
            status: "succeeded".into(),
            cursor,
            evidence,
        })
    }
}

fn check_cancel(
    token: &CancellationToken,
    started: Instant,
    max_ms: u64,
) -> Result<(), WorkflowError> {
    if token.is_cancelled() {
        return Err(WorkflowError::Cancelled);
    }
    if started.elapsed() > Duration::from_millis(max_ms) {
        return Err(WorkflowError::Timeout);
    }
    Ok(())
}

async fn sleep_checked(
    duration_ms: u64,
    token: &CancellationToken,
    started: Instant,
    max_ms: u64,
) -> Result<(), WorkflowError> {
    let remaining = Duration::from_millis(max_ms).saturating_sub(started.elapsed());
    let duration = Duration::from_millis(duration_ms).min(remaining);
    tokio::select! {
        _ = token.cancelled() => Err(WorkflowError::Cancelled),
        _ = tokio::time::sleep(duration) => {
            if started.elapsed() > Duration::from_millis(max_ms) { Err(WorkflowError::Timeout) } else { Ok(()) }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn expect_bytes<R: WorkflowRuntime + ?Sized>(
    events: &mut broadcast::Receiver<EventEnvelope>,
    cursor: &mut EvidenceCursor,
    runtime: &R,
    pattern: &[u8],
    timeout_ms: u64,
    cancellation: &CancellationToken,
    started: Instant,
    max_duration_ms: u64,
) -> Result<Vec<u8>, WorkflowError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let table = prefix_table(pattern);
    let mut matched = 0usize;
    let mut origins = std::collections::VecDeque::<(u64, u64)>::with_capacity(pattern.len());
    loop {
        check_cancel(cancellation, started, max_duration_ms)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(WorkflowError::ExpectTimeout);
        }
        let event = tokio::select! {
            _ = cancellation.cancelled() => return Err(WorkflowError::Cancelled),
            result = tokio::time::timeout(remaining, events.recv()) => match result {
                Ok(Ok(event)) => event,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => return Err(WorkflowError::CursorUnavailable("live event subscriber lagged".into())),
                Ok(Err(broadcast::error::RecvError::Closed)) => return Err(WorkflowError::CursorUnavailable("live event stream closed".into())),
                Err(_) => return Err(WorkflowError::ExpectTimeout),
            }
        };
        consume_event(cursor, &event)?;
        match &event.event {
            EventPayload::Rx(bytes) => {
                let data = bytes
                    .decode()
                    .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
                for (offset, byte) in data.iter().copied().enumerate() {
                    while matched > 0 && pattern[matched] != byte {
                        matched = table[matched - 1];
                        while origins.len() > matched {
                            origins.pop_front();
                        }
                    }
                    if pattern[matched] == byte {
                        matched += 1;
                    }
                    origins.push_back((event.seq, offset as u64));
                    while origins.len() > pattern.len() {
                        origins.pop_front();
                    }
                    if matched == pattern.len() {
                        let (start_seq, start_offset) =
                            origins.front().copied().unwrap_or((event.seq, 0));
                        let _ = (start_seq, start_offset);
                        return Ok(pattern.to_vec());
                    }
                }
                cursor.byte_offset = data.len() as u64;
            }
            EventPayload::Connection(connection) => {
                if !matches!(connection.state, ConnectionState::Connected) {
                    return Err(WorkflowError::Disconnected);
                }
                if event.connection_epoch != cursor.connection_epoch {
                    return Err(WorkflowError::EpochChanged {
                        expected: cursor.connection_epoch,
                        actual: event.connection_epoch,
                    });
                }
            }
            EventPayload::Gap(gap) if gap.scope != GapScope::ClientDelivery => {
                return Err(WorkflowError::RxGap(gap.reason.clone()));
            }
            _ => {}
        }
        let state = runtime.port_state();
        if !state.connected {
            return Err(WorkflowError::Disconnected);
        }
        if state.connection_epoch != cursor.connection_epoch {
            return Err(WorkflowError::EpochChanged {
                expected: cursor.connection_epoch,
                actual: state.connection_epoch,
            });
        }
    }
}

fn consume_event(cursor: &mut EvidenceCursor, event: &EventEnvelope) -> Result<(), WorkflowError> {
    if cursor.session_id != event.session_id || cursor.port_id != event.port_id {
        return Err(WorkflowError::WrongSession);
    }
    if event.seq <= cursor.seq {
        return Ok(());
    }
    let expected = cursor.seq.saturating_add(1);
    if event.seq != expected {
        return Err(WorkflowError::EvidenceGap {
            expected,
            actual: event.seq,
        });
    }
    if event.connection_epoch != cursor.connection_epoch {
        return Err(WorkflowError::EpochChanged {
            expected: cursor.connection_epoch,
            actual: event.connection_epoch,
        });
    }
    cursor.seq = event.seq;
    cursor.byte_offset = 0;
    Ok(())
}

fn prefix_table(pattern: &[u8]) -> Vec<usize> {
    let mut table = vec![0; pattern.len()];
    let mut length = 0;
    for index in 1..pattern.len() {
        while length > 0 && pattern[index] != pattern[length] {
            length = table[length - 1];
        }
        if pattern[index] == pattern[length] {
            length += 1;
        }
        table[index] = length;
    }
    table
}

fn decode_hex(input: &str) -> Result<Vec<u8>, WorkflowError> {
    let mut compact = String::with_capacity(input.len());
    for ch in input.chars() {
        if !ch.is_ascii_whitespace() {
            compact.push(ch);
        }
    }
    if compact.is_empty() || !compact.len().is_multiple_of(2) {
        return Err(WorkflowError::InvalidBytes(
            "hex must contain an even number of digits".into(),
        ));
    }
    let bytes = compact.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16);
        let low = (pair[1] as char).to_digit(16);
        match (high, low) {
            (Some(high), Some(low)) => output.push(((high << 4) | low) as u8),
            _ => {
                return Err(WorkflowError::InvalidBytes(
                    "invalid hexadecimal byte string".into(),
                ))
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_values_are_explicit_and_bounded() {
        assert_eq!(
            ByteValue::Text { text: "AT".into() }.decode().unwrap(),
            b"AT"
        );
        assert_eq!(
            ByteValue::Hex {
                hex: "00 ff".into()
            }
            .decode()
            .unwrap(),
            [0, 255]
        );
        assert_eq!(
            ByteValue::Base64 {
                base64: "AP8=".into()
            }
            .decode()
            .unwrap(),
            [0, 255]
        );
    }

    #[test]
    fn definitions_reject_scripting_shapes_and_overlong_patterns() {
        let limits = WorkflowLimits {
            max_pattern_bytes: 2,
            ..WorkflowLimits::default()
        };
        let definition = WorkflowDefinition {
            id: "probe".into(),
            name: None,
            steps: vec![WorkflowStep::Expect {
                pattern: ByteValue::Text { text: "abc".into() },
                timeout_ms: None,
                capture: None,
            }],
        };
        assert!(matches!(
            definition.validate(&limits),
            Err(WorkflowError::PatternLimit { .. })
        ));
        let parsed: Result<WorkflowStep, _> = serde_json::from_str(r#"{"op":"loop","steps":[]}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn kmp_prefix_table_handles_overlap() {
        assert_eq!(prefix_table(b"abab"), vec![0, 0, 1, 2]);
    }
}
