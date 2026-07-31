//! Hub supervisor: wires config → serial + broker + clients.

use crate::broker::{Broker, EndpointView, PortStatus};
use crate::client::{spawn_api_server, spawn_tcp_listener, ApiState};
use crate::config::{ClientConfig, Config};
use crate::observe::SessionLog;
use crate::policy::Policy;
use crate::serial::SerialHub;

#[cfg(unix)]
use crate::client::{prepare_pty_client, PreparedPtyClient};

pub struct HubHandle {
    serial: SerialHub,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pub broker: Broker,
}

struct StartupTasks(Vec<tokio::task::JoinHandle<()>>);

impl StartupTasks {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, task: tokio::task::JoinHandle<()>) {
        self.0.push(task);
    }

    fn finish(mut self) -> Vec<tokio::task::JoinHandle<()>> {
        std::mem::take(&mut self.0)
    }
}

impl Drop for StartupTasks {
    fn drop(&mut self) {
        for task in self.0.drain(..) {
            task.abort();
        }
    }
}

impl HubHandle {
    fn stop_all(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        self.serial.stop();
    }

    pub fn shutdown(mut self) {
        self.stop_all();
    }
}

impl Drop for HubHandle {
    fn drop(&mut self) {
        self.stop_all();
    }
}

pub async fn run_hub(cfg: Config) -> anyhow::Result<HubHandle> {
    // Re-validate at the process boundary so embedders cannot bypass remote
    // bind authentication requirements by constructing Config directly.
    cfg.validate()?;
    let bearer_token = resolve_bearer_token(&cfg)?;
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
        .unwrap_or(65_536);

    let port = PortStatus {
        path: cfg.real.path.clone(),
        baud: cfg.real.baud,
        connected: false,
        detail: "starting".into(),
    };

    let split = Broker::new(policy, port, log.clone(), history_cap, 256);
    let broker = split.broker;
    let serial_rx = split.serial_tx_rx;

    // Publish endpoint catalog (1 real → many virtual / network endpoints).
    let endpoints: Vec<EndpointView> = cfg
        .endpoint_catalog()
        .into_iter()
        .map(|e| EndpointView {
            kind: e.kind,
            name: e.name,
            address: e.address,
            can_read: e.can_read,
            can_write: e.can_write,
            note: e.note,
        })
        .collect();
    for ep in &endpoints {
        tracing::info!(
            "fanout endpoint kind={} name={} address={} (r={} w={}) — {}",
            ep.kind,
            ep.name,
            ep.address,
            ep.can_read,
            ep.can_write,
            ep.note
        );
        log.event(&format!(
            "endpoint kind={} name={} address={}",
            ep.kind, ep.name, ep.address
        ));
    }
    broker.set_endpoints(endpoints);

    // Until startup completes this guard aborts every already-started endpoint
    // if a later bind fails.
    let mut tasks = StartupTasks::new();
    #[cfg(unix)]
    let mut prepared_ptys = Vec::<PreparedPtyClient>::new();

    // Global API (HTTP/WS) — one bind, unlimited concurrent WebSocket monitors/agents.
    if cfg.api.enabled {
        let (ws_writer, ws_can_read, ws_can_write, history) = cfg
            .global_websocket_client()
            .map(|(name, can_read, can_write, history)| {
                (name.to_owned(), can_read, can_write, history)
            })
            .unwrap_or_else(|| {
                (
                    "api".to_owned(),
                    cfg.api.can_read,
                    cfg.api.can_write,
                    history_cap,
                )
            });
        let st = ApiState {
            broker: broker.clone(),
            default_writer: "api".into(),
            ws_writer,
            history_on_ws_connect: history,
            bearer_token: bearer_token.clone(),
            cors_origins: cfg.api.cors_origins.clone(),
            can_read: cfg.api.can_read,
            can_write: cfg.api.can_write,
            ws_can_read,
            ws_can_write,
        };
        tasks.push(spawn_api_server(st, cfg.api.bind.clone()).await?);
    }

    for client in &cfg.clients {
        match client {
            ClientConfig::Tcp {
                name,
                bind,
                can_write,
                can_read,
            } => {
                tasks.push(
                    spawn_tcp_listener(
                        broker.clone(),
                        name.clone(),
                        bind.clone(),
                        *can_read,
                        *can_write,
                    )
                    .await?,
                );
            }
            ClientConfig::Websocket {
                name,
                bind,
                can_write,
                can_read,
                history_bytes,
            } => {
                // Dedicated bind if provided and different from api — otherwise API covers WS.
                if let Some(bind) = bind {
                    if !cfg.api.enabled || bind != &cfg.api.bind {
                        let st = ApiState {
                            broker: broker.clone(),
                            default_writer: name.clone(),
                            ws_writer: name.clone(),
                            history_on_ws_connect: *history_bytes,
                            bearer_token: bearer_token.clone(),
                            cors_origins: cfg.api.cors_origins.clone(),
                            can_read: *can_read,
                            can_write: *can_write,
                            ws_can_read: *can_read,
                            ws_can_write: *can_write,
                        };
                        tasks.push(spawn_api_server(st, bind.clone()).await?);
                    } else {
                        tracing::info!(
                            "websocket client '{name}' served by global api on {}",
                            cfg.api.bind
                        );
                    }
                } else {
                    tracing::info!(
                        "websocket client '{name}' served by global api on {} (/v1/stream, multi-client)",
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
                    prepared_ptys.push(prepare_pty_client(
                        broker.clone(),
                        name.clone(),
                        link.clone(),
                        *can_read,
                        *can_write,
                    )?);
                }
                #[cfg(not(unix))]
                {
                    let _ = (name, link, can_write, can_read);
                    anyhow::bail!("pty clients are not supported on this platform");
                }
            }
        }
    }

    // No real serial handle is opened until every listener has bound and every
    // PTY has completed its fallible OS setup. A later startup error therefore
    // cannot pulse DTR/reset a device and then leave the advertised hub dead.
    let serial = SerialHub::start(cfg.real.clone(), broker.clone(), serial_rx)?;
    #[cfg(unix)]
    for prepared in prepared_ptys {
        tasks.push(prepared.start());
    }

    // Give mock/serial a moment to open.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    log.event("hub_ready");
    tracing::info!(
        "ohmyserial hub ready (real={} → {} fan-out endpoints)",
        cfg.real.path,
        cfg.endpoint_catalog().len()
    );

    // Always print a plain human guide (not only tracing).
    eprint!("{}", cfg.connect_guide());

    Ok(HubHandle {
        serial,
        tasks: tasks.finish(),
        broker,
    })
}

fn resolve_bearer_token(cfg: &Config) -> anyhow::Result<Option<String>> {
    let Some(name) = cfg.api.token_env.as_deref() else {
        return Ok(None);
    };
    let name = name.trim();
    let token = std::env::var(name).map_err(|_| {
        anyhow::anyhow!("api.token_env variable '{name}' is missing or not Unicode")
    })?;
    if token.is_empty()
        || !token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~'))
    {
        anyhow::bail!(
            "api.token_env variable '{name}' must contain a non-empty URL-safe token (letters, digits, '-', '.', '_', '~')"
        );
    }
    Ok(Some(token))
}
