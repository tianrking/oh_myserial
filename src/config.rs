//! TOML configuration schema for oh_myserial.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub real: RealPortConfig,
    #[serde(default)]
    pub tx: TxConfig,
    #[serde(default)]
    pub clients: Vec<ClientConfig>,
    #[serde(default)]
    pub log: LogConfig,
    /// Optional HTTP/WS control plane bind (also used when a websocket client has no bind).
    #[serde(default)]
    pub api: ApiConfig,
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
    /// Frame delimiter byte when mode is `queue_by_frame` (default 0x0A = '\n').
    #[serde(default = "default_frame_delim")]
    pub frame_delim: u8,
    /// Slow client RX strategy: `drop_oldest` | `disconnect_slow` | `block`
    #[serde(default = "default_slow_client")]
    pub slow_client: String,
    /// Per-client outbound queue capacity (chunks).
    #[serde(default = "default_client_queue")]
    pub client_queue: usize,
}

impl Default for TxConfig {
    fn default() -> Self {
        Self {
            mode: default_tx_mode(),
            primary: None,
            write_lock_ms: default_write_lock_ms(),
            frame_delim: default_frame_delim(),
            slow_client: default_slow_client(),
            client_queue: default_client_queue(),
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
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind: default_api_bind(),
            enabled: true,
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
            log: LogConfig::default(),
            api: ApiConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.real.path.trim().is_empty() {
            anyhow::bail!("real.path must not be empty");
        }
        match self.tx.mode.as_str() {
            "queue_by_line" | "queue_by_frame" | "exclusive" | "primary_wins" => {}
            other => anyhow::bail!("unknown tx.mode: {other}"),
        }
        match self.tx.slow_client.as_str() {
            "drop_oldest" | "disconnect_slow" | "block" => {}
            other => anyhow::bail!("unknown tx.slow_client: {other}"),
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
        if !matches!(self.real.databits, 5 | 6 | 7 | 8) {
            anyhow::bail!("real.databits must be 5..=8");
        }
        if !matches!(self.real.stopbits, 1 | 2) {
            anyhow::bail!("real.stopbits must be 1 or 2");
        }

        let mut names = std::collections::HashSet::new();
        for c in &self.clients {
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
        }
        Ok(())
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
fn default_frame_delim() -> u8 {
    b'\n'
}
fn default_slow_client() -> String {
    "drop_oldest".into()
}
fn default_client_queue() -> usize {
    256
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_validates() {
        Config::default().validate().unwrap();
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
        let cfg: Config = toml::from_str(toml).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.real.baud, 9600);
        assert_eq!(cfg.clients.len(), 1);
    }
}
