//! oh_myserial — cross-platform serial hub for humans and agents.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use ohmyserial::config::Config;
use ohmyserial::{hub, serial};

#[derive(Parser, Debug)]
#[command(
    name = "ohmyserial",
    version,
    about = "Cross-platform open-source serial hub for humans and agents"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the serial hub from a TOML config file.
    Run {
        /// Path to config TOML.
        #[arg(short, long, default_value = "ohmyserial.toml")]
        config: PathBuf,
    },
    /// Print a sample config to stdout.
    Init {
        /// Write to file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// List serial ports visible on this machine.
    ListPorts,
    /// Print status of a running hub via HTTP API.
    Status {
        #[arg(long, default_value = "http://127.0.0.1:8787")]
        api: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Commands::Run { config } => {
            let cfg = Config::load(&config)?;
            tracing::info!("loaded config {}", config.display());
            let handle = hub::run_hub(cfg).await?;
            tracing::info!("press Ctrl+C to stop");
            tokio::signal::ctrl_c().await?;
            tracing::info!("shutting down");
            handle.shutdown();
            Ok(())
        }
        Commands::Init { output } => {
            let sample = sample_config();
            if let Some(path) = output {
                std::fs::write(&path, sample)?;
                eprintln!("wrote {}", path.display());
            } else {
                print!("{sample}");
            }
            Ok(())
        }
        Commands::ListPorts => {
            for p in serial::list_ports()? {
                println!("{p}");
            }
            Ok(())
        }
        Commands::Status { api } => {
            let url = format!("{}/v1/status", api.trim_end_matches('/'));
            let body = reqwest_get(&url).await?;
            println!("{body}");
            Ok(())
        }
    }
}

/// Minimal HTTP GET without adding reqwest dep — use hyper via awc? Keep simple with std + tokio.
async fn reqwest_get(url: &str) -> anyhow::Result<String> {
    // Use ureq-less approach: tokio tcp is messy for HTTP.
    // Prefer adding no extra dep: shell out is bad.
    // Use `http` raw via hyper is already in axum tree... simplest: use std::process? No.
    // Add nothing — parse URL and use hyper client from axum deps... axum doesn't export client.
    // Use `tokio::process` curl? Portable enough fallback:
    match simple_http_get(url).await {
        Ok(s) => Ok(s),
        Err(_) => {
            // last resort: tell user to curl
            anyhow::bail!("failed to GET {url}; try: curl -s {url}")
        }
    }
}

async fn simple_http_get(url: &str) -> anyhow::Result<String> {
    // Very small HTTP/1.1 client for http://host:port/path
    let url = url.strip_prefix("http://").ok_or_else(|| anyhow::anyhow!("only http:// supported"))?;
    let (host_port, path) = url
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((url, "/".into()));
    let stream = tokio::net::TcpStream::connect(host_port).await?;
    let mut stream = stream;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    if let Some(idx) = text.find("\r\n\r\n") {
        Ok(text[idx + 4..].to_string())
    } else {
        Ok(text.into_owned())
    }
}

fn sample_config() -> String {
    r#"# oh_myserial — one real serial port, many parallel monitors / agents
#
# Real port: device path, or "mock:demo" for loopback without hardware.
# Fan-out: every RX byte is broadcast; TX is arbitrated (see [tx]).

[real]
path = "mock:demo"
baud = 115200
databits = 8
parity = "none"
stopbits = 1
flow = "none"
reconnect = true
reconnect_ms = 1000

[tx]
mode = "queue_by_line"   # queue_by_line | queue_by_frame | exclusive | primary_wins
primary = "ui"
write_lock_ms = 3000
slow_client = "drop_oldest"

# Primary HTTP + WebSocket API (many agents can open /v1/stream at once)
[api]
bind = "127.0.0.1:8787"
enabled = true

# ---- Bulk fan-out (recommended): 1 real → N virtual / network endpoints ----
[fanout]
# Unix virtual serial (macOS/Linux only). Set >0 to create /tmp/ohmyserial-v0, v1, …
# Open each path in a different serial monitor. Windows: leave 0, use TCP/WS.
pty_count = 0
pty_link_prefix = "/tmp/ohmyserial-v"
pty_name_prefix = "v"
pty_can_write = true
pty_can_read = true

# TCP listeners: each port accepts MANY concurrent clients (all get full RX).
# Example: 2 ports → 8788 and 8789; any number of programs may connect to each.
tcp_count = 2
tcp_host = "127.0.0.1"
tcp_base_port = 8788
tcp_name_prefix = "tcp"
tcp_can_write = true
tcp_can_read = true

# Extra dedicated HTTP/WS servers (optional; primary [api] already multi-client)
# ws_binds = ["127.0.0.1:8790"]
# ws_name_prefix = "ws"
# ws_history_bytes = 65536

# ---- Or declare endpoints one-by-one (merged with [fanout]) ----
# [[clients]]
# type = "pty"
# name = "ui"
# link = "/tmp/ohmyserial-ui"
# can_write = true
# can_read = true
#
# [[clients]]
# type = "tcp"
# name = "script"
# bind = "127.0.0.1:8800"
#
# [[clients]]
# type = "websocket"
# name = "agent"
# history_bytes = 65536

[log]
# file = "logs/session.blog"
mirror_console = true
format = "hex+text"
"#
    .to_string()
}
