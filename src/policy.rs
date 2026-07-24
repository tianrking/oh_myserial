//! TX admission and framing policy.

use std::time::{Duration, Instant};

use crate::config::TxConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMode {
    QueueByLine,
    QueueByFrame,
    Exclusive,
    PrimaryWins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowClientPolicy {
    DropOldest,
    DisconnectSlow,
    Block,
}

impl TxMode {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "queue_by_line" => Ok(Self::QueueByLine),
            "queue_by_frame" => Ok(Self::QueueByFrame),
            "exclusive" => Ok(Self::Exclusive),
            "primary_wins" => Ok(Self::PrimaryWins),
            other => anyhow::bail!("unknown tx mode: {other}"),
        }
    }
}

impl SlowClientPolicy {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "drop_oldest" => Ok(Self::DropOldest),
            "disconnect_slow" => Ok(Self::DisconnectSlow),
            "block" => Ok(Self::Block),
            other => anyhow::bail!("unknown slow_client policy: {other}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub mode: TxMode,
    pub primary: Option<String>,
    pub write_lock_ms: u64,
    pub frame_delim: u8,
    pub slow_client: SlowClientPolicy,
    pub client_queue: usize,
}

impl Policy {
    pub fn from_config(cfg: &TxConfig) -> anyhow::Result<Self> {
        Ok(Self {
            mode: TxMode::parse(&cfg.mode)?,
            primary: cfg.primary.clone(),
            write_lock_ms: cfg.write_lock_ms,
            frame_delim: cfg.frame_delim,
            slow_client: SlowClientPolicy::parse(&cfg.slow_client)?,
            client_queue: cfg.client_queue.max(1),
        })
    }

    pub fn lock_ttl(&self) -> Duration {
        Duration::from_millis(self.write_lock_ms.max(1))
    }
}

/// Partial-line / partial-frame assembler for a single client writer.
#[derive(Debug, Default)]
pub struct FrameAssembler {
    buf: Vec<u8>,
}

impl FrameAssembler {
    pub fn push(&mut self, data: &[u8], delim: u8) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == delim) {
            let mut frame = self.buf.drain(..=pos).collect::<Vec<_>>();
            // keep delimiter in frame
            out.push(frame.split_off(0));
        }
        out
    }

    /// Flush remaining bytes (used on disconnect or exclusive raw mode).
    pub fn take_remainder(&mut self) -> Option<Vec<u8>> {
        if self.buf.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf))
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteLock {
    pub owner: String,
    pub expires_at: Instant,
}

impl WriteLock {
    pub fn active(&self, now: Instant) -> bool {
        now < self.expires_at
    }
}

/// Decide whether `client` may emit a framed unit right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitDecision {
    Allow,
    Deny { reason: String },
    /// Allowed but another client is primary; still allow if no exclusive lock conflict.
    AllowPrimaryPrefer,
}

pub fn admit_write(
    mode: TxMode,
    client: &str,
    primary: Option<&str>,
    lock: Option<&WriteLock>,
    now: Instant,
) -> AdmitDecision {
    // Active write lock always wins: only owner may write.
    if let Some(lock) = lock {
        if lock.active(now) && lock.owner != client {
            return AdmitDecision::Deny {
                reason: format!(
                    "write lock held by '{}' until {}ms",
                    lock.owner,
                    lock.expires_at
                        .saturating_duration_since(now)
                        .as_millis()
                ),
            };
        }
    }

    match mode {
        TxMode::QueueByLine | TxMode::QueueByFrame => AdmitDecision::Allow,
        TxMode::Exclusive => {
            // Without a lock, exclusive mode requires holding the lock.
            match lock {
                Some(l) if l.active(now) && l.owner == client => AdmitDecision::Allow,
                Some(l) if l.active(now) => AdmitDecision::Deny {
                    reason: format!("exclusive mode: lock owned by '{}'", l.owner),
                },
                _ => AdmitDecision::Deny {
                    reason: "exclusive mode: acquire write lock first (POST /v1/lock)".into(),
                },
            }
        }
        TxMode::PrimaryWins => {
            if primary.is_some_and(|p| p == client) {
                AdmitDecision::AllowPrimaryPrefer
            } else {
                AdmitDecision::Allow
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembler_splits_lines() {
        let mut a = FrameAssembler::default();
        let frames = a.push(b"hi\nthere", b'\n');
        assert_eq!(frames, vec![b"hi\n".to_vec()]);
        let frames = a.push(b"\nmore\n", b'\n');
        assert_eq!(frames, vec![b"there\n".to_vec(), b"more\n".to_vec()]);
    }

    #[test]
    fn exclusive_requires_lock() {
        let now = Instant::now();
        let d = admit_write(TxMode::Exclusive, "agent", None, None, now);
        assert!(matches!(d, AdmitDecision::Deny { .. }));

        let lock = WriteLock {
            owner: "agent".into(),
            expires_at: now + Duration::from_secs(1),
        };
        let d = admit_write(TxMode::Exclusive, "agent", None, Some(&lock), now);
        assert_eq!(d, AdmitDecision::Allow);
    }

    #[test]
    fn lock_blocks_others() {
        let now = Instant::now();
        let lock = WriteLock {
            owner: "ui".into(),
            expires_at: now + Duration::from_secs(1),
        };
        let d = admit_write(TxMode::QueueByLine, "agent", None, Some(&lock), now);
        assert!(matches!(d, AdmitDecision::Deny { .. }));
    }
}
