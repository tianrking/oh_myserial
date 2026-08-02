//! TOML configuration schema for oh_myserial.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub real: RealPortConfig,
    #[serde(default)]
    pub tx: TxConfig,
    /// Explicit client endpoints (PTY / TCP / WebSocket).
    #[serde(default)]
    pub clients: Vec<ClientConfig>,
    /// Bulk fan-out: expand one real port into many virtual serial / TCP / WS endpoints.
    #[serde(default)]
    pub fanout: FanoutConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// Canonical, byte-lossless event evidence. It is always retained in a
    /// bounded memory ring; disk persistence is opt-in via `directory`.
    #[serde(default)]
    pub ledger: LedgerConfig,
    /// Optional HTTP/WS control plane bind (also used when a websocket client has no bind).
    #[serde(default)]
    pub api: ApiConfig,
}

/// One-real-port → many parallel monitoring/interaction endpoints.
///
/// After load, [`Config::expand_fanout`] materializes these into [`Config::clients`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FanoutConfig {
    /// How many Unix PTY virtual serial ports to create (macOS/Linux).
    /// Links: `{pty_link_prefix}0`, `{pty_link_prefix}1`, …
    #[serde(default)]
    pub pty_count: u32,
    #[serde(default = "default_pty_link_prefix")]
    pub pty_link_prefix: String,
    #[serde(default = "default_pty_name_prefix")]
    pub pty_name_prefix: String,
    #[serde(default = "default_true")]
    pub pty_can_write: bool,
    #[serde(default = "default_true")]
    pub pty_can_read: bool,

    /// How many TCP listeners to open. Each listener accepts **many** concurrent clients;
    /// every connection gets full RX and may TX (per policy).
    #[serde(default)]
    pub tcp_count: u32,
    #[serde(default = "default_tcp_host")]
    pub tcp_host: String,
    #[serde(default = "default_tcp_base_port")]
    pub tcp_base_port: u16,
    #[serde(default = "default_tcp_name_prefix")]
    pub tcp_name_prefix: String,
    #[serde(default = "default_true")]
    pub tcp_can_write: bool,
    #[serde(default = "default_true")]
    pub tcp_can_read: bool,

    /// Extra dedicated HTTP/WS binds (full API + `/v1/stream`).
    /// The primary `[api].bind` already allows unlimited concurrent WebSocket clients.
    #[serde(default)]
    pub ws_binds: Vec<String>,
    #[serde(default = "default_ws_name_prefix")]
    pub ws_name_prefix: String,
    #[serde(default = "default_history_bytes")]
    pub ws_history_bytes: usize,
    #[serde(default = "default_true")]
    pub ws_can_write: bool,
    #[serde(default = "default_true")]
    pub ws_can_read: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealPortConfig {
    /// Serial device path, e.g. `/dev/tty.usbmodem*` or `COM3`.
    /// Use `mock:` prefix for in-process loopback (testing without hardware).
    pub path: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default = "default_databits")]
    pub databits: u8,
    #[serde(default = "default_parity")]
    pub parity: String,
    #[serde(default = "default_stopbits")]
    pub stopbits: u8,
    #[serde(default = "default_flow")]
    pub flow: String,
    #[serde(default = "default_true")]
    pub reconnect: bool,
    #[serde(default = "default_reconnect_ms")]
    pub reconnect_ms: u64,
    /// Read timeout in milliseconds for the serial reader thread.
    #[serde(default = "default_read_timeout_ms")]
    pub read_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxConfig {
    /// `queue_by_line` | `queue_by_frame` | `exclusive` | `primary_wins`
    #[serde(default = "default_tx_mode")]
    pub mode: String,
    /// Client name preferred under `primary_wins`.
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default = "default_write_lock_ms")]
    pub write_lock_ms: u64,
    /// End-to-end deadline for queue admission and confirmed host writes.
    #[serde(default = "default_write_timeout_ms")]
    pub write_timeout_ms: u64,
    /// Maximum bytes retained for one delimiter-framed client command.
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes: usize,
    /// Maximum bytes accepted by one atomic write.
    #[serde(default = "default_max_write_bytes")]
    pub max_write_bytes: usize,
    /// Frame delimiter byte when mode is `queue_by_frame` (default 0x0A = '\n').
    #[serde(default = "default_frame_delim")]
    pub frame_delim: u8,
    /// Slow client RX strategy: `drop_oldest` | `drop_newest` | `disconnect_slow` | `block`
    #[serde(default = "default_slow_client")]
    pub slow_client: String,
    /// Per-client outbound queue capacity (chunks).
    #[serde(default = "default_client_queue")]
    pub client_queue: usize,
    /// Maximum time `slow_client = "block"` may stall fan-out before the slow
    /// client is disconnected to protect the serial owner.
    #[serde(default = "default_slow_block_ms")]
    pub slow_block_ms: u64,
}

impl Default for TxConfig {
    fn default() -> Self {
        Self {
            mode: default_tx_mode(),
            primary: None,
            write_lock_ms: default_write_lock_ms(),
            write_timeout_ms: default_write_timeout_ms(),
            max_frame_bytes: default_max_frame_bytes(),
            max_write_bytes: default_max_write_bytes(),
            frame_delim: default_frame_delim(),
            slow_client: default_slow_client(),
            client_queue: default_client_queue(),
            slow_block_ms: default_slow_block_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClientConfig {
    #[serde(rename = "pty")]
    Pty {
        name: String,
        /// Symlink path presented to host tools, e.g. `/tmp/ohmyserial-ui`.
        link: PathBuf,
        #[serde(default = "default_true")]
        can_write: bool,
        #[serde(default = "default_true")]
        can_read: bool,
    },
    #[serde(rename = "tcp")]
    Tcp {
        name: String,
        #[serde(default = "default_tcp_bind")]
        bind: String,
        #[serde(default = "default_true")]
        can_write: bool,
        #[serde(default = "default_true")]
        can_read: bool,
    },
    #[serde(rename = "websocket")]
    Websocket {
        name: String,
        /// If set, starts a dedicated listener; otherwise uses global `[api].bind`.
        #[serde(default)]
        bind: Option<String>,
        #[serde(default = "default_true")]
        can_write: bool,
        #[serde(default = "default_true")]
        can_read: bool,
        #[serde(default = "default_history_bytes")]
        history_bytes: usize,
    },
}

impl ClientConfig {
    pub fn name(&self) -> &str {
        match self {
            ClientConfig::Pty { name, .. }
            | ClientConfig::Tcp { name, .. }
            | ClientConfig::Websocket { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    #[serde(default)]
    pub file: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub mirror_console: bool,
    /// `text` | `hex` | `hex+text`
    #[serde(default = "default_log_format")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerConfig {
    /// Optional root directory for hashed NDJSON session segments.
    #[serde(default)]
    pub directory: Option<PathBuf>,
    #[serde(default = "default_ledger_memory_events")]
    pub memory_events: usize,
    #[serde(default = "default_ledger_memory_bytes")]
    pub memory_bytes: usize,
    #[serde(default = "default_ledger_stream_capacity")]
    pub stream_capacity: usize,
    #[serde(default = "default_ledger_rotate_bytes")]
    pub rotate_bytes: u64,
    /// Stronger durability at the cost of a disk sync for every event.
    #[serde(default)]
    pub fsync_each_event: bool,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            directory: None,
            memory_events: default_ledger_memory_events(),
            memory_bytes: default_ledger_memory_bytes(),
            stream_capacity: default_ledger_stream_capacity(),
            rotate_bytes: default_ledger_rotate_bytes(),
            fsync_each_event: false,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            file: None,
            mirror_console: true,
            format: default_log_format(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    #[serde(default = "default_api_bind")]
    pub bind: String,
    /// Enable HTTP/WS API server (status, write, lock, stream).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Environment variable containing the bearer token. The token itself is
    /// deliberately never accepted in TOML.
    #[serde(default)]
    pub token_env: Option<String>,
    /// Explicit cross-origin browser origins. An empty list is same-origin
    /// only; wildcard origins are intentionally unsupported.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// Permit status/history/RX access through this API listener.
    #[serde(default = "default_true")]
    pub can_read: bool,
    /// Permit write/lease/TX access through this API listener.
    #[serde(default = "default_true")]
    pub can_write: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: default_api_bind(),
            enabled: true,
            token_env: None,
            cors_origins: Vec::new(),
            can_read: true,
            can_write: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            real: RealPortConfig {
                path: "mock:demo".into(),
                baud: default_baud(),
                databits: default_databits(),
                parity: default_parity(),
                stopbits: default_stopbits(),
                flow: default_flow(),
                reconnect: true,
                reconnect_ms: default_reconnect_ms(),
                read_timeout_ms: default_read_timeout_ms(),
            },
            tx: TxConfig::default(),
            clients: vec![ClientConfig::Tcp {
                name: "tcp".into(),
                bind: default_tcp_bind(),
                can_write: true,
                can_read: true,
            }],
            fanout: FanoutConfig::default(),
            log: LogConfig::default(),
            ledger: LedgerConfig::default(),
            api: ApiConfig::default(),
        }
    }
}

/// Static description of a configured fan-out endpoint (for status / docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointDesc {
    pub kind: String,
    pub name: String,
    /// Path, bind address, or URL path depending on kind.
    pub address: String,
    pub can_read: bool,
    pub can_write: bool,
    /// Human note, e.g. "many concurrent connections".
    pub note: String,
}

/// Options for one-shot CLI share (no TOML required).
#[derive(Debug, Clone)]
pub struct QuickShare {
    pub device: String,
    pub baud: u32,
    pub pty_count: u32,
    pub tcp_count: u32,
    pub tcp_base_port: u16,
    pub api_bind: String,
    pub mirror_console: bool,
}

impl Default for QuickShare {
    fn default() -> Self {
        Self {
            device: "mock:demo".into(),
            baud: 115_200,
            // People-friendly defaults: 2 virtual serials on Unix; TCP everywhere.
            pty_count: default_friendly_pty_count(),
            tcp_count: 1,
            tcp_base_port: 8788,
            api_bind: "127.0.0.1:8787".into(),
            mirror_console: true,
        }
    }
}

fn default_friendly_pty_count() -> u32 {
    #[cfg(unix)]
    {
        2
    }
    #[cfg(not(unix))]
    {
        0
    }
}

impl Config {
    /// WebSocket identity served by the global API listener, if explicitly
    /// configured. A single `/v1/stream` route cannot truthfully represent
    /// more than one configured identity/permission set.
    pub(crate) fn global_websocket_client(&self) -> Option<(&str, bool, bool, usize)> {
        if !self.api.enabled {
            return None;
        }
        self.clients.iter().find_map(|client| match client {
            ClientConfig::Websocket {
                name,
                bind,
                can_write,
                can_read,
                history_bytes,
            } if bind.as_ref().is_none_or(|bind| bind == &self.api.bind) => {
                Some((name.as_str(), *can_read, *can_write, *history_bytes))
            }
            _ => None,
        })
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut cfg: Config = toml::from_str(&text)?;
        cfg.expand_fanout()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Build a ready-to-run config from CLI flags (zero TOML for common cases).
    pub fn from_quick(q: QuickShare) -> anyhow::Result<Self> {
        let mut cfg = Config {
            real: RealPortConfig {
                path: q.device,
                baud: q.baud,
                databits: 8,
                parity: "none".into(),
                stopbits: 1,
                flow: "none".into(),
                reconnect: true,
                reconnect_ms: 1000,
                read_timeout_ms: 50,
            },
            tx: TxConfig::default(),
            clients: Vec::new(),
            fanout: FanoutConfig {
                pty_count: q.pty_count,
                pty_link_prefix: default_pty_link_prefix(),
                pty_name_prefix: default_pty_name_prefix(),
                pty_can_write: true,
                pty_can_read: true,
                tcp_count: q.tcp_count,
                tcp_host: default_tcp_host(),
                tcp_base_port: q.tcp_base_port,
                tcp_name_prefix: default_tcp_name_prefix(),
                tcp_can_write: true,
                tcp_can_read: true,
                ws_binds: Vec::new(),
                ws_name_prefix: default_ws_name_prefix(),
                ws_history_bytes: default_history_bytes(),
                ws_can_write: true,
                ws_can_read: true,
            },
            log: LogConfig {
                file: None,
                mirror_console: q.mirror_console,
                format: "hex+text".into(),
            },
            ledger: LedgerConfig::default(),
            api: ApiConfig {
                bind: q.api_bind,
                enabled: true,
                token_env: None,
                cors_origins: Vec::new(),
                can_read: true,
                can_write: true,
            },
        };
        cfg.expand_fanout()?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Human-readable "how to connect" card for terminals.
    pub fn connect_guide(&self) -> String {
        let mut lines = Vec::new();
        lines.push(String::new());
        lines.push("┌─────────────────────────────────────────────────────────────┐".into());
        lines.push("│  ohmyserial — how to connect (same live data everywhere)   │".into());
        lines.push("├─────────────────────────────────────────────────────────────┤".into());
        lines.push(format!(
            "│  REAL   {path:<52}│",
            path = truncate(&self.real.path, 52)
        ));
        lines.push(format!(
            "│  BAUD   {baud:<52}│",
            baud = self.real.baud.to_string()
        ));
        lines.push("├─────────────────────────────────────────────────────────────┤".into());

        for ep in self.endpoint_catalog() {
            let label = match ep.kind.as_str() {
                "pty" => "SERIAL",
                "tcp" => "TCP   ",
                "websocket" => "WS    ",
                "http" => "HTTP  ",
                _ => "OTHER ",
            };
            lines.push(format!(
                "│  {label} {addr:<52}│",
                addr = truncate(&ep.address, 52)
            ));
        }

        lines.push("├─────────────────────────────────────────────────────────────┤".into());
        if self.api.enabled {
            lines.push(format!(
                "│  UI     {url:<52}│",
                url = truncate(&format!("http://{}/", self.api.bind), 52)
            ));
        }
        lines.push("├─────────────────────────────────────────────────────────────┤".into());
        lines.push("│  Tips                                                       │".into());
        lines.push("│  • Browser UI: open the UI URL above (embedded console)     │".into());
        lines.push("│  • Open each SERIAL path in a different serial app          │".into());
        lines.push("│  • Agent: WebSocket stream + HTTP POST /v1/write            │".into());
        lines.push("│  • Many programs can share one TCP port                     │".into());
        lines.push("│  • curl http://127.0.0.1:8787/v1/endpoints                  │".into());
        lines.push("│  • Ctrl+C to stop                                           │".into());
        lines.push("└─────────────────────────────────────────────────────────────┘".into());
        lines.push(String::new());
        lines.join("\n")
    }

    /// Expand `[fanout]` into concrete `[[clients]]` entries (idempotent names).
    pub fn expand_fanout(&mut self) -> anyhow::Result<()> {
        let f = self.fanout.clone();
        #[cfg(unix)]
        let existing: std::collections::HashSet<String> =
            self.clients.iter().map(|c| c.name().to_string()).collect();

        // --- PTY bulk ---
        if f.pty_count > 0 {
            #[cfg(not(unix))]
            {
                anyhow::bail!(
                    "fanout.pty_count={} is set but PTY is only supported on macOS/Linux",
                    f.pty_count
                );
            }
            #[cfg(unix)]
            {
                for i in 0..f.pty_count {
                    let name = format!("{}{}", f.pty_name_prefix, i);
                    if existing.contains(&name) || self.clients.iter().any(|c| c.name() == name) {
                        continue;
                    }
                    let link = PathBuf::from(format!("{}{}", f.pty_link_prefix, i));
                    self.clients.push(ClientConfig::Pty {
                        name,
                        link,
                        can_write: f.pty_can_write,
                        can_read: f.pty_can_read,
                    });
                }
            }
        }

        // --- TCP bulk ---
        for i in 0..f.tcp_count {
            let name = format!("{}{}", f.tcp_name_prefix, i);
            if self.clients.iter().any(|c| c.name() == name) {
                continue;
            }
            let port = f
                .tcp_base_port
                .checked_add(i as u16)
                .ok_or_else(|| anyhow::anyhow!("tcp_base_port overflow"))?;
            let bind = format!("{}:{}", f.tcp_host, port);
            self.clients.push(ClientConfig::Tcp {
                name,
                bind,
                can_write: f.tcp_can_write,
                can_read: f.tcp_can_read,
            });
        }

        // --- Extra WS binds ---
        for (i, bind) in f.ws_binds.iter().enumerate() {
            let name = format!("{}{}", f.ws_name_prefix, i);
            if self.clients.iter().any(|c| c.name() == name) {
                continue;
            }
            self.clients.push(ClientConfig::Websocket {
                name,
                bind: Some(bind.clone()),
                can_write: f.ws_can_write,
                can_read: f.ws_can_read,
                history_bytes: f.ws_history_bytes,
            });
        }

        Ok(())
    }

    /// Describe all endpoints that will be offered (after expand).
    pub fn endpoint_catalog(&self) -> Vec<EndpointDesc> {
        let mut out = Vec::new();
        if self.api.enabled {
            out.push(EndpointDesc {
                kind: "http".into(),
                name: "api".into(),
                address: format!("http://{}", self.api.bind),
                can_read: self.api.can_read,
                can_write: self.api.can_write,
                note: "control plane; unlimited concurrent WS on /v1/stream".into(),
            });
            let (stream_name, stream_can_read, stream_can_write) = self
                .global_websocket_client()
                .map(|(name, can_read, can_write, _)| (name, can_read, can_write))
                .unwrap_or(("api-stream", self.api.can_read, self.api.can_write));
            out.push(EndpointDesc {
                kind: "websocket".into(),
                name: stream_name.into(),
                address: format!("ws://{}/v1/stream", self.api.bind),
                can_read: stream_can_read,
                can_write: stream_can_write,
                note: "many agents/tools can connect at once; full RX fan-out".into(),
            });
        }
        for c in &self.clients {
            match c {
                ClientConfig::Pty {
                    name,
                    link,
                    can_write,
                    can_read,
                } => out.push(EndpointDesc {
                    kind: "pty".into(),
                    name: name.clone(),
                    address: link.display().to_string(),
                    can_read: *can_read,
                    can_write: *can_write,
                    note: "virtual serial for classic host tools (one opener per PTY)".into(),
                }),
                ClientConfig::Tcp {
                    name,
                    bind,
                    can_write,
                    can_read,
                } => out.push(EndpointDesc {
                    kind: "tcp".into(),
                    name: name.clone(),
                    address: bind.clone(),
                    can_read: *can_read,
                    can_write: *can_write,
                    note: "raw stream; many concurrent TCP clients per bind".into(),
                }),
                ClientConfig::Websocket {
                    name,
                    bind,
                    can_write,
                    can_read,
                    ..
                } => {
                    let served_by_global_api =
                        self.api.enabled && bind.as_ref().is_none_or(|bind| bind == &self.api.bind);
                    if served_by_global_api {
                        continue;
                    }
                    let addr = bind
                        .as_ref()
                        .map(|b| format!("ws://{b}/v1/stream"))
                        .unwrap_or_else(|| format!("ws://{}/v1/stream", self.api.bind));
                    out.push(EndpointDesc {
                        kind: "websocket".into(),
                        name: name.clone(),
                        address: addr,
                        can_read: *can_read,
                        can_write: *can_write,
                        note: if bind.is_some() {
                            "dedicated WS/HTTP server bind".into()
                        } else {
                            "served by primary [api] server".into()
                        },
                    });
                }
            }
        }
        out
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.real.path.trim().is_empty() {
            anyhow::bail!("real.path must not be empty");
        }
        let token_env = self.api.token_env.as_deref().map(str::trim);
        if self.api.token_env.is_some() && token_env.is_some_and(str::is_empty) {
            anyhow::bail!("api.token_env must not be empty");
        }
        if token_env.is_some_and(|name| name.contains('=') || name.contains('\0')) {
            anyhow::bail!("api.token_env is not a valid environment variable name");
        }
        if self.api.enabled {
            let api_addr = parse_api_bind(&self.api.bind, "api.bind")?;
            if !api_addr.ip().is_loopback() {
                anyhow::bail!(
                    "plaintext HTTP/WebSocket API must bind to loopback; use an SSH tunnel or a TLS reverse proxy ({})",
                    self.api.bind
                );
            }
            let global_websocket_count = self
                .clients
                .iter()
                .filter(|client| {
                    matches!(
                        client,
                        ClientConfig::Websocket { bind, .. }
                            if bind.as_ref().is_none_or(|bind| bind == &self.api.bind)
                    )
                })
                .count();
            if global_websocket_count > 1 {
                anyhow::bail!(
                    "at most one websocket client may use the global api /v1/stream route"
                );
            }
        }
        for origin in &self.api.cors_origins {
            validate_cors_origin(origin)?;
        }
        for bind in &self.fanout.ws_binds {
            validate_websocket_bind_security(bind)?;
        }
        match self.tx.mode.as_str() {
            "queue_by_line" | "queue_by_frame" | "exclusive" | "primary_wins" => {}
            other => anyhow::bail!("unknown tx.mode: {other}"),
        }
        match self.tx.slow_client.as_str() {
            "drop_oldest" | "drop_newest" | "disconnect_slow" | "block" => {}
            other => anyhow::bail!("unknown tx.slow_client: {other}"),
        }
        if self.tx.client_queue == 0 {
            anyhow::bail!("tx.client_queue must be at least 1");
        }
        if self.tx.write_lock_ms == 0 {
            anyhow::bail!("tx.write_lock_ms must be at least 1");
        }
        if self.tx.write_timeout_ms == 0 {
            anyhow::bail!("tx.write_timeout_ms must be at least 1");
        }
        if self.tx.max_frame_bytes == 0 {
            anyhow::bail!("tx.max_frame_bytes must be at least 1");
        }
        if self.tx.max_write_bytes == 0 {
            anyhow::bail!("tx.max_write_bytes must be at least 1");
        }
        if self.tx.slow_block_ms == 0 {
            anyhow::bail!("tx.slow_block_ms must be at least 1");
        }
        if self.ledger.memory_events == 0 {
            anyhow::bail!("ledger.memory_events must be at least 1");
        }
        if self.ledger.memory_bytes == 0 {
            anyhow::bail!("ledger.memory_bytes must be at least 1");
        }
        if self.ledger.stream_capacity == 0 {
            anyhow::bail!("ledger.stream_capacity must be at least 1");
        }
        if self.ledger.rotate_bytes < 1024 {
            anyhow::bail!("ledger.rotate_bytes must be at least 1024");
        }
        match self.log.format.as_str() {
            "text" | "hex" | "hex+text" => {}
            other => anyhow::bail!("unknown log.format: {other}"),
        }
        match self.real.parity.to_lowercase().as_str() {
            "none" | "odd" | "even" => {}
            other => anyhow::bail!("unknown real.parity: {other}"),
        }
        match self.real.flow.to_lowercase().as_str() {
            "none" | "software" | "hardware" => {}
            other => anyhow::bail!("unknown real.flow: {other}"),
        }
        if !matches!(self.real.databits, 5..=8) {
            anyhow::bail!("real.databits must be 5..=8");
        }
        if !matches!(self.real.stopbits, 1 | 2) {
            anyhow::bail!("real.stopbits must be 1 or 2");
        }

        let mut names = std::collections::HashSet::new();
        for c in &self.clients {
            if c.name().trim().is_empty()
                || c.name().len() > 128
                || c.name().chars().any(char::is_control)
            {
                anyhow::bail!(
                    "client name must be non-empty, at most 128 bytes, and contain no control characters"
                );
            }
            if !names.insert(c.name().to_string()) {
                anyhow::bail!("duplicate client name: {}", c.name());
            }
            #[cfg(not(unix))]
            if matches!(c, ClientConfig::Pty { .. }) {
                anyhow::bail!(
                    "client '{}' type=pty is only supported on macOS/Linux",
                    c.name()
                );
            }
            match c {
                ClientConfig::Tcp { bind, .. } => {
                    let addr = parse_api_bind(bind, "tcp bind")?;
                    if !addr.ip().is_loopback() {
                        anyhow::bail!(
                            "raw TCP has no authentication and must bind to loopback; use an SSH tunnel for remote access ({bind})"
                        );
                    }
                }
                ClientConfig::Websocket {
                    bind: Some(bind), ..
                } => validate_websocket_bind_security(bind)?,
                ClientConfig::Websocket { bind: None, .. } if !self.api.enabled => {
                    anyhow::bail!(
                        "websocket client '{}' has no bind while api.enabled=false",
                        c.name()
                    );
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn parse_api_bind(bind: &str, field: &str) -> anyhow::Result<std::net::SocketAddr> {
    bind.parse()
        .map_err(|e| anyhow::anyhow!("invalid {field} '{bind}': {e}"))
}

fn validate_websocket_bind_security(bind: &str) -> anyhow::Result<()> {
    let addr = parse_api_bind(bind, "websocket bind")?;
    if !addr.ip().is_loopback() {
        anyhow::bail!(
            "plaintext dedicated WebSocket must bind to loopback; use an SSH tunnel or a TLS reverse proxy ({bind})"
        );
    }
    Ok(())
}

fn validate_cors_origin(origin: &str) -> anyhow::Result<()> {
    let trimmed = origin.trim();
    if trimmed.is_empty() || trimmed == "*" {
        anyhow::bail!("api.cors_origins must contain explicit origins, never wildcard '*'");
    }
    if trimmed != origin || trimmed.chars().any(char::is_whitespace) {
        anyhow::bail!("invalid CORS origin '{origin}'");
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        anyhow::bail!("CORS origin must start with http:// or https://: '{origin}'");
    }
    let authority = trimmed
        .split_once("://")
        .map(|(_, authority)| authority)
        .unwrap_or_default();
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        anyhow::bail!("CORS value must be an origin without path/query/fragment: '{origin}'");
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        format!("{s:<max$}")
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        format!("{t:<max$}")
    }
}

fn default_baud() -> u32 {
    115_200
}
fn default_databits() -> u8 {
    8
}
fn default_parity() -> String {
    "none".into()
}
fn default_stopbits() -> u8 {
    1
}
fn default_flow() -> String {
    "none".into()
}
fn default_true() -> bool {
    true
}
fn default_reconnect_ms() -> u64 {
    1000
}
fn default_read_timeout_ms() -> u64 {
    50
}
fn default_tx_mode() -> String {
    "queue_by_line".into()
}
fn default_write_lock_ms() -> u64 {
    3000
}
fn default_write_timeout_ms() -> u64 {
    5000
}
fn default_max_frame_bytes() -> usize {
    65_536
}
fn default_max_write_bytes() -> usize {
    65_536
}
fn default_frame_delim() -> u8 {
    b'\n'
}
fn default_slow_client() -> String {
    "drop_oldest".into()
}
fn default_client_queue() -> usize {
    256
}
fn default_slow_block_ms() -> u64 {
    1000
}
fn default_tcp_bind() -> String {
    "127.0.0.1:8788".into()
}
fn default_api_bind() -> String {
    "127.0.0.1:8787".into()
}
fn default_history_bytes() -> usize {
    65_536
}
fn default_log_format() -> String {
    "hex+text".into()
}
fn default_ledger_memory_events() -> usize {
    16_384
}
fn default_ledger_memory_bytes() -> usize {
    32 * 1024 * 1024
}
fn default_ledger_stream_capacity() -> usize {
    1024
}
fn default_ledger_rotate_bytes() -> u64 {
    64 * 1024 * 1024
}
fn default_pty_link_prefix() -> String {
    "/tmp/ohmyserial-v".into()
}
fn default_pty_name_prefix() -> String {
    "v".into()
}
fn default_tcp_host() -> String {
    "127.0.0.1".into()
}
fn default_tcp_base_port() -> u16 {
    8788
}
fn default_tcp_name_prefix() -> String {
    "tcp".into()
}
fn default_ws_name_prefix() -> String {
    "ws".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn ledger_capacity_and_rotation_limits_are_validated() {
        let mut cfg = Config::default();
        cfg.ledger.memory_events = 0;
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("memory_events"));

        cfg.ledger.memory_events = 1;
        cfg.ledger.memory_bytes = 0;
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("memory_bytes"));

        cfg.ledger.memory_bytes = 1;
        cfg.ledger.stream_capacity = 0;
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("stream_capacity"));

        cfg.ledger.stream_capacity = 1;
        cfg.ledger.rotate_bytes = 1023;
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("rotate_bytes"));
    }

    #[test]
    fn parse_minimal_toml() {
        let toml = r#"
[real]
path = "mock:test"
baud = 9600

[[clients]]
type = "tcp"
name = "a"
bind = "127.0.0.1:9999"
"#;
        let mut cfg: Config = toml::from_str(toml).unwrap();
        cfg.expand_fanout().unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.real.baud, 9600);
        assert_eq!(cfg.clients.len(), 1);
    }

    #[test]
    fn quick_share_builds() {
        let cfg = Config::from_quick(QuickShare {
            device: "mock:x".into(),
            baud: 9600,
            pty_count: 0,
            tcp_count: 2,
            tcp_base_port: 19010,
            api_bind: "127.0.0.1:19011".into(),
            mirror_console: false,
        })
        .unwrap();
        assert_eq!(cfg.real.baud, 9600);
        assert_eq!(cfg.clients.len(), 2);
        let guide = cfg.connect_guide();
        assert!(guide.contains("how to connect"));
        assert!(guide.contains("19010") || guide.contains("TCP"));
    }

    #[test]
    fn fanout_expands_tcp_ports() {
        let toml = r#"
[real]
path = "mock:test"

[fanout]
tcp_count = 3
tcp_host = "127.0.0.1"
tcp_base_port = 19000
tcp_name_prefix = "m"
"#;
        let mut cfg: Config = toml::from_str(toml).unwrap();
        cfg.expand_fanout().unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.clients.len(), 3);
        match &cfg.clients[1] {
            ClientConfig::Tcp { name, bind, .. } => {
                assert_eq!(name, "m1");
                assert_eq!(bind, "127.0.0.1:19001");
            }
            _ => panic!("expected tcp"),
        }
        let eps = cfg.endpoint_catalog();
        assert!(eps
            .iter()
            .any(|e| e.kind == "tcp" && e.address.contains("19001")));
    }

    #[test]
    fn plaintext_non_loopback_api_is_rejected_even_with_a_token() {
        let mut cfg = Config::default();
        cfg.api.bind = "0.0.0.0:8787".into();
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("SSH tunnel"), "error={error}");

        cfg.api.token_env = Some("OHMYSERIAL_API_TOKEN".into());
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("TLS reverse proxy"), "error={error}");
    }

    #[test]
    fn plaintext_non_loopback_websocket_is_rejected_even_with_a_token() {
        let mut cfg = Config::default();
        cfg.clients.push(ClientConfig::Websocket {
            name: "remote-agent".into(),
            bind: Some("0.0.0.0:8790".into()),
            can_write: false,
            can_read: true,
            history_bytes: 1024,
        });
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("SSH tunnel"), "error={error}");

        cfg.api.token_env = Some("OHMYSERIAL_API_TOKEN".into());
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("TLS reverse proxy"), "error={error}");
    }

    #[test]
    fn unauthenticated_raw_tcp_must_remain_loopback_only() {
        let cfg = Config {
            clients: vec![ClientConfig::Tcp {
                name: "unsafe-tcp".into(),
                bind: "0.0.0.0:8788".into(),
                can_write: true,
                can_read: true,
            }],
            ..Config::default()
        };
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("SSH tunnel"), "error={error}");
    }

    #[test]
    fn websocket_without_bind_requires_global_api() {
        let mut cfg = Config::default();
        cfg.api.enabled = false;
        cfg.clients = vec![ClientConfig::Websocket {
            name: "missing".into(),
            bind: None,
            can_write: false,
            can_read: true,
            history_bytes: 1024,
        }];
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("api.enabled=false"), "error={error}");
    }

    #[test]
    fn cors_origins_are_explicit_and_origin_only() {
        let mut cfg = Config::default();
        cfg.api.cors_origins = vec!["*".into()];
        assert!(cfg.validate().unwrap_err().to_string().contains("wildcard"));

        cfg.api.cors_origins = vec!["https://console.example.test/path".into()];
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("without path"));

        cfg.api.cors_origins = vec!["https://console.example.test".into()];
        cfg.validate().unwrap();
    }

    #[test]
    fn global_endpoint_catalog_uses_api_permissions() {
        let mut cfg = Config::default();
        cfg.api.can_read = true;
        cfg.api.can_write = false;
        let endpoints = cfg.endpoint_catalog();
        let api = endpoints.iter().find(|ep| ep.name == "api").unwrap();
        let stream = endpoints.iter().find(|ep| ep.name == "api-stream").unwrap();
        assert!(api.can_read && !api.can_write);
        assert!(stream.can_read && !stream.can_write);
    }

    #[test]
    fn configured_global_websocket_identity_and_permissions_are_exact() {
        let mut cfg = Config::default();
        cfg.api.can_read = false;
        cfg.api.can_write = true;
        cfg.clients = vec![ClientConfig::Websocket {
            name: "agent".into(),
            bind: None,
            can_write: false,
            can_read: true,
            history_bytes: 2048,
        }];
        cfg.validate().unwrap();

        assert_eq!(
            cfg.global_websocket_client(),
            Some(("agent", true, false, 2048))
        );
        let endpoints = cfg.endpoint_catalog();
        assert_eq!(
            endpoints
                .iter()
                .filter(|endpoint| endpoint.kind == "websocket")
                .count(),
            1
        );
        let stream = endpoints
            .iter()
            .find(|endpoint| endpoint.kind == "websocket")
            .unwrap();
        assert_eq!(stream.name, "agent");
        assert!(stream.can_read && !stream.can_write);
    }

    #[test]
    fn global_api_rejects_ambiguous_websocket_identities() {
        let mut cfg = Config::default();
        cfg.clients = vec![
            ClientConfig::Websocket {
                name: "agent-a".into(),
                bind: None,
                can_write: false,
                can_read: true,
                history_bytes: 1024,
            },
            ClientConfig::Websocket {
                name: "agent-b".into(),
                bind: Some(cfg.api.bind.clone()),
                can_write: true,
                can_read: true,
                history_bytes: 1024,
            },
        ];
        let error = cfg.validate().unwrap_err().to_string();
        assert!(error.contains("at most one websocket client"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn fanout_expands_pty() {
        let toml = r#"
[real]
path = "mock:test"

[fanout]
pty_count = 2
pty_link_prefix = "/tmp/oms-test-v"
pty_name_prefix = "p"
"#;
        let mut cfg: Config = toml::from_str(toml).unwrap();
        cfg.expand_fanout().unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.clients.len(), 2);
        match &cfg.clients[0] {
            ClientConfig::Pty { name, link, .. } => {
                assert_eq!(name, "p0");
                assert_eq!(link.to_string_lossy(), "/tmp/oms-test-v0");
            }
            _ => panic!("expected pty"),
        }
    }
}
