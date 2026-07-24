//! oh_myserial — cross-platform serial hub for humans and agents.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use ohmyserial::config::{Config, QuickShare};
use ohmyserial::{hub, serial};

#[derive(Parser, Debug)]
#[command(
    name = "ohmyserial",
    version,
    about = "Share one serial port with many apps and AI agents",
    long_about = "ohmyserial opens a real UART once, then fans out to virtual serial ports (Unix), \
TCP streams, and WebSocket/HTTP for agents.\n\n\
Quick start (no config file):\n  \
  ohmyserial share /dev/cu.usbmodem14101 --pty 3\n  \
  ohmyserial share COM3 --tcp 2\n  \
  ohmyserial share mock:demo"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Easiest: share a serial port with multi PTY / TCP / WebSocket (no TOML needed).
    Share {
        /// Real device path, e.g. /dev/cu.usbmodem*, /dev/ttyUSB0, COM3, or mock:demo
        device: String,
        /// Baud rate
        #[arg(short, long, default_value_t = 115_200)]
        baud: u32,
        /// Number of virtual serial ports (macOS/Linux PTY). Default: 2 on Unix, 0 on Windows.
        #[arg(long, default_value_t = default_pty_cli())]
        pty: u32,
        /// Number of TCP fan-out ports (each accepts many clients). Default: 1
        #[arg(long, default_value_t = 1)]
        tcp: u32,
        /// First TCP port (then +1, +2, …)
        #[arg(long, default_value_t = 8788)]
        tcp_base: u16,
        /// HTTP + WebSocket API bind
        #[arg(long, default_value = "127.0.0.1:8787")]
        api: String,
        /// Quiet session log mirror on console
        #[arg(long, default_value_t = false)]
        quiet: bool,
        /// Open the embedded web console in the default browser
        #[arg(long, default_value_t = false)]
        ui: bool,
    },
    /// Run from a TOML config file (advanced).
    Run {
        /// Path to config TOML.
        #[arg(short, long, default_value = "ohmyserial.toml")]
        config: PathBuf,
        /// Optional: override real.port path without editing the file
        #[arg(short = 'd', long)]
        device: Option<String>,
        /// Optional: override baud
        #[arg(short, long)]
        baud: Option<u32>,
        /// Optional: override fanout.pty_count
        #[arg(long)]
        pty: Option<u32>,
        /// Optional: override fanout.tcp_count
        #[arg(long)]
        tcp: Option<u32>,
        /// Open the embedded web console in the default browser
        #[arg(long, default_value_t = false)]
        ui: bool,
    },
    /// Write a friendly sample config (platform-aware defaults).
    Init {
        /// Write to file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// List serial ports visible on this machine.
    ListPorts,
    /// Print status / endpoints of a running hub.
    Status {
        #[arg(long, default_value = "http://127.0.0.1:8787")]
        api: String,
        /// Show only fan-out endpoints
        #[arg(long, default_value_t = false)]
        endpoints: bool,
    },
}

fn default_pty_cli() -> u32 {
    #[cfg(unix)]
    {
        2
    }
    #[cfg(not(unix))]
    {
        0
    }
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
        Commands::Share {
            device,
            baud,
            pty,
            tcp,
            tcp_base,
            api,
            quiet,
            ui,
        } => {
            eprintln!("ohmyserial share");
            eprintln!("  device = {device}");
            eprintln!("  baud   = {baud}");
            eprintln!("  pty    = {pty} virtual serial(s)  (Unix PTY)");
            eprintln!("  tcp    = {tcp} port(s) from {tcp_base}");
            eprintln!("  api/ws = {api}");
            eprintln!("  ui     = http://{api}/");
            eprintln!();

            let cfg = Config::from_quick(QuickShare {
                device,
                baud,
                pty_count: pty,
                tcp_count: tcp,
                tcp_base_port: tcp_base,
                api_bind: api.clone(),
                mirror_console: !quiet,
            })?;
            run_until_ctrl_c(cfg, ui, Some(api)).await
        }
        Commands::Run {
            config,
            device,
            baud,
            pty,
            tcp,
            ui,
        } => {
            // Reload from disk so fanout overrides re-expand cleanly.
            let text = std::fs::read_to_string(&config)?;
            let mut cfg: Config = toml::from_str(&text)?;
            if let Some(d) = device {
                cfg.real.path = d;
            }
            if let Some(b) = baud {
                cfg.real.baud = b;
            }
            if let Some(p) = pty {
                cfg.fanout.pty_count = p;
            }
            if let Some(t) = tcp {
                cfg.fanout.tcp_count = t;
            }
            cfg.expand_fanout()?;
            cfg.validate()?;
            let api_bind = cfg.api.bind.clone();
            tracing::info!("loaded config {}", config.display());
            run_until_ctrl_c(cfg, ui, Some(api_bind)).await
        }
        Commands::Init { output } => {
            let sample = sample_config();
            if let Some(path) = output {
                std::fs::write(&path, &sample)?;
                eprintln!("wrote {}", path.display());
                eprintln!();
                eprintln!("Next:");
                eprintln!("  1) edit real.path  (or use: ohmyserial share <device> --pty 3)");
                eprintln!("  2) ohmyserial run -c {}", path.display());
            } else {
                print!("{sample}");
            }
            Ok(())
        }
        Commands::ListPorts => {
            let ports = serial::list_ports()?;
            if ports.is_empty() {
                eprintln!("(no serial ports found)");
            } else {
                eprintln!("Available serial ports:\n");
                for p in &ports {
                    println!("  {p}");
                }
                eprintln!();
                eprintln!("Share one with:");
                eprintln!("  ohmyserial share <DEVICE> --pty 3 --tcp 1");
                eprintln!("Example:");
                if let Some(first) = ports.first() {
                    // port lines look like: "/dev/cu.xxx (UsbPort)" — take first token
                    let dev = first.split_whitespace().next().unwrap_or(first);
                    eprintln!("  ohmyserial share {dev} --baud 115200");
                }
            }
            Ok(())
        }
        Commands::Status { api, endpoints } => {
            let path = if endpoints { "/v1/endpoints" } else { "/v1/status" };
            let url = format!("{}{}", api.trim_end_matches('/'), path);
            let body = simple_http_get(&url).await?;
            println!("{body}");
            Ok(())
        }
    }
}

async fn run_until_ctrl_c(
    cfg: Config,
    open_ui: bool,
    api_bind: Option<String>,
) -> anyhow::Result<()> {
    let ui_url = api_bind
        .as_ref()
        .map(|b| format!("http://{b}/"))
        .unwrap_or_else(|| "http://127.0.0.1:8787/".into());

    let handle = hub::run_hub(cfg).await?;

    if ohmyserial::client::ui_embedded() {
        eprintln!("Web UI: {ui_url}");
        if open_ui {
            match opener::open(&ui_url) {
                Ok(()) => eprintln!("opened browser"),
                Err(e) => eprintln!("could not open browser: {e} (open {ui_url} manually)"),
            }
        } else {
            eprintln!("tip: re-run with --ui to open the console automatically");
        }
    } else {
        eprintln!("Web UI not embedded (build web/dist then rebuild ohmyserial)");
    }

    tracing::info!("press Ctrl+C to stop");
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    handle.shutdown();
    Ok(())
}

async fn simple_http_get(url: &str) -> anyhow::Result<String> {
    let url = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow::anyhow!("only http:// supported"))?;
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
    let pty_default = default_pty_cli();
    let pty_hint = if cfg!(unix) {
        "# macOS/Linux: 2 virtual serials ready for two serial apps"
    } else {
        "# Windows: PTY not available — use TCP + WebSocket (set pty_count = 0)"
    };
    format!(
        r#"# oh_myserial — friendly sample
# Tip: you often don't need this file:
#   ohmyserial share /dev/cu.usbmodemXXXX --pty 3
#   ohmyserial share COM3 --tcp 2

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
mode = "queue_by_line"
primary = "ui"
write_lock_ms = 3000
slow_client = "drop_oldest"

[api]
bind = "127.0.0.1:8787"
enabled = true

[fanout]
{pty_hint}
pty_count = {pty_default}
pty_link_prefix = "/tmp/ohmyserial-v"
pty_name_prefix = "v"
pty_can_write = true
pty_can_read = true

# TCP: one port is enough for many concurrent programs
tcp_count = 1
tcp_host = "127.0.0.1"
tcp_base_port = 8788
tcp_name_prefix = "tcp"
tcp_can_write = true
tcp_can_read = true

[log]
mirror_console = true
format = "hex+text"
"#
    )
}
