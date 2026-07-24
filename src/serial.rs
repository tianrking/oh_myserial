//! Real serial port reader/writer with reconnect and mock loopback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serialport::{DataBits, FlowControl, Parity, StopBits};
use tokio::sync::{mpsc, oneshot};

use crate::broker::{Broker, PortStatus};
use crate::config::RealPortConfig;

pub struct SerialHub {
    stop: Arc<AtomicBool>,
}

impl SerialHub {
    pub fn start(
        cfg: RealPortConfig,
        broker: Broker,
        mut to_device: mpsc::Receiver<Bytes>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_r = stop.clone();
        let stop_w = stop.clone();
        let cfg_r = cfg.clone();
        let broker_r = broker.clone();

        // Channel between writer task and the blocking IO thread.
        let (write_tx, write_rx) = std::sync::mpsc::channel::<WriteCmd>();

        // Writer task: async -> blocking thread
        tokio::spawn(async move {
            while let Some(data) = to_device.recv().await {
                if stop_w.load(Ordering::Relaxed) {
                    break;
                }
                if write_tx.send(WriteCmd::Data(data)).is_err() {
                    break;
                }
            }
            let _ = write_tx.send(WriteCmd::Shutdown);
        });

        // Blocking IO thread owns the serial port handle.
        std::thread::Builder::new()
            .name("ohmyserial-serial".into())
            .spawn(move || serial_thread(cfg_r, broker_r, write_rx, stop_r))
            .expect("spawn serial thread");

        Self { stop }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

enum WriteCmd {
    Data(Bytes),
    Shutdown,
}

fn serial_thread(
    cfg: RealPortConfig,
    broker: Broker,
    write_rx: std::sync::mpsc::Receiver<WriteCmd>,
    stop: Arc<AtomicBool>,
) {
    if cfg.path.starts_with("mock:") {
        run_mock(&cfg, &broker, write_rx, stop);
        return;
    }

    let mut backoff = Duration::from_millis(cfg.reconnect_ms.max(50));
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match open_port(&cfg) {
            Ok(mut port) => {
                broker.set_port_status(PortStatus {
                    path: cfg.path.clone(),
                    baud: cfg.baud,
                    connected: true,
                    detail: "open".into(),
                });
                broker.log().event(&format!("serial_open path={}", cfg.path));
                backoff = Duration::from_millis(cfg.reconnect_ms.max(50));

                if !io_loop(&mut *port, &broker, &write_rx, &stop, cfg.read_timeout_ms) {
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
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
    }
}

fn io_loop(
    port: &mut dyn serialport::SerialPort,
    broker: &Broker,
    write_rx: &std::sync::mpsc::Receiver<WriteCmd>,
    stop: &AtomicBool,
    read_timeout_ms: u64,
) -> bool {
    let mut buf = [0u8; 4096];
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }

        // Non-blocking-ish writes first.
        loop {
            match write_rx.try_recv() {
                Ok(WriteCmd::Data(data)) => {
                    if let Err(e) = std::io::Write::write_all(&mut *port, &data) {
                        tracing::warn!("serial write error: {e}");
                        return true; // reconnect
                    }
                    let _ = std::io::Write::flush(&mut *port);
                }
                Ok(WriteCmd::Shutdown) => return false,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
            }
        }

        match std::io::Read::read(&mut *port, &mut buf) {
            Ok(0) => {
                // timeout or EOF depending on backend
                std::thread::sleep(Duration::from_millis(read_timeout_ms.min(20)));
            }
            Ok(n) => {
                broker.on_device_rx(Bytes::copy_from_slice(&buf[..n]));
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                tracing::warn!("serial read error: {e}");
                return true; // reconnect
            }
        }
    }
}

fn open_port(cfg: &RealPortConfig) -> anyhow::Result<Box<dyn serialport::SerialPort>> {
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

    let port = serialport::new(&cfg.path, cfg.baud)
        .data_bits(data)
        .parity(parity)
        .stop_bits(stop)
        .flow_control(flow)
        .timeout(Duration::from_millis(cfg.read_timeout_ms.max(10)))
        .open()?;
    Ok(port)
}

/// Mock serial: pairs TX back as RX (loopback) and accepts inject via optional channel later.
fn run_mock(
    cfg: &RealPortConfig,
    broker: &Broker,
    write_rx: std::sync::mpsc::Receiver<WriteCmd>,
    stop: Arc<AtomicBool>,
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
    while !stop.load(Ordering::Relaxed) {
        match write_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(WriteCmd::Data(data)) => {
                // loopback
                broker.on_device_rx(data);
            }
            Ok(WriteCmd::Shutdown) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    broker.set_port_status(PortStatus {
        path: cfg.path.clone(),
        baud: cfg.baud,
        connected: false,
        detail: "mock closed".into(),
    });
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

/// Graceful join placeholder.
#[allow(dead_code)]
pub struct JoinOnDrop(Option<oneshot::Sender<()>>);
