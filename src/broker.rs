//! Central broker: client registry, RX fan-out, TX arbitration.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use uuid::Uuid;

use crate::observe::{Direction, SessionLog};
use crate::policy::{
    admit_write, AdmitDecision, FrameAssembler, Policy, SlowClientPolicy, TxMode, WriteLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub Uuid);

impl ClientId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
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
    pub connected_at: chrono::DateTime<chrono::Local>,
}

#[derive(Debug, Clone)]
pub struct PortStatus {
    pub path: String,
    pub baud: u32,
    pub connected: bool,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusSnapshot {
    pub port: PortStatusView,
    pub tx_mode: String,
    pub lock_owner: Option<String>,
    pub lock_expires_ms: Option<u64>,
    pub clients: Vec<ClientView>,
    pub stats: StatsView,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PortStatusView {
    pub path: String,
    pub baud: u32,
    pub connected: bool,
    pub detail: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub can_read: bool,
    pub can_write: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatsView {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_drops: u64,
    pub tx_denies: u64,
}

struct ClientSlot {
    info: ClientInfo,
    /// Outbound to client (device RX fan-out).
    to_client: mpsc::Sender<Bytes>,
    assembler: FrameAssembler,
}

struct BrokerState {
    clients: HashMap<ClientId, ClientSlot>,
    policy: Policy,
    lock: Option<WriteLock>,
    port: PortStatus,
    history: VecDeque<u8>,
    history_cap: usize,
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
    rx_drops: AtomicU64,
    tx_denies: AtomicU64,
}

/// Handle shared across adapters and API.
#[derive(Clone)]
pub struct Broker {
    state: Arc<Mutex<BrokerState>>,
    /// Device-bound TX frames (already admitted & framed).
    serial_tx: mpsc::Sender<Bytes>,
    /// Fan-out of raw RX for websocket late-join style subscribers (optional).
    rx_broadcast: broadcast::Sender<Bytes>,
    log: SessionLog,
    /// Notify waiters when serial connection state changes.
    port_watch: watch::Sender<PortStatus>,
}

pub struct BrokerSplit {
    pub broker: Broker,
    /// Receiver for bytes that must be written to the real serial port.
    pub serial_tx_rx: mpsc::Receiver<Bytes>,
    pub port_watch_rx: watch::Receiver<PortStatus>,
}

impl Broker {
    pub fn new(
        policy: Policy,
        port: PortStatus,
        log: SessionLog,
        history_cap: usize,
        serial_queue: usize,
    ) -> BrokerSplit {
        let (serial_tx, serial_tx_rx) = mpsc::channel(serial_queue.max(16));
        let (rx_broadcast, _) = broadcast::channel(256);
        let (port_watch, port_watch_rx) = watch::channel(port.clone());

        let state = BrokerState {
            clients: HashMap::new(),
            policy,
            lock: None,
            port,
            history: VecDeque::with_capacity(history_cap.min(1024)),
            history_cap,
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            rx_drops: AtomicU64::new(0),
            tx_denies: AtomicU64::new(0),
        };

        BrokerSplit {
            broker: Broker {
                state: Arc::new(Mutex::new(state)),
                serial_tx,
                rx_broadcast,
                log,
                port_watch,
            },
            serial_tx_rx,
            port_watch_rx,
        }
    }

    pub fn log(&self) -> &SessionLog {
        &self.log
    }

    pub fn subscribe_rx(&self) -> broadcast::Receiver<Bytes> {
        self.rx_broadcast.subscribe()
    }

    pub fn set_port_status(&self, status: PortStatus) {
        {
            let mut g = self.state.lock();
            g.port = status.clone();
        }
        let _ = self.port_watch.send(status);
    }

    pub fn register_client(
        &self,
        name: impl Into<String>,
        kind: impl Into<String>,
        can_read: bool,
        can_write: bool,
        queue_cap: Option<usize>,
    ) -> (ClientId, mpsc::Receiver<Bytes>) {
        let id = ClientId::new();
        let name = name.into();
        let kind = kind.into();
        let cap = queue_cap.unwrap_or_else(|| self.state.lock().policy.client_queue);
        let (to_client, rx) = mpsc::channel(cap);

        let info = ClientInfo {
            id,
            name: name.clone(),
            kind: kind.clone(),
            can_read,
            can_write,
            connected_at: chrono::Local::now(),
        };

        {
            let mut g = self.state.lock();
            g.clients.insert(
                id,
                ClientSlot {
                    info,
                    to_client,
                    assembler: FrameAssembler::default(),
                },
            );
        }

        self.log
            .event(&format!("client_join id={id} name={name} kind={kind}"));
        (id, rx)
    }

    pub fn unregister_client(&self, id: ClientId) {
        let mut g = self.state.lock();
        if let Some(slot) = g.clients.remove(&id) {
            // Drop lock if owner disconnects.
            if g.lock
                .as_ref()
                .is_some_and(|l| l.owner == slot.info.name)
            {
                g.lock = None;
            }
            self.log.event(&format!(
                "client_leave id={id} name={}",
                slot.info.name
            ));
        }
    }

    /// Called when device produces data.
    pub fn on_device_rx(&self, data: Bytes) {
        if data.is_empty() {
            return;
        }
        self.log.log(Direction::Rx, None, &data);
        {
            let g = self.state.lock();
            g.rx_bytes
                .fetch_add(data.len() as u64, Ordering::Relaxed);
        }

        let _ = self.rx_broadcast.send(data.clone());

        let mut dead = Vec::new();
        let mut drop_count = 0u64;
        {
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

            let slow = g.policy.slow_client;
            for (id, slot) in g.clients.iter_mut() {
                if !slot.info.can_read {
                    continue;
                }
                match slot.to_client.try_send(data.clone()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => match slow {
                        SlowClientPolicy::DropOldest => {
                            drop_count += 1;
                        }
                        SlowClientPolicy::DisconnectSlow => {
                            dead.push(*id);
                            drop_count += 1;
                        }
                        SlowClientPolicy::Block => {
                            // Cannot block here (sync). Drop with counter.
                            drop_count += 1;
                        }
                    },
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        dead.push(*id);
                    }
                }
            }
            if drop_count > 0 {
                g.rx_drops.fetch_add(drop_count, Ordering::Relaxed);
            }
        }
        for id in dead {
            self.unregister_client(id);
        }
    }

    /// Client wants to send raw bytes toward the device.
    pub async fn client_tx(&self, id: ClientId, data: Bytes) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }

        let (name, frames) = {
            let mut g = self.state.lock();
            let mode = g.policy.mode;
            let delim = g.policy.frame_delim;
            let primary = g.policy.primary.clone();
            let now = Instant::now();
            // purge expired lock
            if g.lock.as_ref().is_some_and(|l| !l.active(now)) {
                g.lock = None;
            }

            let name = {
                let slot = g
                    .clients
                    .get(&id)
                    .ok_or_else(|| "client not registered".to_string())?;
                if !slot.info.can_write {
                    g.tx_denies.fetch_add(1, Ordering::Relaxed);
                    return Err("client is read-only".into());
                }
                slot.info.name.clone()
            };

            let decision = admit_write(
                mode,
                &name,
                primary.as_deref(),
                g.lock.as_ref(),
                now,
            );
            match decision {
                AdmitDecision::Deny { reason } => {
                    g.tx_denies.fetch_add(1, Ordering::Relaxed);
                    return Err(reason);
                }
                AdmitDecision::Allow | AdmitDecision::AllowPrimaryPrefer => {}
            }

            let slot = g
                .clients
                .get_mut(&id)
                .ok_or_else(|| "client not registered".to_string())?;
            let frames = match mode {
                TxMode::QueueByLine | TxMode::QueueByFrame => slot.assembler.push(&data, delim),
                TxMode::Exclusive | TxMode::PrimaryWins => {
                    // pass-through chunks (still under lock/admit rules)
                    vec![data.to_vec()]
                }
            };
            (name, frames)
        };

        for frame in frames {
            let bytes = Bytes::from(frame);
            self.log
                .log(Direction::Tx, Some(&name), &bytes);
            {
                let g = self.state.lock();
                g.tx_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            }
            self.serial_tx
                .send(bytes)
                .await
                .map_err(|_| "serial writer closed".to_string())?;
        }
        Ok(())
    }

    /// API helper: write as a transient named client (or existing name for lock checks).
    pub async fn api_write(&self, as_client: &str, data: Bytes) -> Result<(), String> {
        // Register ephemeral client channel we never read.
        let (id, rx) = self.register_client(as_client, "http", false, true, Some(1));
        // can_read=false so no RX is pushed.
        drop(rx);
        let res = self.client_tx(id, data).await;
        self.unregister_client(id);
        res
    }

    pub fn acquire_lock(&self, client: &str) -> Result<WriteLockView, String> {
        let mut g = self.state.lock();
        let now = Instant::now();
        if g.lock.as_ref().is_some_and(|l| !l.active(now)) {
            g.lock = None;
        }
        if let Some(lock) = &g.lock {
            if lock.active(now) && lock.owner != client {
                return Err(format!("lock held by '{}'", lock.owner));
            }
        }
        let expires = now + g.policy.lock_ttl();
        g.lock = Some(WriteLock {
            owner: client.to_string(),
            expires_at: expires,
        });
        self.log
            .event(&format!("lock_granted owner={client}"));
        Ok(WriteLockView {
            owner: client.to_string(),
            expires_ms: g.policy.write_lock_ms,
        })
    }

    pub fn release_lock(&self, client: Option<&str>) -> Result<(), String> {
        let mut g = self.state.lock();
        let now = Instant::now();
        match &g.lock {
            Some(lock) if lock.active(now) => {
                if let Some(c) = client {
                    if lock.owner != c {
                        return Err(format!("lock owned by '{}'", lock.owner));
                    }
                }
                let owner = lock.owner.clone();
                g.lock = None;
                self.log.event(&format!("lock_released owner={owner}"));
                Ok(())
            }
            _ => {
                g.lock = None;
                Ok(())
            }
        }
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
                detail: g.port.detail.clone(),
            },
            tx_mode: mode.into(),
            lock_owner,
            lock_expires_ms,
            clients: g
                .clients
                .values()
                .map(|c| ClientView {
                    id: c.info.id.to_string(),
                    name: c.info.name.clone(),
                    kind: c.info.kind.clone(),
                    can_read: c.info.can_read,
                    can_write: c.info.can_write,
                })
                .collect(),
            stats: StatsView {
                rx_bytes: g.rx_bytes.load(Ordering::Relaxed),
                tx_bytes: g.tx_bytes.load(Ordering::Relaxed),
                rx_drops: g.rx_drops.load(Ordering::Relaxed),
                tx_denies: g.tx_denies.load(Ordering::Relaxed),
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WriteLockView {
    pub owner: String,
    pub expires_ms: u64,
}

/// Optional oneshot used by tests.
#[allow(dead_code)]
pub type Done = oneshot::Sender<()>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use crate::policy::{SlowClientPolicy, TxMode};

    fn test_broker(mode: TxMode) -> (Broker, mpsc::Receiver<Bytes>) {
        let policy = Policy {
            mode,
            primary: Some("ui".into()),
            write_lock_ms: 1000,
            frame_delim: b'\n',
            slow_client: SlowClientPolicy::DropOldest,
            client_queue: 16,
        };
        let port = PortStatus {
            path: "mock".into(),
            baud: 115200,
            connected: true,
            detail: "ok".into(),
        };
        let split = Broker::new(policy, port, SessionLog::disabled(), 1024, 32);
        (split.broker, split.serial_tx_rx)
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
        assert_eq!(&frame[..], b"hello\n");
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
}
