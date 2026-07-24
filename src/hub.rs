//! Hub supervisor: wires config → serial + broker + clients.

use crate::broker::{Broker, PortStatus};
use crate::client::{spawn_api_server, spawn_tcp_listener, ApiState};
use crate::config::{ClientConfig, Config};
use crate::observe::SessionLog;
use crate::policy::Policy;
use crate::serial::SerialHub;

#[cfg(unix)]
use crate::client::spawn_pty_client;

pub struct HubHandle {
    serial: SerialHub,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pub broker: Broker,
}

impl HubHandle {
    pub fn shutdown(self) {
        self.serial.stop();
        for t in self.tasks {
            t.abort();
        }
    }
}

pub async fn run_hub(cfg: Config) -> anyhow::Result<HubHandle> {
    let policy = Policy::from_config(&cfg.tx)?;
    let log = SessionLog::from_config(&cfg.log)?;
    log.event("hub_starting");

    let history_cap = cfg
        .clients
        .iter()
        .filter_map(|c| match c {
            ClientConfig::Websocket { history_bytes, .. } => Some(*history_bytes),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let port = PortStatus {
        path: cfg.real.path.clone(),
        baud: cfg.real.baud,
        connected: false,
        detail: "starting".into(),
    };

    let split = Broker::new(policy, port, log.clone(), history_cap, 256);
    let broker = split.broker;
    let serial_rx = split.serial_tx_rx;

    let serial = SerialHub::start(cfg.real.clone(), broker.clone(), serial_rx);

    let mut tasks = Vec::new();

    // Global API (HTTP/WS)
    if cfg.api.enabled {
        let history = history_cap.max(0);
        let st = ApiState {
            broker: broker.clone(),
            default_writer: "api".into(),
            history_on_ws_connect: history,
        };
        tasks.push(spawn_api_server(st, cfg.api.bind.clone()));
    }

    for client in &cfg.clients {
        match client {
            ClientConfig::Tcp {
                name,
                bind,
                can_write,
                can_read,
            } => {
                tasks.push(spawn_tcp_listener(
                    broker.clone(),
                    name.clone(),
                    bind.clone(),
                    *can_read,
                    *can_write,
                ));
            }
            ClientConfig::Websocket {
                name,
                bind,
                can_write: _,
                can_read: _,
                history_bytes,
            } => {
                // Dedicated bind if provided and different from api — otherwise API covers WS.
                if let Some(bind) = bind {
                    if !cfg.api.enabled || bind != &cfg.api.bind {
                        let st = ApiState {
                            broker: broker.clone(),
                            default_writer: name.clone(),
                            history_on_ws_connect: *history_bytes,
                        };
                        tasks.push(spawn_api_server(st, bind.clone()));
                    } else {
                        tracing::info!(
                            "websocket client '{name}' served by global api on {}",
                            cfg.api.bind
                        );
                    }
                } else {
                    tracing::info!(
                        "websocket client '{name}' served by global api on {}",
                        cfg.api.bind
                    );
                }
            }
            ClientConfig::Pty {
                name,
                link,
                can_write,
                can_read,
            } => {
                #[cfg(unix)]
                {
                    tasks.push(spawn_pty_client(
                        broker.clone(),
                        name.clone(),
                        link.clone(),
                        *can_read,
                        *can_write,
                    ));
                }
                #[cfg(not(unix))]
                {
                    let _ = (name, link, can_write, can_read);
                    anyhow::bail!("pty clients are not supported on this platform");
                }
            }
        }
    }

    // Give mock/serial a moment to open.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    log.event("hub_ready");
    tracing::info!("ohmyserial hub ready (port={})", cfg.real.path);

    Ok(HubHandle {
        serial,
        tasks,
        broker,
    })
}
