//! Central broker: client registry, RX fan-out, TX arbitration.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::{Condvar, Mutex};
use tokio::sync::{broadcast, mpsc, oneshot, watch, Notify};
use uuid::Uuid;

use crate::config::SerialSettings;
use crate::ledger::{
    BytesPayload, ConnectionPayload, ConnectionState, ControlPayload, EventEnvelope, EventPayload,
    GapCertainty, GapPayload, GapScope, Ledger, LedgerError, MemoryOptions,
};
use crate::observe::{Direction, SessionLog};
use crate::policy::{
    admit_write, AdmitContext, AdmitDecision, FrameAssembler, Policy, SlowClientPolicy, TxMode,
    WriteLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub Uuid);

impl ClientId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: ClientId,
    pub name: String,
    pub kind: String,
    pub can_read: bool,
    pub can_write: bool,
    pub primary_eligible: bool,
    pub connected_at: chrono::DateTime<chrono::Local>,
}

#[derive(Debug, Clone)]
pub struct PortStatus {
    pub path: String,
    pub baud: u32,
    pub connected: bool,
    pub detail: String,
}

/// A physical serial control-line operation. These commands are deliberately
/// separate from byte TX: the serial-owner thread is the only code allowed to
/// touch the OS handle, and every command receives an explicit acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCommand {
    Dtr(bool),
    Rts(bool),
    Break { duration_ms: u64 },
}

impl ControlCommand {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Break { duration_ms } = self {
            if !(1..=1_000).contains(duration_ms) {
                return Err("break duration_ms must be between 1 and 1000".into());
            }
        }
        Ok(())
    }

    fn audit_name(&self) -> &'static str {
        match self {
            Self::Dtr(_) => "dtr",
            Self::Rts(_) => "rts",
            Self::Break { .. } => "break",
        }
    }

    fn audit_value(&self) -> String {
        match self {
            Self::Dtr(level) | Self::Rts(level) => level.to_string(),
            Self::Break { duration_ms } => format!("duration_ms={duration_ms}"),
        }
    }
}

/// Internal command envelope handed to the blocking serial owner.
pub enum SerialControl {
    Command {
        command: ControlCommand,
        acknowledgement: oneshot::Sender<Result<(), String>>,
    },
    Configure {
        settings: SerialSettings,
        acknowledgement: oneshot::Sender<Result<(), String>>,
    },
    BeginHandoff {
        duration_ms: u64,
        acknowledgement: oneshot::Sender<Result<(), String>>,
    },
    ResumeHandoff {
        acknowledgement: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusSnapshot {
    pub port: PortStatusView,
    pub handoff: Option<HandoffStatusView>,
    pub tx_mode: String,
    pub lock_owner: Option<String>,
    pub lock_expires_ms: Option<u64>,
    /// Configured fan-out endpoints (PTY / TCP / WS / HTTP).
    pub endpoints: Vec<EndpointView>,
    pub clients: Vec<ClientView>,
    pub stats: StatsView,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HandoffStatusView {
    pub active: bool,
    pub path: String,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HandoffView {
    pub path: String,
    pub expires_ms: u64,
    /// Opaque bearer returned only by the handoff request and resume endpoint.
    pub handoff_token: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EndpointView {
    pub kind: String,
    pub name: String,
    pub address: String,
    pub can_read: bool,
    pub can_write: bool,
    pub note: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PortStatusView {
    pub path: String,
    pub baud: u32,
    pub connected: bool,
    pub epoch: u64,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub can_read: bool,
    pub can_write: bool,
    pub primary_eligible: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsView {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Total RX chunks discarded for a client by any slow-client policy.
    pub rx_drops: u64,
    pub rx_drop_oldest: u64,
    pub rx_drop_newest: u64,
    pub rx_block_events: u64,
    pub slow_disconnects: u64,
    pub tx_denies: u64,
}

struct ClientSlot {
    info: ClientInfo,
    fanout: Arc<ClientFanout>,
    assembler: FrameAssembler,
}

/// Broker-owned queue in front of the public Tokio receiver. Keeping ownership
/// here is what makes `drop_oldest` implementable; a Tokio `mpsc::Sender` alone
/// can reject a new item but cannot remove an already queued item.
struct ClientFanout {
    capacity: usize,
    state: Mutex<ClientFanoutState>,
    not_empty: Notify,
    not_empty_blocking: Condvar,
    not_full: Condvar,
}

struct ClientFanoutState {
    pending: VecDeque<Bytes>,
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanoutResult {
    Queued,
    DroppedOldest,
    DroppedNewest,
    BlockedThenQueued,
    Disconnected { dropped: u64, blocked: bool },
    Closed,
}

/// Exact-capacity receiver for one client fan-out queue. Bytes remain in the
/// broker-owned queue until the adapter actually asks for them, so policies
/// such as `drop_oldest` cover every undelivered chunk.
pub struct ClientRx {
    fanout: Arc<ClientFanout>,
}

impl ClientRx {
    pub async fn recv(&mut self) -> Option<Bytes> {
        loop {
            // Register before inspecting state so enqueue cannot race with the
            // transition into the await below.
            let notified = self.fanout.not_empty.notified();
            let closed = {
                let mut state = self.fanout.state.lock();
                if let Some(data) = state.pending.pop_front() {
                    self.fanout.not_full.notify_all();
                    return Some(data);
                }
                state.closed
            };
            if closed {
                return None;
            }
            notified.await;
        }
    }

    pub fn blocking_recv(&mut self) -> Option<Bytes> {
        let mut state = self.fanout.state.lock();
        while state.pending.is_empty() && !state.closed {
            self.fanout.not_empty_blocking.wait(&mut state);
        }
        let data = state.pending.pop_front();
        if data.is_some() {
            self.fanout.not_full.notify_all();
        }
        data
    }
}

impl Drop for ClientRx {
    fn drop(&mut self) {
        self.fanout.close();
    }
}

impl ClientFanout {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(ClientFanoutState {
                pending: VecDeque::with_capacity(capacity.max(1)),
                closed: false,
            }),
            not_empty: Notify::new(),
            not_empty_blocking: Condvar::new(),
            not_full: Condvar::new(),
        }
    }

    fn enqueue(
        &self,
        data: Bytes,
        policy: SlowClientPolicy,
        block_timeout: Duration,
    ) -> FanoutResult {
        let mut state = self.state.lock();
        if state.closed {
            return FanoutResult::Closed;
        }

        if state.pending.len() < self.capacity {
            state.pending.push_back(data);
            drop(state);
            self.not_empty.notify_one();
            self.not_empty_blocking.notify_one();
            return FanoutResult::Queued;
        }

        match policy {
            SlowClientPolicy::DropOldest => {
                let _ = state.pending.pop_front();
                state.pending.push_back(data);
                drop(state);
                self.not_empty.notify_one();
                self.not_empty_blocking.notify_one();
                FanoutResult::DroppedOldest
            }
            SlowClientPolicy::DropNewest => FanoutResult::DroppedNewest,
            SlowClientPolicy::DisconnectSlow => {
                let dropped = state.pending.len() as u64 + 1;
                state.closed = true;
                state.pending.clear();
                drop(state);
                self.not_empty.notify_waiters();
                self.not_empty_blocking.notify_all();
                self.not_full.notify_all();
                FanoutResult::Disconnected {
                    dropped,
                    blocked: false,
                }
            }
            SlowClientPolicy::Block => {
                let mut blocked = false;
                while state.pending.len() >= self.capacity && !state.closed {
                    blocked = true;
                    let timed_out = self.not_full.wait_for(&mut state, block_timeout);
                    if timed_out.timed_out() && state.pending.len() >= self.capacity {
                        let dropped = state.pending.len() as u64 + 1;
                        state.closed = true;
                        state.pending.clear();
                        drop(state);
                        self.not_empty.notify_waiters();
                        self.not_empty_blocking.notify_all();
                        self.not_full.notify_all();
                        return FanoutResult::Disconnected {
                            dropped,
                            blocked: true,
                        };
                    }
                }
                if state.closed {
                    return FanoutResult::Closed;
                }
                state.pending.push_back(data);
                drop(state);
                self.not_empty.notify_one();
                self.not_empty_blocking.notify_one();
                if blocked {
                    FanoutResult::BlockedThenQueued
                } else {
                    FanoutResult::Queued
                }
            }
        }
    }

    fn close(&self) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        state.pending.clear();
        drop(state);
        self.not_empty.notify_waiters();
        self.not_empty_blocking.notify_all();
        self.not_full.notify_all();
    }
}

struct BrokerState {
    clients: HashMap<ClientId, ClientSlot>,
    policy: Policy,
    lock: Option<WriteLock>,
    handoff: Option<HandoffState>,
    port: PortStatus,
    connection_epoch: u64,
    history: VecDeque<u8>,
    history_cap: usize,
    endpoints: Vec<EndpointView>,
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
    rx_drops: AtomicU64,
    rx_drop_oldest: AtomicU64,
    rx_drop_newest: AtomicU64,
    rx_block_events: AtomicU64,
    slow_disconnects: AtomicU64,
    tx_denies: AtomicU64,
    persistence_gap_reported: bool,
}

struct HandoffState {
    token: String,
    path: String,
    expires_at: Instant,
}

/// One atomic device-bound write and its completion metadata. Keeping these in
/// the same bounded channel item prevents ACK metadata from drifting away from
/// bytes during shutdown or cancellation races.
pub struct DeviceWrite {
    data: Bytes,
    /// Exact live registration that authorized this write. Client IDs are
    /// random per connection, so reconnecting with the same display name
    /// cannot revive a queued command from a dead connection.
    client_id: ClientId,
    connection_epoch: u64,
    lease_token: Option<String>,
    primary_eligible: bool,
    deadline: Instant,
    actor: String,
    completion: Option<oneshot::Sender<Result<(), String>>>,
}

impl DeviceWrite {
    pub fn bytes(&self) -> &Bytes {
        &self.data
    }
}

/// Handle shared across adapters and API.
#[derive(Clone)]
pub struct Broker {
    state: Arc<Mutex<BrokerState>>,
    /// Device-bound TX frames (already admitted & framed).
    serial_tx: mpsc::Sender<DeviceWrite>,
    /// Fan-out of raw RX for websocket late-join style subscribers (optional).
    rx_broadcast: broadcast::Sender<Bytes>,
    ledger: Ledger,
    log: SessionLog,
    /// Notify waiters when serial connection state changes.
    port_watch: watch::Sender<PortStatus>,
    /// Sender for control-line commands consumed only by the serial owner.
    serial_control: Arc<Mutex<Option<mpsc::Sender<SerialControl>>>>,
}

pub struct ClientRegistration {
    broker: Broker,
    id: ClientId,
}

impl Drop for ClientRegistration {
    fn drop(&mut self) {
        self.broker.unregister_client(self.id);
    }
}

pub struct BrokerSplit {
    pub broker: Broker,
    /// Receiver for bytes that must be written to the real serial port.
    pub serial_tx_rx: mpsc::Receiver<DeviceWrite>,
    pub port_watch_rx: watch::Receiver<PortStatus>,
}

impl Broker {
    fn validate_actor_label(label: &str) -> Result<(), String> {
        if label.trim().is_empty() {
            return Err("client label must not be empty".into());
        }
        if label.len() > 128 {
            return Err("client label must be at most 128 bytes".into());
        }
        if label.chars().any(char::is_control) {
            return Err("client label must not contain control characters".into());
        }
        Ok(())
    }

    #[allow(clippy::new_ret_no_self)] // returns the broker plus its owned device/watch receivers
    pub fn new(
        policy: Policy,
        port: PortStatus,
        log: SessionLog,
        history_cap: usize,
        serial_queue: usize,
    ) -> BrokerSplit {
        let ledger = Ledger::memory(MemoryOptions::default())
            .expect("the built-in ledger memory limits are valid");
        Self::new_with_ledger(policy, port, log, history_cap, serial_queue, ledger)
    }

    pub fn new_with_ledger(
        policy: Policy,
        port: PortStatus,
        log: SessionLog,
        history_cap: usize,
        serial_queue: usize,
        ledger: Ledger,
    ) -> BrokerSplit {
        let (serial_tx, serial_tx_rx) = mpsc::channel(serial_queue.max(16));
        let (rx_broadcast, _) = broadcast::channel(256);
        let (port_watch, port_watch_rx) = watch::channel(port.clone());

        let connection_epoch = u64::from(port.connected);
        let state = BrokerState {
            clients: HashMap::new(),
            policy,
            lock: None,
            handoff: None,
            port,
            connection_epoch,
            history: VecDeque::with_capacity(history_cap.min(1024)),
            history_cap,
            endpoints: Vec::new(),
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            rx_drops: AtomicU64::new(0),
            rx_drop_oldest: AtomicU64::new(0),
            rx_drop_newest: AtomicU64::new(0),
            rx_block_events: AtomicU64::new(0),
            slow_disconnects: AtomicU64::new(0),
            tx_denies: AtomicU64::new(0),
            persistence_gap_reported: false,
        };

        BrokerSplit {
            broker: Broker {
                state: Arc::new(Mutex::new(state)),
                serial_tx,
                rx_broadcast,
                ledger,
                log,
                port_watch,
                serial_control: Arc::new(Mutex::new(None)),
            },
            serial_tx_rx,
            port_watch_rx,
        }
    }

    pub fn log(&self) -> &SessionLog {
        &self.log
    }

    pub fn ledger(&self) -> Ledger {
        self.ledger.clone()
    }

    /// Attach the command channel after all broker state has been created. The
    /// serial owner calls this before it opens a real device, so no control
    /// command can bypass the single-handle ownership boundary.
    pub fn attach_serial_control(&self, sender: mpsc::Sender<SerialControl>) {
        *self.serial_control.lock() = Some(sender);
    }

    /// Remove the owner channel during shutdown. Pending callers receive a
    /// deterministic error instead of waiting for a detached worker.
    pub fn detach_serial_control(&self) {
        *self.serial_control.lock() = None;
    }

    fn handoff_is_active(g: &BrokerState) -> bool {
        g.handoff.is_some()
    }

    fn clear_handoff_if(&self, token: &str) {
        let mut g = self.state.lock();
        if g.handoff
            .as_ref()
            .is_some_and(|handoff| handoff.token == token)
        {
            g.handoff = None;
        }
    }

    /// Ask the owner to close the physical port for a bounded external-tool
    /// handoff. The bearer is generated server-side and never enters the
    /// canonical event ledger or status snapshot.
    pub async fn begin_handoff(
        &self,
        actor: &str,
        lease_token: Option<&str>,
        duration_ms: u64,
    ) -> Result<HandoffView, String> {
        Self::validate_actor_label(actor)?;
        if !(1..=600_000).contains(&duration_ms) {
            return Err("handoff duration_ms must be between 1 and 600000".into());
        }
        let now = Instant::now();
        let (sender, connection_epoch, timeout, path, token) = {
            let mut g = self.state.lock();
            if Self::handoff_is_active(&g) {
                return Err("a serial handoff is already active".into());
            }
            if !g.port.connected {
                return Err("serial port is disconnected; handoff was not queued".into());
            }
            let Some(token) = lease_token else {
                return Err("handoff requires an active write lease token".into());
            };
            if !g
                .lock
                .as_ref()
                .is_some_and(|lock| lock.authorizes(Some(token), now))
            {
                return Err("handoff lease expired, was released, or was replaced".into());
            }
            let Some(sender) = self.serial_control.lock().clone() else {
                return Err("serial owner control channel is unavailable".into());
            };
            let handoff_token = Uuid::new_v4().as_simple().to_string();
            let path = g.port.path.clone();
            g.handoff = Some(HandoffState {
                token: handoff_token.clone(),
                path: path.clone(),
                expires_at: now + Duration::from_millis(duration_ms),
            });
            (
                sender,
                g.connection_epoch,
                Duration::from_millis(g.policy.write_timeout_ms.max(1)),
                path,
                handoff_token,
            )
        };

        let (acknowledgement, result) = oneshot::channel();
        let envelope = SerialControl::BeginHandoff {
            duration_ms,
            acknowledgement,
        };
        let result = match tokio::time::timeout(timeout, sender.send(envelope)).await {
            Ok(Ok(())) => match tokio::time::timeout(timeout, result).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("serial owner dropped the handoff acknowledgement".into()),
                Err(_) => Err("serial handoff acknowledgement timed out".into()),
            },
            Ok(Err(_)) => Err("serial owner control channel is closed".into()),
            Err(_) => Err("serial handoff queue deadline expired".into()),
        };
        if let Err(error) = result {
            self.clear_handoff_if(&token);
            let _ = self.record_event(
                connection_epoch,
                EventPayload::Control(ControlPayload {
                    actor: Some(actor.to_owned()),
                    name: "handoff_rejected".into(),
                    value: Some(error.clone()),
                }),
            );
            return Err(error);
        }

        // A handoff invalidates the old write lease. The external tool must
        // not be able to race queued writes through the hub while it owns the
        // physical handle; a fresh lease can be acquired after resume.
        let released_lease = {
            let mut g = self.state.lock();
            if let Some(handoff) = g.handoff.as_mut() {
                // Start the public TTL only after the owner has actually
                // released the handle and acknowledged the boundary.
                handoff.expires_at = Instant::now() + Duration::from_millis(duration_ms);
            }
            g.lock.take().map(|lock| lock.owner)
        };
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Control(ControlPayload {
                actor: Some(actor.to_owned()),
                name: "handoff_started".into(),
                value: Some(format!("duration_ms={duration_ms}")),
            }),
        );
        if let Some(owner) = released_lease {
            let _ = self.record_event(
                connection_epoch,
                EventPayload::Control(ControlPayload {
                    actor: Some(owner),
                    name: "lease_released_for_handoff".into(),
                    value: None,
                }),
            );
        }
        Ok(HandoffView {
            path,
            expires_ms: duration_ms,
            handoff_token: token,
        })
    }

    /// Resume the owner after a handoff token is presented. Expired tokens
    /// fail closed; the owner independently resumes at the same TTL boundary.
    pub async fn resume_handoff(&self, handoff_token: &str) -> Result<(), String> {
        if handoff_token.is_empty() || handoff_token.len() > 128 {
            return Err("invalid handoff token".into());
        }
        let now = Instant::now();
        let (sender, connection_epoch, timeout) = {
            let mut g = self.state.lock();
            let Some(handoff) = g.handoff.as_ref() else {
                return Err("no active serial handoff".into());
            };
            if handoff.expires_at <= now {
                g.handoff = None;
                return Err("handoff token expired".into());
            }
            if handoff.token != handoff_token {
                return Err("invalid handoff token".into());
            }
            let Some(sender) = self.serial_control.lock().clone() else {
                return Err("serial owner control channel is unavailable".into());
            };
            (
                sender,
                g.connection_epoch,
                Duration::from_millis(g.policy.write_timeout_ms.max(1)),
            )
        };
        let (acknowledgement, result) = oneshot::channel();
        let envelope = SerialControl::ResumeHandoff { acknowledgement };
        let result = match tokio::time::timeout(timeout, sender.send(envelope)).await {
            Ok(Ok(())) => match tokio::time::timeout(timeout, result).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("serial owner dropped the resume acknowledgement".into()),
                Err(_) => Err("serial handoff resume timed out".into()),
            },
            Ok(Err(_)) => Err("serial owner control channel is closed".into()),
            Err(_) => Err("serial handoff resume queue deadline expired".into()),
        };
        result?;
        let _ = self.state.lock().handoff.take();
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Control(ControlPayload {
                actor: None,
                name: "handoff_resumed".into(),
                value: None,
            }),
        );
        Ok(())
    }

    /// Called by the owner when a TTL expires without an explicit resume.
    pub fn expire_handoff(&self) -> bool {
        let mut g = self.state.lock();
        let expired = g
            .handoff
            .as_ref()
            .is_some_and(|handoff| handoff.expires_at <= Instant::now());
        if expired {
            g.handoff = None;
        }
        expired
    }

    pub fn abort_handoff(&self) {
        self.state.lock().handoff = None;
    }

    /// Execute one physical control-line operation under a live write lease.
    /// The lease is intentionally required even when TX mode is not exclusive:
    /// toggling DTR/RTS or asserting BREAK can reset or disrupt hardware.
    pub async fn serial_control(
        &self,
        actor: &str,
        lease_token: Option<&str>,
        command: ControlCommand,
    ) -> Result<(), String> {
        Self::validate_actor_label(actor)?;
        command.validate()?;
        let now = Instant::now();
        let (sender, connection_epoch, timeout) = {
            let g = self.state.lock();
            if Self::handoff_is_active(&g) {
                return Err("serial handoff is active; control is temporarily unavailable".into());
            }
            if !g.port.connected {
                return Err("serial port is disconnected; control was not queued".into());
            }
            let Some(token) = lease_token else {
                return Err("control requires an active write lease token".into());
            };
            if !g
                .lock
                .as_ref()
                .is_some_and(|lock| lock.authorizes(Some(token), now))
            {
                return Err("control lease expired, was released, or was replaced".into());
            }
            let Some(sender) = self.serial_control.lock().clone() else {
                return Err("serial owner control channel is unavailable".into());
            };
            (
                sender,
                g.connection_epoch,
                Duration::from_millis(g.policy.write_timeout_ms.max(1)),
            )
        };

        let (acknowledgement, result) = oneshot::channel();
        let envelope = SerialControl::Command {
            command: command.clone(),
            acknowledgement,
        };
        let result = match tokio::time::timeout(timeout, sender.send(envelope)).await {
            Ok(Ok(())) => match tokio::time::timeout(timeout, result).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("serial owner dropped the control acknowledgement".into()),
                Err(_) => Err("serial control acknowledgement timed out".into()),
            },
            Ok(Err(_)) => Err("serial owner control channel is closed".into()),
            Err(_) => Err("serial control queue deadline expired".into()),
        };

        let (name, value) = (command.audit_name(), command.audit_value());
        match result {
            Ok(()) => {
                let _ = self.record_event(
                    connection_epoch,
                    EventPayload::Control(ControlPayload {
                        actor: Some(actor.to_owned()),
                        name: name.to_owned(),
                        value: Some(value),
                    }),
                );
                Ok(())
            }
            Err(error) => {
                let _ = self.record_event(
                    connection_epoch,
                    EventPayload::Control(ControlPayload {
                        actor: Some(actor.to_owned()),
                        name: format!("{name}_rejected"),
                        value: Some(error.clone()),
                    }),
                );
                Err(error)
            }
        }
    }

    /// Negotiate runtime serial line settings through the serial owner. Like
    /// physical control lines, changing framing can disrupt a live device, so
    /// the caller must hold an opaque write lease.
    pub async fn serial_configure(
        &self,
        actor: &str,
        lease_token: Option<&str>,
        settings: SerialSettings,
    ) -> Result<(), String> {
        Self::validate_actor_label(actor)?;
        settings.validate()?;
        let now = Instant::now();
        let (sender, connection_epoch, timeout) = {
            let g = self.state.lock();
            if Self::handoff_is_active(&g) {
                return Err(
                    "serial handoff is active; configuration is temporarily unavailable".into(),
                );
            }
            if !g.port.connected {
                return Err("serial port is disconnected; configuration was not queued".into());
            }
            let Some(token) = lease_token else {
                return Err("configuration requires an active write lease token".into());
            };
            if !g
                .lock
                .as_ref()
                .is_some_and(|lock| lock.authorizes(Some(token), now))
            {
                return Err("configuration lease expired, was released, or was replaced".into());
            }
            let Some(sender) = self.serial_control.lock().clone() else {
                return Err("serial owner control channel is unavailable".into());
            };
            (
                sender,
                g.connection_epoch,
                Duration::from_millis(g.policy.write_timeout_ms.max(1)),
            )
        };

        let (acknowledgement, result) = oneshot::channel();
        let result = match tokio::time::timeout(
            timeout,
            sender.send(SerialControl::Configure {
                settings: settings.clone(),
                acknowledgement,
            }),
        )
        .await
        {
            Ok(Ok(())) => match tokio::time::timeout(timeout, result).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("serial owner dropped the configuration acknowledgement".into()),
                Err(_) => Err("serial configuration acknowledgement timed out".into()),
            },
            Ok(Err(_)) => Err("serial owner control channel is closed".into()),
            Err(_) => Err("serial configuration queue deadline expired".into()),
        };

        match result {
            Ok(()) => {
                let _ = self.record_event(
                    connection_epoch,
                    EventPayload::Control(ControlPayload {
                        actor: Some(actor.to_owned()),
                        name: "serial_configured".into(),
                        value: Some(format!(
                            "baud={} databits={} parity={} stopbits={} flow={}",
                            settings.baud,
                            settings.databits,
                            settings.parity,
                            settings.stopbits,
                            settings.flow
                        )),
                    }),
                );
                Ok(())
            }
            Err(error) => {
                let _ = self.record_event(
                    connection_epoch,
                    EventPayload::Control(ControlPayload {
                        actor: Some(actor.to_owned()),
                        name: "serial_configure_rejected".into(),
                        value: Some(error.clone()),
                    }),
                );
                Err(error)
            }
        }
    }

    pub fn record_control(
        &self,
        actor: Option<String>,
        name: impl Into<String>,
        value: Option<String>,
    ) -> Result<EventEnvelope, LedgerError> {
        let connection_epoch = self.state.lock().connection_epoch;
        self.record_event(
            connection_epoch,
            EventPayload::Control(ControlPayload {
                actor,
                name: name.into(),
                value,
            }),
        )
    }

    fn record_event(
        &self,
        connection_epoch: u64,
        payload: EventPayload,
    ) -> Result<EventEnvelope, LedgerError> {
        let result = self.ledger.append(connection_epoch, payload);
        if let Err(error @ LedgerError::PersistenceDegraded { .. }) = &result {
            let first = {
                let mut state = self.state.lock();
                if state.persistence_gap_reported {
                    false
                } else {
                    state.persistence_gap_reported = true;
                    true
                }
            };
            if first {
                self.log
                    .event(&format!("ledger_persistence_degraded error={error}"));
                // This canonical warning is intentionally ring/live-only: the
                // store is already degraded, but observers still need a stable
                // sequence marking where durable evidence ceased.
                let _ = self.ledger.append(
                    connection_epoch,
                    EventPayload::Gap(GapPayload {
                        scope: GapScope::Persistence,
                        certainty: GapCertainty::NotDelivered,
                        reason: error.to_string(),
                        bytes: None,
                        actor: None,
                        client_ids: Vec::new(),
                    }),
                );
            }
        }
        result
    }

    pub fn subscribe_rx(&self) -> broadcast::Receiver<Bytes> {
        self.rx_broadcast.subscribe()
    }

    pub fn set_port_status(&self, status: PortStatus) {
        let connection_epoch = {
            let mut g = self.state.lock();
            if status.connected && !g.port.connected {
                g.connection_epoch = g.connection_epoch.saturating_add(1);
            }
            g.port = status.clone();
            g.connection_epoch
        };
        let connection_state = if status.connected {
            ConnectionState::Connected
        } else if status.detail.starts_with("open error:") {
            ConnectionState::OpenFailed
        } else if status.detail == "starting" || status.detail == "reconnecting" {
            ConnectionState::Reconnecting
        } else {
            ConnectionState::Disconnected
        };
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Connection(ConnectionPayload {
                state: connection_state,
                path: status.path.clone(),
                baud: status.baud,
                detail: Some(status.detail.clone()),
            }),
        );
        let _ = self.port_watch.send(status);
    }

    /// Publish configured fan-out endpoints (virtual serial, TCP, WS, …).
    pub fn set_endpoints(&self, endpoints: Vec<EndpointView>) {
        let mut g = self.state.lock();
        g.endpoints = endpoints;
    }

    pub fn register_client(
        &self,
        name: impl Into<String>,
        kind: impl Into<String>,
        can_read: bool,
        can_write: bool,
        queue_cap: Option<usize>,
    ) -> (ClientId, ClientRx) {
        self.register_client_with_primary_eligibility(
            name, kind, can_read, can_write, queue_cap, true,
        )
    }

    fn register_client_with_primary_eligibility(
        &self,
        name: impl Into<String>,
        kind: impl Into<String>,
        can_read: bool,
        can_write: bool,
        queue_cap: Option<usize>,
        primary_eligible: bool,
    ) -> (ClientId, ClientRx) {
        let id = ClientId::new();
        let name = name.into();
        let kind = kind.into();
        let cap = queue_cap
            .unwrap_or_else(|| self.state.lock().policy.client_queue)
            .max(1);
        let fanout = Arc::new(ClientFanout::new(cap));
        let rx = ClientRx {
            fanout: fanout.clone(),
        };

        let info = ClientInfo {
            id,
            name: name.clone(),
            kind: kind.clone(),
            can_read,
            can_write,
            primary_eligible,
            connected_at: chrono::Local::now(),
        };

        let connection_epoch = {
            let mut g = self.state.lock();
            g.clients.insert(
                id,
                ClientSlot {
                    info,
                    fanout,
                    assembler: FrameAssembler::default(),
                },
            );
            g.connection_epoch
        };

        self.log
            .event(&format!("client_join id={id} name={name} kind={kind}"));
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Control(ControlPayload {
                actor: Some(name.clone()),
                name: "client_joined".into(),
                value: Some(format!("id={id} kind={kind}")),
            }),
        );
        (id, rx)
    }

    pub fn unregister_client(&self, id: ClientId) {
        let removed = {
            let mut g = self.state.lock();
            g.clients.remove(&id).map(|slot| (slot, g.connection_epoch))
        };
        if let Some((slot, connection_epoch)) = removed {
            // A lease is a bearer credential with its own expiry. It is not
            // tied to a display name, so an unrelated same-name disconnect
            // must never revoke it.
            slot.fanout.close();
            self.log
                .event(&format!("client_leave id={id} name={}", slot.info.name));
            let _ = self.record_event(
                connection_epoch,
                EventPayload::Control(ControlPayload {
                    actor: Some(slot.info.name),
                    name: "client_left".into(),
                    value: Some(format!("id={id} kind={}", slot.info.kind)),
                }),
            );
        }
    }

    /// RAII cleanup for adapters and request-scoped clients. The registration
    /// is removed even when its async task is cancelled or returns through `?`.
    pub fn client_registration(&self, id: ClientId) -> ClientRegistration {
        ClientRegistration {
            broker: self.clone(),
            id,
        }
    }

    /// Called when device produces data.
    pub fn on_device_rx(&self, data: Bytes) {
        if data.is_empty() {
            return;
        }
        let connection_epoch = self.state.lock().connection_epoch;
        let _ = self.record_event(connection_epoch, EventPayload::rx(&data));
        self.log.log(Direction::Rx, None, &data);
        {
            let g = self.state.lock();
            g.rx_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        }

        let _ = self.rx_broadcast.send(data.clone());

        let (slow, slow_block_timeout, recipients) = {
            let mut g = self.state.lock();
            // history
            if g.history_cap > 0 {
                for b in data.iter() {
                    if g.history.len() >= g.history_cap {
                        g.history.pop_front();
                    }
                    g.history.push_back(*b);
                }
            }
            let recipients = g
                .clients
                .iter()
                .filter(|(_, slot)| slot.info.can_read)
                .map(|(id, slot)| (*id, slot.fanout.clone()))
                .collect::<Vec<_>>();
            (
                g.policy.slow_client,
                Duration::from_millis(g.policy.slow_block_ms),
                recipients,
            )
        };

        // Do not hold BrokerState while `block` applies backpressure. This lets
        // another thread unregister/close the slow client and release the wait.
        let mut dead = Vec::new();
        let mut drop_oldest = 0u64;
        let mut drop_newest = 0u64;
        let mut block_events = 0u64;
        let mut disconnects = 0u64;
        let mut delivery_gaps = Vec::new();
        let block_deadline = Instant::now() + slow_block_timeout;
        for (id, fanout) in recipients {
            let remaining_block_budget = block_deadline.saturating_duration_since(Instant::now());
            match fanout.enqueue(data.clone(), slow, remaining_block_budget) {
                FanoutResult::Queued => {}
                FanoutResult::DroppedOldest => {
                    drop_oldest += 1;
                    delivery_gaps.push(id.to_string());
                }
                FanoutResult::DroppedNewest => {
                    drop_newest += 1;
                    delivery_gaps.push(id.to_string());
                }
                FanoutResult::BlockedThenQueued => block_events += 1,
                FanoutResult::Disconnected { dropped, blocked } => {
                    drop_newest += dropped;
                    block_events += u64::from(blocked);
                    disconnects += 1;
                    if dropped > 0 {
                        delivery_gaps.push(id.to_string());
                    }
                    dead.push(id);
                }
                FanoutResult::Closed => dead.push(id),
            }
        }

        if drop_oldest + drop_newest + block_events + disconnects > 0 {
            let g = self.state.lock();
            g.rx_drops
                .fetch_add(drop_oldest + drop_newest, Ordering::Relaxed);
            g.rx_drop_oldest.fetch_add(drop_oldest, Ordering::Relaxed);
            g.rx_drop_newest.fetch_add(drop_newest, Ordering::Relaxed);
            g.rx_block_events.fetch_add(block_events, Ordering::Relaxed);
            g.slow_disconnects.fetch_add(disconnects, Ordering::Relaxed);
        }
        for id in dead {
            self.unregister_client(id);
        }
        if !delivery_gaps.is_empty() {
            let _ = self.record_event(
                connection_epoch,
                EventPayload::Gap(GapPayload {
                    scope: GapScope::ClientDelivery,
                    certainty: GapCertainty::NotDelivered,
                    reason: format!(
                        "slow-client policy dropped delivery (oldest={drop_oldest}, newest={drop_newest})"
                    ),
                    bytes: Some(BytesPayload::from_bytes(&data)),
                    actor: None,
                    client_ids: delivery_gaps,
                }),
            );
        }
    }

    /// Record that the serial driver could not prove continuous RX
    /// observation. No byte range is invented because the process cannot know
    /// what the device or driver may have dropped.
    pub(crate) fn on_serial_read_gap(&self, reason: impl Into<String>) {
        let connection_epoch = self.state.lock().connection_epoch;
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Gap(GapPayload {
                scope: GapScope::RxObservation,
                certainty: GapCertainty::Unknown,
                reason: reason.into(),
                bytes: None,
                actor: None,
                client_ids: Vec::new(),
            }),
        );
    }

    /// Complete an admitted write after the serial owner actually wrote and
    /// flushed its bytes. TX logs and counters intentionally live here rather
    /// than at queue admission time.
    pub(crate) fn on_device_tx_written(&self, write: DeviceWrite) {
        let evidence = self.record_event(
            write.connection_epoch,
            EventPayload::tx_from(
                write.actor.clone(),
                Some(write.client_id.to_string()),
                &write.data,
            ),
        );
        self.log.log(Direction::Tx, Some(&write.actor), &write.data);
        self.state
            .lock()
            .tx_bytes
            .fetch_add(write.data.len() as u64, Ordering::Relaxed);
        if let Some(completion) = write.completion {
            let result = match evidence {
                Ok(_) => Ok(()),
                Err(error) if error.recorded_event().is_some() => Err(format!(
                    "device write succeeded, but evidence persistence failed; do not retry: {error}"
                )),
                Err(error) => Err(format!(
                    "device write succeeded, but evidence recording failed; do not retry: {error}"
                )),
            };
            let _ = completion.send(result);
        }
    }

    /// Fail an admitted write after a host write attempt. Its device-side
    /// outcome may be partial or unknown, so it is never counted as confirmed.
    pub(crate) fn on_device_tx_failed(&self, write: DeviceWrite, reason: impl Into<String>) {
        let reason = reason.into();
        let _ = self.record_event(
            write.connection_epoch,
            EventPayload::Gap(GapPayload {
                scope: GapScope::TxOutcome,
                certainty: GapCertainty::PartialOrUnknown,
                reason: reason.clone(),
                bytes: Some(BytesPayload::from_bytes(&write.data)),
                actor: Some(write.actor.clone()),
                client_ids: vec![write.client_id.to_string()],
            }),
        );
        self.log.event(&format!(
            "tx_gap reason=device_write_failed certainty=unknown bytes={} error={reason}",
            write.data.len()
        ));
        if let Some(completion) = write.completion {
            let _ = completion.send(Err(format!(
                "device write failed; side effect may be partial or unknown: {reason}"
            )));
        }
    }

    /// Reject the oldest admitted write at the serial-owner gate before any
    /// host write is attempted. Unlike a host write error, this has a known
    /// zero-device-side-effect result.
    pub(crate) fn on_device_tx_not_written(&self, write: DeviceWrite, reason: impl Into<String>) {
        let reason = reason.into();
        let _ = self.record_event(
            write.connection_epoch,
            EventPayload::Control(ControlPayload {
                actor: Some(write.actor.clone()),
                name: "write_rejected".into(),
                value: Some(format!(
                    "client_id={} bytes={} reason={reason}",
                    write.client_id,
                    write.data.len()
                )),
            }),
        );
        self.state.lock().tx_denies.fetch_add(1, Ordering::Relaxed);
        self.log.event(&format!(
            "tx_gap reason=write_rejected certainty=not_written bytes={} error={reason}",
            write.data.len()
        ));
        if let Some(completion) = write.completion {
            let _ = completion.send(Err(format!("device write was not attempted: {reason}")));
        }
    }

    /// Final authority, connection, and deadline gate evaluated by the thread
    /// that owns the serial handle immediately before the host write.
    pub(crate) fn validate_device_write(&self, write: &DeviceWrite) -> Result<(), String> {
        let g = self.state.lock();
        let now = Instant::now();
        if Self::handoff_is_active(&g) {
            return Err("serial handoff is active; write was not attempted".into());
        }
        if !g.port.connected || write.connection_epoch != g.connection_epoch {
            return Err("stale connection epoch".into());
        }
        if now >= write.deadline {
            return Err("write deadline expired before the device write".into());
        }
        let client = g
            .clients
            .get(&write.client_id)
            .ok_or_else(|| "originating client disconnected before the device write".to_string())?;
        if !client.info.can_write
            || client.info.name != write.actor
            || client.info.primary_eligible != write.primary_eligible
        {
            return Err("originating client authority changed before the device write".into());
        }
        if let Some(token) = write.lease_token.as_deref() {
            if !g
                .lock
                .as_ref()
                .is_some_and(|lock| lock.authorizes(Some(token), now))
            {
                return Err("write lease expired, was released, or was replaced".into());
            }
        }
        let primary = g.policy.primary.as_deref();
        let primary_connected = primary.is_some_and(|configured| {
            g.clients.values().any(|slot| {
                slot.info.can_write && slot.info.primary_eligible && slot.info.name == configured
            })
        });
        match admit_write(AdmitContext {
            mode: g.policy.mode,
            client: &write.actor,
            primary,
            client_is_primary: client.info.primary_eligible
                && primary.is_some_and(|configured| configured == write.actor),
            primary_connected,
            lock: g.lock.as_ref(),
            lease_token: write.lease_token.as_deref(),
            now,
        }) {
            AdmitDecision::Allow => Ok(()),
            AdmitDecision::Deny { reason } => Err(reason),
        }
    }

    /// Client stream write. Line/frame modes retain partial data until the
    /// configured delimiter arrives.
    pub async fn client_tx(&self, id: ClientId, data: Bytes) -> Result<(), String> {
        self.client_tx_with_lease(id, data, None).await
    }

    /// Token-aware stream write for clients operating under a write lease.
    pub async fn client_tx_with_lease(
        &self,
        id: ClientId,
        data: Bytes,
        lease_token: Option<&str>,
    ) -> Result<(), String> {
        self.client_tx_inner(id, data, lease_token, false, false)
            .await
    }

    /// One complete write unit. Unlike [`Self::client_tx`], this deliberately
    /// bypasses line assembly, so binary/HTTP writes without a delimiter cannot
    /// be accepted and then silently discarded with a transient client.
    pub async fn client_tx_atomic(&self, id: ClientId, data: Bytes) -> Result<(), String> {
        self.client_tx_atomic_with_lease(id, data, None).await
    }

    pub async fn client_tx_atomic_with_lease(
        &self,
        id: ClientId,
        data: Bytes,
        lease_token: Option<&str>,
    ) -> Result<(), String> {
        self.client_tx_inner(id, data, lease_token, true, false)
            .await
    }

    pub async fn client_tx_atomic_confirmed_with_lease(
        &self,
        id: ClientId,
        data: Bytes,
        lease_token: Option<&str>,
    ) -> Result<(), String> {
        self.client_tx_inner(id, data, lease_token, true, true)
            .await
    }

    async fn client_tx_inner(
        &self,
        id: ClientId,
        data: Bytes,
        lease_token: Option<&str>,
        atomic: bool,
        wait_for_device: bool,
    ) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }

        let (name, primary_eligible, frames, connection_epoch, write_timeout) = {
            let mut g = self.state.lock();
            let mode = g.policy.mode;
            let delim = g.policy.frame_delim;
            let max_frame_bytes = g.policy.max_frame_bytes;
            let max_write_bytes = g.policy.max_write_bytes;
            let write_timeout = Duration::from_millis(g.policy.write_timeout_ms);
            let primary = g.policy.primary.clone();
            let now = Instant::now();
            // purge expired lock
            if g.lock.as_ref().is_some_and(|l| !l.active(now)) {
                g.lock = None;
            }

            if Self::handoff_is_active(&g) {
                g.tx_denies.fetch_add(1, Ordering::Relaxed);
                return Err("serial handoff is active; write was not queued".into());
            }
            if !g.port.connected {
                g.tx_denies.fetch_add(1, Ordering::Relaxed);
                return Err(format!(
                    "serial port '{}' is disconnected; write was not queued",
                    g.port.path
                ));
            }

            let (name, primary_eligible) = {
                let slot = g
                    .clients
                    .get(&id)
                    .ok_or_else(|| "client not registered".to_string())?;
                if !slot.info.can_write {
                    g.tx_denies.fetch_add(1, Ordering::Relaxed);
                    return Err("client is read-only".into());
                }
                (slot.info.name.clone(), slot.info.primary_eligible)
            };

            let primary_connected = primary.as_deref().is_some_and(|configured| {
                g.clients.values().any(|slot| {
                    slot.info.can_write
                        && slot.info.primary_eligible
                        && slot.info.name == configured
                })
            });
            let decision = admit_write(AdmitContext {
                mode,
                client: &name,
                primary: primary.as_deref(),
                client_is_primary: primary_eligible
                    && primary
                        .as_deref()
                        .is_some_and(|configured| configured == name),
                primary_connected,
                lock: g.lock.as_ref(),
                lease_token,
                now,
            });
            match decision {
                AdmitDecision::Deny { reason } => {
                    g.tx_denies.fetch_add(1, Ordering::Relaxed);
                    return Err(reason);
                }
                AdmitDecision::Allow => {}
            }

            let frames = {
                let slot = g
                    .clients
                    .get_mut(&id)
                    .ok_or_else(|| "client not registered".to_string())?;
                if atomic {
                    if data.len() > max_write_bytes {
                        Err(format!(
                            "atomic write exceeds tx.max_write_bytes ({max_write_bytes})"
                        ))
                    } else {
                        Ok(vec![data.to_vec()])
                    }
                } else {
                    match mode {
                        TxMode::QueueByLine | TxMode::QueueByFrame => {
                            slot.assembler.push(&data, delim, max_frame_bytes)
                        }
                        TxMode::Exclusive | TxMode::PrimaryWins => {
                            if data.len() > max_write_bytes {
                                Err(format!(
                                    "stream write exceeds tx.max_write_bytes ({max_write_bytes})"
                                ))
                            } else {
                                Ok(vec![data.to_vec()])
                            }
                        }
                    }
                }
            };
            let frames = match frames {
                Ok(frames) => frames,
                Err(reason) => {
                    g.tx_denies.fetch_add(1, Ordering::Relaxed);
                    return Err(reason);
                }
            };
            (
                name,
                primary_eligible,
                frames,
                g.connection_epoch,
                write_timeout,
            )
        };

        for frame in frames {
            let deadline = Instant::now() + write_timeout;
            let (completion_tx, completion_rx) = if wait_for_device {
                let (tx, rx) = oneshot::channel();
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };

            let permit = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.serial_tx.reserve(),
            )
            .await
            .map_err(|_| "device write queue deadline expired; write was not queued".to_string())?
            .map_err(|_| "serial writer closed; write was not queued".to_string())?;
            let write = DeviceWrite {
                data: Bytes::from(frame),
                client_id: id,
                connection_epoch,
                lease_token: lease_token.map(str::to_owned),
                primary_eligible,
                deadline,
                actor: name.clone(),
                completion: completion_tx,
            };
            if let Err(reason) = self.validate_device_write(&write) {
                self.state.lock().tx_denies.fetch_add(1, Ordering::Relaxed);
                drop(permit);
                return Err(format!("write was not queued: {reason}"));
            }
            permit.send(write);

            if let Some(rx) = completion_rx {
                match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), rx).await {
                    Err(_) => {
                        return Err(
                            "write acknowledgement timed out; outcome may be unknown, do not blindly retry"
                                .into(),
                        )
                    }
                    Ok(Err(_)) => {
                        return Err("serial writer closed before write acknowledgement".into())
                    }
                    Ok(Ok(result)) => result?,
                }
            }
        }
        Ok(())
    }

    /// API helper: each request is one atomic write unit.
    pub async fn api_write(&self, as_client: &str, data: Bytes) -> Result<(), String> {
        self.api_write_with_lease(as_client, data, None).await
    }

    pub async fn api_write_with_lease(
        &self,
        as_client: &str,
        data: Bytes,
        lease_token: Option<&str>,
    ) -> Result<(), String> {
        Self::validate_actor_label(as_client)?;
        // Register ephemeral client channel we never read.
        let (id, rx) = self.register_client_with_primary_eligibility(
            as_client,
            "http",
            false,
            true,
            Some(1),
            false,
        );
        let _registration = self.client_registration(id);
        // can_read=false so no RX is pushed.
        drop(rx);
        // Request-scoped registrations disappear when this future returns or
        // is cancelled. Keep it alive until the serial owner confirms the
        // write so queued commands cannot outlive their authority.
        self.client_tx_atomic_confirmed_with_lease(id, data, lease_token)
            .await
    }

    /// HTTP/automation helper whose success means the serial owner completed
    /// the host write, not merely that the bounded queue accepted it.
    pub async fn api_write_confirmed_with_lease(
        &self,
        as_client: &str,
        data: Bytes,
        lease_token: Option<&str>,
    ) -> Result<(), String> {
        Self::validate_actor_label(as_client)?;
        let (id, rx) = self.register_client_with_primary_eligibility(
            as_client,
            "http",
            false,
            true,
            Some(1),
            false,
        );
        let _registration = self.client_registration(id);
        drop(rx);
        self.client_tx_atomic_confirmed_with_lease(id, data, lease_token)
            .await
    }

    pub fn acquire_lock(&self, client: &str) -> Result<WriteLockView, String> {
        Self::validate_actor_label(client)?;
        let mut g = self.state.lock();
        if Self::handoff_is_active(&g) {
            return Err("serial handoff is active; acquire a lease after resume".into());
        }
        let now = Instant::now();
        if g.lock.as_ref().is_some_and(|l| !l.active(now)) {
            g.lock = None;
        }
        if let Some(lock) = &g.lock {
            if lock.active(now) {
                return Err(format!(
                    "write lease held by '{}'; renew it with its lease token",
                    lock.owner
                ));
            }
        }
        let expires = now + g.policy.lock_ttl();
        let token = Uuid::new_v4().as_simple().to_string();
        g.lock = Some(WriteLock {
            owner: client.to_string(),
            token: token.clone(),
            expires_at: expires,
        });
        let expires_ms = g.policy.write_lock_ms;
        let connection_epoch = g.connection_epoch;
        drop(g);
        self.log.event(&format!("lock_granted owner={client}"));
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Control(ControlPayload {
                actor: Some(client.to_string()),
                name: "lease_acquired".into(),
                value: Some(format!("ttl_ms={expires_ms}")),
            }),
        );
        Ok(WriteLockView {
            owner: client.to_string(),
            expires_ms,
            lease_token: token,
        })
    }

    pub fn renew_lock(&self, lease_token: &str) -> Result<WriteLockView, String> {
        let mut g = self.state.lock();
        let now = Instant::now();
        let ttl = g.policy.lock_ttl();
        let expires_ms = g.policy.write_lock_ms;
        let Some(lock) = g.lock.as_mut() else {
            return Err("no active write lease".into());
        };
        if !lock.active(now) {
            g.lock = None;
            return Err("write lease expired".into());
        }
        if !lock.authorizes(Some(lease_token), now) {
            return Err("invalid lease token".into());
        }
        lock.expires_at = now + ttl;
        let view = WriteLockView {
            owner: lock.owner.clone(),
            expires_ms,
            lease_token: lock.token.clone(),
        };
        let connection_epoch = g.connection_epoch;
        drop(g);
        self.log
            .event(&format!("lock_renewed owner={}", view.owner));
        let _ = self.record_event(
            connection_epoch,
            EventPayload::Control(ControlPayload {
                actor: Some(view.owner.clone()),
                name: "lease_renewed".into(),
                value: Some(format!("ttl_ms={expires_ms}")),
            }),
        );
        Ok(view)
    }

    /// Release an active lease. The optional shape preserves the old call site
    /// while changing its meaning from spoofable owner name to bearer token.
    pub fn release_lock(&self, lease_token: Option<&str>) -> Result<(), String> {
        let mut g = self.state.lock();
        let now = Instant::now();
        let released = match &g.lock {
            Some(lock) if lock.active(now) => {
                let Some(token) = lease_token else {
                    return Err("lease token is required".into());
                };
                if !lock.authorizes(Some(token), now) {
                    return Err("invalid lease token".into());
                }
                let owner = lock.owner.clone();
                g.lock = None;
                Some((owner, g.connection_epoch))
            }
            _ => {
                g.lock = None;
                None
            }
        };
        drop(g);
        if let Some((owner, connection_epoch)) = released {
            self.log.event(&format!("lock_released owner={owner}"));
            let _ = self.record_event(
                connection_epoch,
                EventPayload::Control(ControlPayload {
                    actor: Some(owner),
                    name: "lease_released".into(),
                    value: None,
                }),
            );
        }
        Ok(())
    }

    pub fn history_bytes(&self) -> Bytes {
        let g = self.state.lock();
        Bytes::from(g.history.iter().copied().collect::<Vec<_>>())
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        let g = self.state.lock();
        let now = Instant::now();
        let (lock_owner, lock_expires_ms) = match &g.lock {
            Some(l) if l.active(now) => (
                Some(l.owner.clone()),
                Some(l.expires_at.saturating_duration_since(now).as_millis() as u64),
            ),
            _ => (None, None),
        };
        let handoff = g.handoff.as_ref().map(|handoff| HandoffStatusView {
            active: true,
            path: handoff.path.clone(),
            expires_ms: handoff
                .expires_at
                .saturating_duration_since(now)
                .as_millis() as u64,
        });
        let mode = match g.policy.mode {
            TxMode::QueueByLine => "queue_by_line",
            TxMode::QueueByFrame => "queue_by_frame",
            TxMode::Exclusive => "exclusive",
            TxMode::PrimaryWins => "primary_wins",
        };
        StatusSnapshot {
            port: PortStatusView {
                path: g.port.path.clone(),
                baud: g.port.baud,
                connected: g.port.connected,
                epoch: g.connection_epoch,
                detail: g.port.detail.clone(),
            },
            handoff,
            tx_mode: mode.into(),
            lock_owner,
            lock_expires_ms,
            endpoints: g.endpoints.clone(),
            clients: g
                .clients
                .values()
                .map(|c| ClientView {
                    id: c.info.id.to_string(),
                    name: c.info.name.clone(),
                    kind: c.info.kind.clone(),
                    can_read: c.info.can_read,
                    can_write: c.info.can_write,
                    primary_eligible: c.info.primary_eligible,
                })
                .collect(),
            stats: StatsView {
                rx_bytes: g.rx_bytes.load(Ordering::Relaxed),
                tx_bytes: g.tx_bytes.load(Ordering::Relaxed),
                rx_drops: g.rx_drops.load(Ordering::Relaxed),
                rx_drop_oldest: g.rx_drop_oldest.load(Ordering::Relaxed),
                rx_drop_newest: g.rx_drop_newest.load(Ordering::Relaxed),
                rx_block_events: g.rx_block_events.load(Ordering::Relaxed),
                slow_disconnects: g.slow_disconnects.load(Ordering::Relaxed),
                tx_denies: g.tx_denies.load(Ordering::Relaxed),
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WriteLockView {
    pub owner: String,
    pub expires_ms: u64,
    /// Secret bearer credential. Returned only on acquire/renew, never status.
    pub lease_token: String,
}

/// Optional oneshot used by tests.
#[allow(dead_code)]
pub type Done = oneshot::Sender<()>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{EventFilter, EventPayload, EventQuery, GapScope};
    use crate::policy::Policy;
    use crate::policy::{SlowClientPolicy, TxMode};

    fn test_broker_with(
        mode: TxMode,
        slow_client: SlowClientPolicy,
        client_queue: usize,
        connected: bool,
    ) -> (Broker, mpsc::Receiver<DeviceWrite>) {
        let policy = Policy {
            mode,
            primary: Some("ui".into()),
            write_lock_ms: 1000,
            write_timeout_ms: 1000,
            max_frame_bytes: 1024,
            max_write_bytes: 1024,
            frame_delim: b'\n',
            slow_client,
            client_queue,
            slow_block_ms: 100,
        };
        let port = PortStatus {
            path: "mock".into(),
            baud: 115200,
            connected,
            detail: "ok".into(),
        };
        let split = Broker::new(policy, port, SessionLog::disabled(), 1024, 32);
        (split.broker, split.serial_tx_rx)
    }

    fn test_broker(mode: TxMode) -> (Broker, mpsc::Receiver<DeviceWrite>) {
        test_broker_with(mode, SlowClientPolicy::DropOldest, 16, true)
    }

    async fn stage_slow_client(broker: &Broker) -> (ClientId, ClientRx) {
        let (id, rx) = broker.register_client("slow", "test", true, false, Some(1));
        broker.on_device_rx(Bytes::from_static(b"A"));
        (id, rx)
    }

    #[tokio::test]
    async fn fanout_rx_to_two_clients() {
        let (broker, _tx) = test_broker(TxMode::QueueByLine);
        let (id1, mut rx1) = broker.register_client("a", "tcp", true, true, None);
        let (id2, mut rx2) = broker.register_client("b", "tcp", true, true, None);
        broker.on_device_rx(Bytes::from_static(b"hello"));
        let a = rx1.recv().await.unwrap();
        let b = rx2.recv().await.unwrap();
        assert_eq!(&a[..], b"hello");
        assert_eq!(&b[..], b"hello");
        broker.unregister_client(id1);
        broker.unregister_client(id2);
    }

    #[tokio::test]
    async fn queue_by_line_waits_for_newline() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let (id, _rx) = broker.register_client("ui", "tcp", true, true, None);
        broker
            .client_tx(id, Bytes::from_static(b"hel"))
            .await
            .unwrap();
        assert!(serial_rx.try_recv().is_err());
        broker
            .client_tx(id, Bytes::from_static(b"lo\n"))
            .await
            .unwrap();
        let frame = serial_rx.recv().await.unwrap();
        assert_eq!(&frame.bytes()[..], b"hello\n");
        broker.unregister_client(id);
    }

    #[tokio::test]
    async fn exclusive_denies_without_lock() {
        let (broker, _serial_rx) = test_broker(TxMode::Exclusive);
        let (id, _rx) = broker.register_client("agent", "ws", true, true, None);
        let err = broker
            .client_tx(id, Bytes::from_static(b"x\n"))
            .await
            .unwrap_err();
        assert!(err.contains("lock") || err.contains("exclusive"));
        broker.unregister_client(id);
    }

    #[tokio::test]
    async fn atomic_api_write_bypasses_line_assembler() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let writer = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .api_write("api", Bytes::from_static(&[0x01, 0x02]))
                    .await
            })
        };
        let written = serial_rx.recv().await.unwrap();
        assert_eq!(written.bytes(), &Bytes::from_static(&[0x01, 0x02]));
        broker.on_device_tx_written(written);
        writer.await.unwrap().unwrap();
        assert_eq!(broker.snapshot().stats.tx_bytes, 2);
    }

    #[tokio::test]
    async fn disconnected_port_rejects_before_serial_queue() {
        let (broker, mut serial_rx) =
            test_broker_with(TxMode::QueueByLine, SlowClientPolicy::DropOldest, 16, false);
        let (id, _rx) = broker.register_client("ui", "test", false, true, None);
        let err = broker
            .client_tx(id, Bytes::from_static(b"stale\n"))
            .await
            .unwrap_err();
        assert!(err.contains("disconnected"));
        assert!(serial_rx.try_recv().is_err());
        assert_eq!(broker.snapshot().stats.tx_denies, 1);
    }

    #[tokio::test]
    async fn prewrite_gate_rejects_an_admitted_write_after_reconnect() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let (id, _rx) = broker.register_client("ui", "test", false, true, None);

        broker
            .client_tx_atomic(id, Bytes::from_static(b"old-connection"))
            .await
            .unwrap();
        let admitted = serial_rx.recv().await.unwrap();
        assert!(broker.validate_device_write(&admitted).is_ok());

        broker.set_port_status(PortStatus {
            path: "mock".into(),
            baud: 115200,
            connected: false,
            detail: "disconnected".into(),
        });
        broker.set_port_status(PortStatus {
            path: "mock".into(),
            baud: 115200,
            connected: true,
            detail: "reconnected".into(),
        });

        assert!(broker.validate_device_write(&admitted).is_err());
        broker.on_device_tx_not_written(admitted, "stale connection epoch");
        assert_eq!(broker.snapshot().stats.tx_bytes, 0);
        assert_eq!(broker.snapshot().stats.tx_denies, 1);
    }

    #[tokio::test]
    async fn lease_is_a_bearer_token_and_survives_same_name_disconnect() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let lease = broker.acquire_lock("owner").unwrap();
        assert_eq!(lease.lease_token.len(), 32);
        assert_ne!(lease.lease_token, lease.owner);

        let (same_name, rx) = broker.register_client("owner", "temporary", false, true, None);
        drop(rx);
        broker.unregister_client(same_name);
        assert_eq!(broker.snapshot().lock_owner.as_deref(), Some("owner"));

        let denied = broker
            .api_write("owner", Bytes::from_static(b"spoofed"))
            .await
            .unwrap_err();
        assert!(denied.contains("lease token"));
        assert!(broker.release_lock(Some("owner")).is_err());

        let authorized = {
            let broker = broker.clone();
            let token = lease.lease_token.clone();
            tokio::spawn(async move {
                broker
                    .api_write_with_lease(
                        "any-display-name",
                        Bytes::from_static(b"authorized"),
                        Some(&token),
                    )
                    .await
            })
        };
        let write = serial_rx.recv().await.unwrap();
        assert_eq!(write.bytes(), &Bytes::from_static(b"authorized"));
        broker.on_device_tx_written(write);
        authorized.await.unwrap().unwrap();

        let renewed = broker.renew_lock(&lease.lease_token).unwrap();
        assert_eq!(renewed.lease_token, lease.lease_token);
        broker.release_lock(Some(&lease.lease_token)).unwrap();
        assert!(broker.snapshot().lock_owner.is_none());
    }

    #[tokio::test]
    async fn control_requires_lease_and_waits_for_owner_ack() {
        let (broker, _serial_rx) = test_broker(TxMode::QueueByLine);
        let (control_tx, mut control_rx) = mpsc::channel(1);
        broker.attach_serial_control(control_tx);
        let no_lease = broker
            .serial_control("agent", None, ControlCommand::Dtr(true))
            .await
            .unwrap_err();
        assert!(no_lease.contains("lease"));

        let lease = broker.acquire_lock("agent").unwrap();
        let operation = {
            let broker = broker.clone();
            let token = lease.lease_token.clone();
            tokio::spawn(async move {
                broker
                    .serial_control("agent", Some(&token), ControlCommand::Dtr(true))
                    .await
            })
        };
        let (command, acknowledgement) = match control_rx.recv().await.expect("owner command") {
            SerialControl::Command {
                command,
                acknowledgement,
            } => (command, acknowledgement),
            SerialControl::Configure { .. } => panic!("unexpected configure command"),
            SerialControl::BeginHandoff { .. } | SerialControl::ResumeHandoff { .. } => {
                panic!("unexpected handoff command")
            }
        };
        assert_eq!(command, ControlCommand::Dtr(true));
        acknowledgement.send(Ok(())).unwrap();
        operation.await.unwrap().unwrap();

        let page = broker.ledger().query(EventQuery {
            after_seq: 0,
            through_seq: None,
            limit: 100,
            filter: EventFilter::default(),
        });
        assert!(page.events.iter().any(|event| {
            matches!(
                &event.event,
                EventPayload::Control(payload)
                    if payload.actor.as_deref() == Some("agent") && payload.name == "dtr"
            )
        }));
    }

    #[tokio::test]
    async fn handoff_is_bounded_invalidates_lease_and_requires_resume_token() {
        let (broker, _serial_rx) = test_broker(TxMode::QueueByLine);
        let (control_tx, mut control_rx) = mpsc::channel(2);
        broker.attach_serial_control(control_tx);
        let lease = broker.acquire_lock("handoff-owner").unwrap();
        let begin = {
            let broker = broker.clone();
            let token = lease.lease_token.clone();
            tokio::spawn(async move {
                broker
                    .begin_handoff("handoff-owner", Some(&token), 5000)
                    .await
            })
        };
        let begin_ack = match control_rx.recv().await.expect("begin handoff command") {
            SerialControl::BeginHandoff {
                duration_ms,
                acknowledgement,
            } => {
                assert_eq!(duration_ms, 5000);
                acknowledgement
            }
            _ => panic!("unexpected command"),
        };
        begin_ack.send(Ok(())).unwrap();
        let view = begin.await.unwrap().unwrap();
        assert_eq!(view.path, "mock");
        assert_eq!(view.expires_ms, 5000);
        assert!(broker.snapshot().handoff.is_some());
        assert!(broker.snapshot().lock_owner.is_none());
        assert!(broker.acquire_lock("new-owner").is_err());

        let resume = {
            let broker = broker.clone();
            let token = view.handoff_token.clone();
            tokio::spawn(async move { broker.resume_handoff(&token).await })
        };
        let resume_ack = match control_rx.recv().await.expect("resume handoff command") {
            SerialControl::ResumeHandoff { acknowledgement } => acknowledgement,
            _ => panic!("unexpected command"),
        };
        resume_ack.send(Ok(())).unwrap();
        resume.await.unwrap().unwrap();
        assert!(broker.snapshot().handoff.is_none());
    }

    #[tokio::test]
    async fn primary_wins_reserves_tx_until_primary_disconnects() {
        let (broker, mut serial_rx) = test_broker(TxMode::PrimaryWins);
        let (primary, _primary_rx) = broker.register_client("ui", "test", false, true, None);
        let (secondary, _secondary_rx) = broker.register_client("agent", "test", false, true, None);

        let err = broker
            .client_tx(secondary, Bytes::from_static(b"secondary"))
            .await
            .unwrap_err();
        assert!(err.contains("primary_wins"));

        broker
            .client_tx(primary, Bytes::from_static(b"primary"))
            .await
            .unwrap();
        assert_eq!(
            serial_rx.recv().await.unwrap().bytes(),
            &Bytes::from_static(b"primary")
        );

        broker.unregister_client(primary);
        broker
            .client_tx(secondary, Bytes::from_static(b"fallback"))
            .await
            .unwrap();
        assert_eq!(
            serial_rx.recv().await.unwrap().bytes(),
            &Bytes::from_static(b"fallback")
        );
    }

    #[tokio::test]
    async fn queued_primary_write_is_revoked_when_its_connection_unregisters() {
        let (broker, mut serial_rx) = test_broker(TxMode::PrimaryWins);
        let (primary, _primary_rx) =
            broker.register_client("ui", "trusted-endpoint", false, true, None);

        broker
            .client_tx_atomic(primary, Bytes::from_static(b"queued-primary"))
            .await
            .unwrap();
        let write = serial_rx.recv().await.unwrap();
        broker.unregister_client(primary);

        let error = broker.validate_device_write(&write).unwrap_err();
        assert!(error.contains("disconnected"), "error={error}");
        broker.on_device_tx_not_written(write, error);
        assert_eq!(broker.snapshot().stats.tx_bytes, 0);
    }

    #[tokio::test]
    async fn http_as_client_label_cannot_impersonate_primary_capability() {
        let (broker, mut serial_rx) = test_broker(TxMode::PrimaryWins);
        let (_primary, _rx) = broker.register_client("ui", "trusted-endpoint", false, true, None);

        let error = broker
            .api_write("ui", Bytes::from_static(b"spoofed"))
            .await
            .unwrap_err();
        assert!(error.contains("primary_wins"), "error={error}");
        assert!(serial_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn drop_oldest_replaces_queued_chunk_and_counts_it() {
        let (broker, _serial_rx) =
            test_broker_with(TxMode::QueueByLine, SlowClientPolicy::DropOldest, 1, true);
        let (id, mut rx) = stage_slow_client(&broker).await;
        broker.on_device_rx(Bytes::from_static(b"D"));

        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"D"));
        let stats = broker.snapshot().stats;
        assert_eq!(stats.rx_drops, 1);
        assert_eq!(stats.rx_drop_oldest, 1);
        assert_eq!(stats.rx_drop_newest, 0);
        broker.unregister_client(id);
    }

    #[tokio::test]
    async fn drop_newest_preserves_queued_chunk_and_counts_it() {
        let (broker, _serial_rx) =
            test_broker_with(TxMode::QueueByLine, SlowClientPolicy::DropNewest, 1, true);
        let (id, mut rx) = stage_slow_client(&broker).await;
        broker.on_device_rx(Bytes::from_static(b"D"));

        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"A"));
        let stats = broker.snapshot().stats;
        assert_eq!(stats.rx_drops, 1);
        assert_eq!(stats.rx_drop_oldest, 0);
        assert_eq!(stats.rx_drop_newest, 1);
        broker.unregister_client(id);
    }

    #[tokio::test]
    async fn block_waits_for_capacity_without_dropping() {
        let (broker, _serial_rx) =
            test_broker_with(TxMode::QueueByLine, SlowClientPolicy::Block, 1, true);
        let (id, mut rx) = stage_slow_client(&broker).await;
        let broker_for_thread = broker.clone();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            broker_for_thread.on_device_rx(Bytes::from_static(b"D"));
            let _ = done_tx.send(());
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut done_rx)
                .await
                .is_err()
        );
        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"A"));
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut done_rx)
            .await
            .unwrap()
            .unwrap();
        worker.join().unwrap();

        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"D"));
        let stats = broker.snapshot().stats;
        assert_eq!(stats.rx_drops, 0);
        assert_eq!(stats.rx_block_events, 1);
        broker.unregister_client(id);
    }

    #[tokio::test]
    async fn block_policy_has_a_deadline_and_disconnects_a_stuck_reader() {
        let (broker, _serial_rx) =
            test_broker_with(TxMode::QueueByLine, SlowClientPolicy::Block, 1, true);
        broker.state.lock().policy.slow_block_ms = 20;
        let (_id, _rx) = stage_slow_client(&broker).await;

        let started = Instant::now();
        broker.on_device_rx(Bytes::from_static(b"D"));
        assert!(started.elapsed() < Duration::from_millis(500));

        let snapshot = broker.snapshot();
        assert!(snapshot.clients.is_empty());
        assert_eq!(snapshot.stats.rx_drops, 2);
        assert_eq!(snapshot.stats.rx_block_events, 1);
        assert_eq!(snapshot.stats.slow_disconnects, 1);
    }

    #[tokio::test]
    async fn confirmed_write_completes_only_after_device_ack() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let writer = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .api_write_confirmed_with_lease("api", Bytes::from_static(b"confirmed"), None)
                    .await
            })
        };

        let write = serial_rx.recv().await.unwrap();
        assert!(!writer.is_finished());
        broker.on_device_tx_written(write);
        assert!(writer.await.unwrap().is_ok());
        assert_eq!(broker.snapshot().stats.tx_bytes, 9);
    }

    #[tokio::test]
    async fn confirmed_write_failure_is_not_reported_as_success() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let writer = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .api_write_confirmed_with_lease("api", Bytes::from_static(b"uncertain"), None)
                    .await
            })
        };

        let write = serial_rx.recv().await.unwrap();
        broker.on_device_tx_failed(write, "driver error");
        let error = writer.await.unwrap().unwrap_err();
        assert!(error.contains("partial or unknown"), "error={error}");
        assert_eq!(broker.snapshot().stats.tx_bytes, 0);
        let events = broker.ledger().query(EventQuery::default()).events;
        assert!(!events
            .iter()
            .any(|event| matches!(event.event, EventPayload::Tx(_))));
        assert!(events.iter().any(|event| matches!(
            event.event,
            EventPayload::Gap(ref gap) if gap.scope == GapScope::TxOutcome
        )));
    }

    #[tokio::test]
    async fn confirmed_write_timeout_is_bounded_and_cleans_up_ephemeral_client() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        broker.state.lock().policy.write_timeout_ms = 25;
        let writer = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .api_write_confirmed_with_lease("api", Bytes::from_static(b"deadline"), None)
                    .await
            })
        };
        let write = serial_rx.recv().await.unwrap();
        let error = writer.await.unwrap().unwrap_err();
        assert!(error.contains("outcome may be unknown"), "error={error}");
        assert!(broker.snapshot().clients.is_empty());
        assert!(broker.validate_device_write(&write).is_err());
        broker.on_device_tx_not_written(write, "expired test write");
    }

    #[tokio::test]
    async fn cancelling_confirmed_http_helper_does_not_leak_primary_client() {
        let (broker, mut serial_rx) = test_broker(TxMode::PrimaryWins);
        let writer = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .api_write_confirmed_with_lease("ui", Bytes::from_static(b"cancelled"), None)
                    .await
            })
        };
        let write = serial_rx.recv().await.unwrap();
        writer.abort();
        let _ = writer.await;
        assert!(broker.snapshot().clients.is_empty());
        let error = broker.validate_device_write(&write).unwrap_err();
        assert!(error.contains("disconnected"), "error={error}");
        broker.on_device_tx_not_written(write, error);
    }

    #[tokio::test]
    async fn releasing_a_lease_invalidates_its_already_queued_write() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let lease = broker.acquire_lock("owner").unwrap();
        let writer = {
            let broker = broker.clone();
            let token = lease.lease_token.clone();
            tokio::spawn(async move {
                broker
                    .api_write_with_lease("owner", Bytes::from_static(b"queued"), Some(&token))
                    .await
            })
        };
        let write = serial_rx.recv().await.unwrap();
        broker.release_lock(Some(&lease.lease_token)).unwrap();
        let error = broker.validate_device_write(&write).unwrap_err();
        assert!(error.contains("lease"), "error={error}");
        broker.on_device_tx_not_written(write, error);
        assert!(writer.await.unwrap().is_err());
        let events = broker.ledger().query(EventQuery::default()).events;
        assert!(!events
            .iter()
            .any(|event| matches!(event.event, EventPayload::Tx(_))));
        assert!(events.iter().any(|event| matches!(
            event.event,
            EventPayload::Control(ref control) if control.name == "write_rejected"
        )));
    }

    #[tokio::test]
    async fn confirmed_tx_is_recorded_only_after_the_host_write_ack() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        let writer = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .api_write_confirmed_with_lease(
                        "evidence-writer",
                        Bytes::from_static(&[0x00, 0xff]),
                        None,
                    )
                    .await
            })
        };
        let write = serial_rx.recv().await.unwrap();
        assert!(!broker
            .ledger()
            .query(EventQuery::default())
            .events
            .iter()
            .any(|event| matches!(event.event, EventPayload::Tx(_))));
        broker.on_device_tx_written(write);
        writer.await.unwrap().unwrap();

        let events = broker.ledger().query(EventQuery::default()).events;
        let tx = events
            .iter()
            .find_map(|event| match &event.event {
                EventPayload::Tx(tx) => Some(tx),
                _ => None,
            })
            .expect("confirmed TX event");
        assert_eq!(tx.actor, "evidence-writer");
        assert_eq!(tx.bytes.decode().unwrap(), [0x00, 0xff]);
    }

    #[test]
    fn lease_credentials_never_enter_the_event_ledger() {
        let (broker, _serial_rx) = test_broker(TxMode::QueueByLine);
        let lease = broker.acquire_lock("audit-owner").unwrap();
        broker.renew_lock(&lease.lease_token).unwrap();
        broker.release_lock(Some(&lease.lease_token)).unwrap();

        let json =
            serde_json::to_string(&broker.ledger().query(EventQuery::default()).events).unwrap();
        assert!(!json.contains(&lease.lease_token));
        assert!(!json.contains("lease_token"));
    }

    #[tokio::test]
    async fn framed_and_atomic_writes_are_size_bounded() {
        let (broker, mut serial_rx) = test_broker(TxMode::QueueByLine);
        broker.state.lock().policy.max_frame_bytes = 4;
        broker.state.lock().policy.max_write_bytes = 4;
        let (id, _rx) = broker.register_client("ui", "test", false, true, None);

        let frame_error = broker
            .client_tx(id, Bytes::from_static(b"12345"))
            .await
            .unwrap_err();
        assert!(frame_error.contains("max_frame_bytes"));
        let atomic_error = broker
            .client_tx_atomic(id, Bytes::from_static(b"12345"))
            .await
            .unwrap_err();
        assert!(atomic_error.contains("max_write_bytes"));
        assert!(serial_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn disconnect_slow_is_counted_and_removes_client() {
        let (broker, _serial_rx) = test_broker_with(
            TxMode::QueueByLine,
            SlowClientPolicy::DisconnectSlow,
            1,
            true,
        );
        let (_id, _rx) = stage_slow_client(&broker).await;
        broker.on_device_rx(Bytes::from_static(b"D"));

        let snapshot = broker.snapshot();
        assert!(snapshot.clients.is_empty());
        assert_eq!(snapshot.stats.rx_drops, 2);
        assert_eq!(snapshot.stats.rx_drop_newest, 2);
        assert_eq!(snapshot.stats.slow_disconnects, 1);
    }
}
