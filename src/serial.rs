//! Real serial port reader/writer with reconnect and mock loopback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::{Condvar, Mutex};
use serialport::{DataBits, FlowControl, Parity, SerialPortType, StopBits};
use tokio::sync::mpsc;

use crate::broker::{Broker, ControlCommand, DeviceWrite, PortStatus, SerialControl};
use crate::config::RealPortConfig;

pub struct SerialHub {
    stop: Arc<StopSignal>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

const MAX_READ_POLL_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Default)]
struct StopSignal {
    requested: AtomicBool,
    wait_lock: Mutex<()>,
    wake: Condvar,
}

impl StopSignal {
    fn request(&self) {
        // Pair the state change with the condition-variable mutex so a waiter
        // cannot observe `false` and then miss the notification before it
        // actually starts waiting.
        let _guard = self.wait_lock.lock();
        self.requested.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// Wait for `timeout`, returning early when shutdown is requested.
    fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.is_requested() {
            return true;
        }

        let mut guard = self.wait_lock.lock();
        if self.is_requested() {
            return true;
        }
        self.wake.wait_for(&mut guard, timeout);
        self.is_requested()
    }
}

impl SerialHub {
    pub fn start(
        cfg: RealPortConfig,
        broker: Broker,
        to_device: mpsc::Receiver<DeviceWrite>,
    ) -> anyhow::Result<Self> {
        let stop = Arc::new(StopSignal::default());
        let stop_r = stop.clone();
        let cfg_r = cfg.clone();
        let broker_r = broker.clone();
        let (control_tx, control_rx) = mpsc::channel(32);
        broker.attach_serial_control(control_tx);

        // The blocking IO thread owns both the serial port and the bounded Tokio
        // receiver. Keeping the original bounded queue all the way to the device
        // prevents an async-to-std bridge from silently becoming unbounded.
        let join = std::thread::Builder::new()
            .name("ohmyserial-serial".into())
            .spawn(move || serial_thread(cfg_r, broker_r, to_device, control_rx, stop_r))?;

        Ok(Self {
            stop,
            join: Mutex::new(Some(join)),
        })
    }

    pub fn stop(&self) {
        self.stop.request();
        if let Some(join) = self.join.lock().take() {
            let _ = join.join();
        }
    }
}

fn serial_thread(
    cfg: RealPortConfig,
    broker: Broker,
    mut write_rx: mpsc::Receiver<DeviceWrite>,
    mut control_rx: mpsc::Receiver<SerialControl>,
    stop: Arc<StopSignal>,
) {
    if cfg.path.starts_with("mock:") {
        run_mock(&cfg, &broker, &mut write_rx, &mut control_rx, stop);
        return;
    }

    let mut backoff = Duration::from_millis(cfg.reconnect_ms.max(50));
    loop {
        if stop.is_requested() {
            break;
        }

        // Commands accepted for a previous connection must never be replayed
        // against a newly opened device. This is especially important for boot,
        // reset and firmware-update commands.
        drop_pending_writes(&broker, &mut write_rx, "serial_disconnected");
        drop_pending_controls(&mut control_rx, "serial_disconnected");

        match open_port(&cfg) {
            Ok((mut port, resolved_path)) => {
                broker.set_port_status(PortStatus {
                    path: resolved_path.clone(),
                    baud: cfg.baud,
                    connected: true,
                    detail: "open".into(),
                });
                broker
                    .log()
                    .event(&format!("serial_open path={resolved_path}"));
                backoff = Duration::from_millis(cfg.reconnect_ms.max(50));

                if !io_loop(
                    &mut *port,
                    &broker,
                    &mut write_rx,
                    &mut control_rx,
                    &stop,
                    &cfg.flow,
                    cfg.read_timeout_ms,
                ) {
                    break; // shutdown
                }

                broker.set_port_status(PortStatus {
                    path: cfg.path.clone(),
                    baud: cfg.baud,
                    connected: false,
                    detail: "disconnected".into(),
                });
                broker.log().event("serial_disconnected");
                if !cfg.reconnect {
                    break;
                }
            }
            Err(e) => {
                broker.set_port_status(PortStatus {
                    path: cfg.path.clone(),
                    baud: cfg.baud,
                    connected: false,
                    detail: format!("open error: {e}"),
                });
                tracing::warn!("open serial {}: {e}", cfg.path);
                if !cfg.reconnect {
                    break;
                }
                if stop.wait_timeout(backoff) {
                    break;
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
    }
    broker.set_port_status(PortStatus {
        path: cfg.path.clone(),
        baud: cfg.baud,
        connected: false,
        detail: "stopped".into(),
    });
    close_pending_writes(&broker, &mut write_rx, "serial_stopped");
    drop_pending_controls(&mut control_rx, "serial_stopped");
    broker.detach_serial_control();
}

fn io_loop(
    port: &mut dyn serialport::SerialPort,
    broker: &Broker,
    write_rx: &mut mpsc::Receiver<DeviceWrite>,
    control_rx: &mut mpsc::Receiver<SerialControl>,
    stop: &StopSignal,
    flow: &str,
    read_timeout_ms: u64,
) -> bool {
    const MAX_WRITE_BURST: usize = 32;
    const MAX_WRITE_BURST_BYTES: usize = 256 * 1024;
    let mut buf = [0u8; 4096];
    loop {
        if stop.is_requested() {
            return false;
        }

        while let Ok(control) = control_rx.try_recv() {
            if !apply_control(port, control, stop, flow) {
                return false;
            }
        }

        // Bound each TX burst so a busy writer cannot starve device RX.
        let mut burst_bytes = 0usize;
        for _ in 0..MAX_WRITE_BURST {
            match write_rx.try_recv() {
                Ok(write) => {
                    if stop.is_requested() {
                        broker.on_device_tx_not_written(write, "serial is stopping");
                        return false;
                    }
                    if let Err(reason) = broker.validate_device_write(&write) {
                        broker.on_device_tx_not_written(write, reason);
                        continue;
                    }
                    let bytes = write.bytes().len();
                    let result = std::io::Write::write_all(&mut *port, write.bytes())
                        .and_then(|_| std::io::Write::flush(&mut *port));
                    if let Err(e) = result {
                        tracing::warn!("serial write error: {e}");
                        broker.on_device_tx_failed(write, e.to_string());
                        return true; // reconnect
                    }
                    broker.on_device_tx_written(write);
                    burst_bytes = burst_bytes.saturating_add(bytes);
                    if burst_bytes >= MAX_WRITE_BURST_BYTES {
                        break;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return false,
            }
        }

        match std::io::Read::read(&mut *port, &mut buf) {
            Ok(0) => {
                // timeout or EOF depending on backend
                if stop.wait_timeout(Duration::from_millis(read_timeout_ms.min(20))) {
                    return false;
                }
            }
            Ok(n) => {
                broker.on_device_rx(Bytes::copy_from_slice(&buf[..n]));
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if stop.wait_timeout(Duration::from_millis(5)) {
                    return false;
                }
            }
            Err(e) => {
                tracing::warn!("serial read error: {e}");
                broker.on_serial_read_gap(format!("serial read error: {e}"));
                return true; // reconnect
            }
        }
    }
}

fn open_port(cfg: &RealPortConfig) -> anyhow::Result<(Box<dyn serialport::SerialPort>, String)> {
    let parity = match cfg.parity.to_lowercase().as_str() {
        "odd" => Parity::Odd,
        "even" => Parity::Even,
        _ => Parity::None,
    };
    let stop = match cfg.stopbits {
        2 => StopBits::Two,
        _ => StopBits::One,
    };
    let data = match cfg.databits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let flow = match cfg.flow.to_lowercase().as_str() {
        "software" => FlowControl::Software,
        "hardware" => FlowControl::Hardware,
        _ => FlowControl::None,
    };

    let resolved_path = resolve_device_path(cfg)?;
    let port = serialport::new(&resolved_path, cfg.baud)
        .data_bits(data)
        .parity(parity)
        .stop_bits(stop)
        .flow_control(flow)
        // The timeout is an internal polling interval, not a framing or data
        // retention promise. Capping it keeps shutdown bounded even if a
        // configuration supplies a very large value; reads still return as
        // soon as bytes arrive.
        .timeout(read_poll_timeout(cfg.read_timeout_ms))
        .open()?;
    Ok((port, resolved_path))
}

fn resolve_device_path(cfg: &RealPortConfig) -> anyhow::Result<String> {
    let Some(selector) = &cfg.usb else {
        return Ok(cfg.path.clone());
    };
    let ports = serialport::available_ports()?;
    let matches = ports
        .into_iter()
        .filter_map(|port| match port.port_type {
            SerialPortType::UsbPort(info)
                if info.vid == selector.vid
                    && info.pid == selector.pid
                    && selector
                        .serial_number
                        .as_ref()
                        .is_none_or(|serial| info.serial_number.as_deref() == Some(serial)) =>
            {
                Some(port.port_name)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => anyhow::bail!(
            "USB selector {:04x}:{:04x}{} matched no serial port",
            selector.vid,
            selector.pid,
            selector
                .serial_number
                .as_deref()
                .map_or(String::new(), |serial| format!(" serial={serial}"))
        ),
        [path] => Ok(path.clone()),
        paths => anyhow::bail!(
            "USB selector {:04x}:{:04x} is ambiguous; matches: {} (set serial_number)",
            selector.vid,
            selector.pid,
            paths.join(", ")
        ),
    }
}

fn read_poll_timeout(configured_ms: u64) -> Duration {
    Duration::from_millis(configured_ms.max(10)).min(MAX_READ_POLL_TIMEOUT)
}

fn apply_control(
    port: &mut dyn serialport::SerialPort,
    control: SerialControl,
    stop: &StopSignal,
    flow: &str,
) -> bool {
    let SerialControl::Command {
        command,
        acknowledgement,
    } = control;
    let result = match command {
        ControlCommand::Dtr(level) => port
            .write_data_terminal_ready(level)
            .map_err(|error| format!("DTR operation failed: {error}")),
        ControlCommand::Rts(level) => {
            if flow.eq_ignore_ascii_case("hardware") {
                Err("RTS control is unavailable while hardware flow control is active".into())
            } else {
                port.write_request_to_send(level)
                    .map_err(|error| format!("RTS operation failed: {error}"))
            }
        }
        ControlCommand::Break { duration_ms } => {
            let result = port
                .set_break()
                .map_err(|error| format!("BREAK assertion failed: {error}"));
            if result.is_ok() {
                let interrupted = stop.wait_timeout(Duration::from_millis(duration_ms));
                let clear = port
                    .clear_break()
                    .map_err(|error| format!("BREAK clear failed: {error}"));
                if interrupted {
                    Err("serial owner stopped during BREAK".into())
                } else {
                    clear
                }
            } else {
                result
            }
        }
    };
    let stopping = stop.is_requested();
    let _ = acknowledgement.send(result);
    !stopping
}

/// Mock serial: pairs TX back as RX (loopback) and accepts inject via optional channel later.
fn run_mock(
    cfg: &RealPortConfig,
    broker: &Broker,
    write_rx: &mut mpsc::Receiver<DeviceWrite>,
    control_rx: &mut mpsc::Receiver<SerialControl>,
    stop: Arc<StopSignal>,
) {
    broker.set_port_status(PortStatus {
        path: cfg.path.clone(),
        baud: cfg.baud,
        connected: true,
        detail: "mock loopback".into(),
    });
    broker
        .log()
        .event(&format!("serial_open path={} (mock)", cfg.path));

    // Optional inject path: mock:name listens on a side channel via global? Keep simple loopback.
    while !stop.is_requested() {
        while let Ok(SerialControl::Command {
            acknowledgement, ..
        }) = control_rx.try_recv()
        {
            let _ = acknowledgement.send(Err(
                "physical serial control lines are unavailable in mock mode".into(),
            ));
        }
        match write_rx.try_recv() {
            Ok(write) => {
                if let Err(reason) = broker.validate_device_write(&write) {
                    broker.on_device_tx_not_written(write, reason);
                    continue;
                }
                // loopback
                let data = write.bytes().clone();
                broker.on_device_tx_written(write);
                broker.on_device_rx(data);
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                if stop.wait_timeout(Duration::from_millis(10)) {
                    break;
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }

    broker.set_port_status(PortStatus {
        path: cfg.path.clone(),
        baud: cfg.baud,
        connected: false,
        detail: "mock closed".into(),
    });
    close_pending_writes(broker, write_rx, "serial_stopped");
    drop_pending_controls(control_rx, "serial_stopped");
    broker.detach_serial_control();
}

impl Drop for SerialHub {
    fn drop(&mut self) {
        self.stop();
    }
}

fn drop_pending_writes(broker: &Broker, write_rx: &mut mpsc::Receiver<DeviceWrite>, reason: &str) {
    let mut chunks = 0_u64;
    let mut bytes = 0_u64;
    while let Ok(write) = write_rx.try_recv() {
        chunks += 1;
        bytes += write.bytes().len() as u64;
        broker.on_device_tx_not_written(write, reason);
    }
    if chunks > 0 {
        broker.log().event(&format!(
            "tx_gap reason={reason} chunks={chunks} bytes={bytes}"
        ));
        tracing::warn!(chunks, bytes, reason, "discarded stale serial writes");
    }
}

fn drop_pending_controls(control_rx: &mut mpsc::Receiver<SerialControl>, reason: &str) {
    while let Ok(SerialControl::Command {
        acknowledgement, ..
    }) = control_rx.try_recv()
    {
        let _ = acknowledgement.send(Err(format!("serial control was not attempted: {reason}")));
    }
}

fn close_pending_writes(broker: &Broker, write_rx: &mut mpsc::Receiver<DeviceWrite>, reason: &str) {
    write_rx.close();
    let mut chunks = 0_u64;
    let mut bytes = 0_u64;
    // After close, blocking_recv waits for every outstanding Sender permit to
    // be sent or dropped. This closes the final shutdown race without polling.
    while let Some(write) = write_rx.blocking_recv() {
        chunks += 1;
        bytes += write.bytes().len() as u64;
        broker.on_device_tx_not_written(write, reason);
    }
    if chunks > 0 {
        broker.log().event(&format!(
            "tx_queue_drained reason={reason} chunks={chunks} bytes={bytes}"
        ));
    }
}

/// List available serial ports on this machine.
pub fn list_ports() -> anyhow::Result<Vec<String>> {
    let ports = serialport::available_ports()?;
    Ok(ports
        .into_iter()
        .map(|p| format!("{} ({:?})", p.port_name, p.port_type))
        .collect())
}

/// Wait until port reports connected or timeout (for tests).
#[allow(dead_code)]
pub async fn wait_connected(broker: &Broker, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if broker.snapshot().port.connected {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Helper used by tests to push device RX without a real port.
#[allow(dead_code)]
pub fn inject_rx(broker: &Broker, data: Bytes) {
    broker.on_device_rx(data);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::SessionLog;
    use crate::policy::{Policy, SlowClientPolicy, TxMode};

    fn test_split(connected: bool) -> crate::broker::BrokerSplit {
        Broker::new(
            Policy {
                mode: TxMode::QueueByLine,
                primary: None,
                write_lock_ms: 1000,
                write_timeout_ms: 1000,
                max_frame_bytes: 1024,
                max_write_bytes: 1024,
                frame_delim: b'\n',
                slow_client: SlowClientPolicy::DropOldest,
                client_queue: 16,
                slow_block_ms: 100,
            },
            PortStatus {
                path: "mock:serial-test".into(),
                baud: 115_200,
                connected,
                detail: "test".into(),
            },
            SessionLog::disabled(),
            1024,
            16,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn closing_a_full_writer_queue_resolves_all_waiters() {
        let split = test_split(true);
        let broker = split.broker;
        let mut write_rx = split.serial_tx_rx;
        let mut writers = Vec::new();
        for index in 0..48u8 {
            let broker = broker.clone();
            writers.push(tokio::spawn(async move {
                broker
                    .api_write_confirmed_with_lease(
                        &format!("writer-{index}"),
                        Bytes::from(vec![index]),
                        None,
                    )
                    .await
            }));
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            while broker.snapshot().clients.len() < 16 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("writers did not fill the bounded queue");

        broker.set_port_status(PortStatus {
            path: "mock:serial-test".into(),
            baud: 115_200,
            connected: false,
            detail: "shutdown".into(),
        });
        let broker_for_close = broker.clone();
        tokio::task::spawn_blocking(move || {
            close_pending_writes(&broker_for_close, &mut write_rx, "test_shutdown")
        })
        .await
        .unwrap();

        for writer in writers {
            let result = tokio::time::timeout(Duration::from_secs(1), writer)
                .await
                .expect("confirmed writer hung during shutdown")
                .unwrap();
            assert!(result.is_err());
        }
        assert!(!broker.snapshot().port.connected);
        assert!(broker.snapshot().clients.is_empty());
    }

    #[tokio::test]
    async fn serial_hub_stop_with_large_read_timeout_is_bounded() {
        let split = test_split(false);
        let broker = split.broker;
        let cfg = RealPortConfig {
            path: "mock:serial-drop".into(),
            usb: None,
            baud: 115_200,
            databits: 8,
            parity: "none".into(),
            stopbits: 1,
            flow: "none".into(),
            reconnect: true,
            reconnect_ms: 10,
            read_timeout_ms: u64::MAX,
        };
        let serial = SerialHub::start(cfg, broker.clone(), split.serial_tx_rx).unwrap();
        assert!(wait_connected(&broker, Duration::from_secs(1)).await);

        let started = std::time::Instant::now();
        serial.stop();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "mock serial shutdown took {:?}",
            started.elapsed()
        );
        assert!(!broker.snapshot().port.connected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serial_hub_stop_interrupts_long_reconnect_backoff() {
        let split = test_split(false);
        let broker = split.broker;
        let cfg = RealPortConfig {
            path: if cfg!(windows) {
                "COM255".into()
            } else {
                "/dev/ohmyserial-port-that-does-not-exist".into()
            },
            usb: None,
            baud: 115_200,
            databits: 8,
            parity: "none".into(),
            stopbits: 1,
            flow: "none".into(),
            reconnect: true,
            reconnect_ms: 60_000,
            read_timeout_ms: u64::MAX,
        };
        let serial = SerialHub::start(cfg, broker.clone(), split.serial_tx_rx).unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if broker.snapshot().port.detail.starts_with("open error:") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("serial thread did not enter reconnect backoff");

        let started = std::time::Instant::now();
        serial.stop();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "reconnect shutdown took {:?}",
            started.elapsed()
        );
        assert!(!broker.snapshot().port.connected);
        assert_eq!(broker.snapshot().port.detail, "stopped");
    }

    #[test]
    fn real_port_read_poll_timeout_has_a_shutdown_safe_upper_bound() {
        assert_eq!(read_poll_timeout(0), Duration::from_millis(10));
        assert_eq!(read_poll_timeout(50), Duration::from_millis(50));
        assert_eq!(read_poll_timeout(u64::MAX), MAX_READ_POLL_TIMEOUT);
    }
}
