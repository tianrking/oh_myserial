//! Session logging (console + optional file).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use chrono::Local;
use parking_lot::Mutex;

use crate::config::LogConfig;

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Rx,
    Tx,
}

impl Direction {
    fn tag(self) -> &'static str {
        match self {
            Direction::Rx => "RX",
            Direction::Tx => "TX",
        }
    }
}

#[derive(Clone)]
pub struct SessionLog {
    inner: Arc<Mutex<SessionLogInner>>,
}

struct SessionLogInner {
    file: Option<File>,
    mirror_console: bool,
    format: LogFormat,
}

#[derive(Debug, Clone, Copy)]
enum LogFormat {
    Text,
    Hex,
    HexText,
}

impl LogFormat {
    fn parse(s: &str) -> Self {
        match s {
            "text" => Self::Text,
            "hex" => Self::Hex,
            _ => Self::HexText,
        }
    }
}

impl SessionLog {
    pub fn from_config(cfg: &LogConfig) -> anyhow::Result<Self> {
        let file = if let Some(path) = &cfg.file {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            Some(OpenOptions::new().create(true).append(true).open(path)?)
        } else {
            None
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(SessionLogInner {
                file,
                mirror_console: cfg.mirror_console,
                format: LogFormat::parse(&cfg.format),
            })),
        })
    }

    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SessionLogInner {
                file: None,
                mirror_console: false,
                format: LogFormat::HexText,
            })),
        }
    }

    pub fn log(&self, dir: Direction, client: Option<&str>, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let who = client.unwrap_or("-");
        let line = match self.inner.lock().format {
            LogFormat::Text => {
                let text = String::from_utf8_lossy(data);
                format!("{ts} {} {who} {}\n", dir.tag(), text.escape_debug())
            }
            LogFormat::Hex => {
                format!(
                    "{ts} {} {who} {}\n",
                    dir.tag(),
                    hex_preview(data, usize::MAX)
                )
            }
            LogFormat::HexText => {
                let text = String::from_utf8_lossy(data);
                format!(
                    "{ts} {} {who} hex={} text={}\n",
                    dir.tag(),
                    hex_preview(data, 64),
                    text.escape_debug()
                )
            }
        };

        let mut g = self.inner.lock();
        if g.mirror_console {
            // Use print to avoid double-formatting via tracing.
            eprint!("{line}");
        }
        if let Some(f) = g.file.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }

    pub fn event(&self, msg: &str) {
        let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let line = format!("{ts} EVENT {msg}\n");
        let mut g = self.inner.lock();
        if g.mirror_console {
            eprint!("{line}");
        }
        if let Some(f) = g.file.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }

    pub fn path_hint(path: &Path) -> String {
        path.display().to_string()
    }
}

fn hex_preview(data: &[u8], max: usize) -> String {
    data.iter()
        .take(max)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
        + if data.len() > max { " ..." } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn writes_file() {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = LogConfig {
            file: Some(tmp.path().to_path_buf()),
            mirror_console: false,
            format: "text".into(),
        };
        let log = SessionLog::from_config(&cfg).unwrap();
        log.log(Direction::Rx, None, b"hello");
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(content.contains("RX"));
        assert!(content.contains("hello"));
    }
}
