//! Hub supervisor: wires config → serial + broker + clients.

use crate::broker::{Broker, EndpointView, PortStatus};
use crate::client::{
    spawn_api_server_owned, spawn_rfc2217_listener, spawn_tcp_listener, ApiServerHandle, ApiState,
};
use crate::config::{ClientConfig, Config};
use crate::ledger::{Ledger, LedgerOptions, MemoryOptions, StoreOptions};
use crate::observe::SessionLog;
use crate::policy::Policy;
use crate::serial::SerialHub;
use crate::workflow::{WorkflowLimits, WorkflowRunner};

#[cfg(unix)]
use crate::client::{prepare_pty_client, PreparedPtyClient};

pub struct HubHandle {
    serial: SerialHub,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    api_servers: Vec<ApiServerHandle>,
    pub broker: Broker,
    stopped: bool,
}

struct StartupTasks {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    api_servers: Vec<ApiServerHandle>,
}

impl StartupTasks {
    fn new() -> Self {
        Self {
            tasks: Vec::new(),
            api_servers: Vec::new(),
        }
    }

    fn push(&mut self, task: tokio::task::JoinHandle<()>) {
        self.tasks.push(task);
    }

    fn push_api(&mut self, server: ApiServerHandle) {
        self.api_servers.push(server);
    }

    fn finish(mut self) -> (Vec<tokio::task::JoinHandle<()>>, Vec<ApiServerHandle>) {
        (
            std::mem::take(&mut self.tasks),
            std::mem::take(&mut self.api_servers),
        )
    }
}

impl Drop for StartupTasks {
    fn drop(&mut self) {
        for server in &self.api_servers {
            server.cancel();
        }
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.api_servers.clear();
    }
}

impl HubHandle {
    fn stop_all(&mut self) {
        if self.stopped {
            return;
        }
        let _ = self.broker.record_control(None, "hub_stopping", None);
        for server in &self.api_servers {
            server.cancel();
        }
        for t in self.tasks.drain(..) {
            t.abort();
        }
        self.api_servers.clear();
        self.serial.stop();
        self.seal_ledger();
        self.stopped = true;
    }

    fn seal_ledger(&self) {
        if let Err(error) = self.broker.ledger().seal() {
            tracing::error!("ledger seal failed during hub shutdown: {error}");
        }
    }

    /// Stop endpoints, wait for their registration guards to run, stop the
    /// serial owner, and only then seal the evidence ledger.
    pub async fn shutdown_gracefully(mut self) {
        let _ = self.broker.record_control(None, "hub_stopping", None);
        for server in &self.api_servers {
            server.cancel();
        }
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        for server in self.api_servers.drain(..) {
            if let Err(error) = server.shutdown().await {
                tracing::warn!("api shutdown task failed: {error}");
            }
        }
        self.serial.stop();
        self.seal_ledger();
        self.stopped = true;
    }

    /// Immediate synchronous fallback for non-async embedders.
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
    let ledger = Ledger::open(LedgerOptions {
        session_id: None,
        memory: MemoryOptions {
            max_events: cfg.ledger.memory_events,
            max_bytes: cfg.ledger.memory_bytes,
        },
        stream_capacity: cfg.ledger.stream_capacity,
        store: cfg.ledger.directory.clone().map(|directory| StoreOptions {
            directory,
            segment_max_bytes: cfg.ledger.rotate_bytes,
            segment_max_events: 100_000,
            flush_every_events: 1,
            fsync_on_flush: cfg.ledger.fsync_each_event,
        }),
    })?;

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

    let split = Broker::new_with_ledger(policy, port, log.clone(), history_cap, 256, ledger);
    let broker = split.broker;
    let serial_rx = split.serial_tx_rx;
    let _ = broker.record_control(None, "hub_starting", None);

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
            workflow_runner: WorkflowRunner::new(WorkflowLimits::default())?,
            default_writer: "api".into(),
            ws_writer,
            history_on_ws_connect: history,
            bearer_token: bearer_token.clone(),
            cors_origins: cfg.api.cors_origins.clone(),
            can_read: cfg.api.can_read,
            can_write: cfg.api.can_write,
            can_control: cfg.api.can_control,
            ws_can_read,
            ws_can_write,
        };
        tasks.push_api(spawn_api_server_owned(st, cfg.api.bind.clone()).await?);
    }

    for client in &cfg.clients {
        match client {
            ClientConfig::Tcp {
                name,
                bind,
                can_write,
                can_read,
                raw,
            } => {
                tasks.push(
                    spawn_tcp_listener(
                        broker.clone(),
                        name.clone(),
                        bind.clone(),
                        *can_read,
                        *can_write,
                        *raw,
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
                            workflow_runner: WorkflowRunner::new(WorkflowLimits::default())?,
                            default_writer: name.clone(),
                            ws_writer: name.clone(),
                            history_on_ws_connect: *history_bytes,
                            bearer_token: bearer_token.clone(),
                            cors_origins: cfg.api.cors_origins.clone(),
                            can_read: *can_read,
                            can_write: *can_write,
                            can_control: cfg.api.can_control,
                            ws_can_read: *can_read,
                            ws_can_write: *can_write,
                        };
                        tasks.push_api(spawn_api_server_owned(st, bind.clone()).await?);
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

    if cfg.rfc2217.enabled {
        tasks.push(
            spawn_rfc2217_listener(
                broker.clone(),
                "rfc2217".into(),
                cfg.rfc2217.bind.clone(),
                cfg.rfc2217.can_read,
                cfg.rfc2217.can_write,
                cfg.rfc2217.can_control,
                cfg.real.serial_settings(),
            )
            .await?,
        );
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
    let _ = broker.record_control(None, "hub_ready", None);
    tracing::info!(
        "ohmyserial hub ready (real={} → {} fan-out endpoints)",
        cfg.real.path,
        cfg.endpoint_catalog().len()
    );

    // Always print a plain human guide (not only tracing).
    eprint!("{}", cfg.connect_guide());

    let (tasks, api_servers) = tasks.finish();
    Ok(HubHandle {
        serial,
        tasks,
        api_servers,
        broker,
        stopped: false,
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
