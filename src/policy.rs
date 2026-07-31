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
    DropNewest,
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
            "drop_newest" => Ok(Self::DropNewest),
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
    pub write_timeout_ms: u64,
    pub max_frame_bytes: usize,
    pub max_write_bytes: usize,
    pub frame_delim: u8,
    pub slow_client: SlowClientPolicy,
    pub client_queue: usize,
    pub slow_block_ms: u64,
}

impl Policy {
    pub fn from_config(cfg: &TxConfig) -> anyhow::Result<Self> {
        Ok(Self {
            mode: TxMode::parse(&cfg.mode)?,
            primary: cfg.primary.clone(),
            write_lock_ms: cfg.write_lock_ms,
            write_timeout_ms: cfg.write_timeout_ms,
            max_frame_bytes: cfg.max_frame_bytes,
            max_write_bytes: cfg.max_write_bytes,
            frame_delim: cfg.frame_delim,
            slow_client: SlowClientPolicy::parse(&cfg.slow_client)?,
            client_queue: cfg.client_queue.max(1),
            slow_block_ms: cfg.slow_block_ms,
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
    pub fn push(
        &mut self,
        data: &[u8],
        delim: u8,
        max_frame_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, String> {
        let mut out = Vec::new();
        for &byte in data {
            if self.buf.len() >= max_frame_bytes {
                self.buf.clear();
                return Err(format!(
                    "frame exceeds tx.max_frame_bytes ({max_frame_bytes}); partial frame was discarded"
                ));
            }
            self.buf.push(byte);
            if byte == delim {
                out.push(std::mem::take(&mut self.buf));
            }
        }
        Ok(out)
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
    /// Random bearer credential required for every operation under this lease.
    pub token: String,
    pub expires_at: Instant,
}

impl WriteLock {
    pub fn active(&self, now: Instant) -> bool {
        now < self.expires_at
    }

    pub fn authorizes(&self, token: Option<&str>, now: Instant) -> bool {
        self.active(now) && token.is_some_and(|candidate| candidate == self.token)
    }
}

/// Decide whether `client` may emit a framed unit right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitDecision {
    Allow,
    Deny { reason: String },
}

/// All facts needed for one deterministic TX admission decision.
pub struct AdmitContext<'a> {
    pub mode: TxMode,
    pub client: &'a str,
    pub primary: Option<&'a str>,
    /// Granted by the server-side endpoint registration, never by a caller's
    /// self-reported display label.
    pub client_is_primary: bool,
    /// Whether a writable client whose name exactly matches `primary` is connected.
    pub primary_connected: bool,
    pub lock: Option<&'a WriteLock>,
    /// Bearer token supplied with this write, if any.
    pub lease_token: Option<&'a str>,
    pub now: Instant,
}

pub fn admit_write(ctx: AdmitContext<'_>) -> AdmitDecision {
    // An active lease overrides the configured mode, but only possession of its
    // random token grants access. Display names are deliberately not credentials.
    if let Some(lock) = ctx.lock {
        if lock.active(ctx.now) {
            if lock.authorizes(ctx.lease_token, ctx.now) {
                return AdmitDecision::Allow;
            }
            return AdmitDecision::Deny {
                reason: format!(
                    "write lease held by '{}' for another {}ms; a valid lease token is required",
                    lock.owner,
                    lock.expires_at
                        .saturating_duration_since(ctx.now)
                        .as_millis()
                ),
            };
        }
    }

    match ctx.mode {
        TxMode::QueueByLine | TxMode::QueueByFrame => AdmitDecision::Allow,
        TxMode::Exclusive => AdmitDecision::Deny {
            reason: "exclusive mode: acquire a write lease first (POST /v1/lock)".into(),
        },
        TxMode::PrimaryWins => match ctx.primary {
            Some(_) if ctx.client_is_primary => AdmitDecision::Allow,
            Some(primary) if ctx.primary_connected => AdmitDecision::Deny {
                reason: format!("primary_wins: writable primary client '{primary}' is connected"),
            },
            _ => AdmitDecision::Allow,
        },
    }
}

#[cfg(test)]
fn context<'a>(
    mode: TxMode,
    client: &'a str,
    primary: Option<&'a str>,
    primary_connected: bool,
    lock: Option<&'a WriteLock>,
    lease_token: Option<&'a str>,
    now: Instant,
) -> AdmitContext<'a> {
    AdmitContext {
        mode,
        client,
        primary,
        client_is_primary: primary.is_some_and(|name| name == client),
        primary_connected,
        lock,
        lease_token,
        now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(owner: &str, token: &str, now: Instant) -> WriteLock {
        WriteLock {
            owner: owner.into(),
            token: token.into(),
            expires_at: now + Duration::from_secs(1),
        }
    }

    #[test]
    fn assembler_splits_lines() {
        let mut a = FrameAssembler::default();
        let frames = a.push(b"hi\nthere", b'\n', 64).unwrap();
        assert_eq!(frames, vec![b"hi\n".to_vec()]);
        let frames = a.push(b"\nmore\n", b'\n', 64).unwrap();
        assert_eq!(frames, vec![b"there\n".to_vec(), b"more\n".to_vec()]);
    }

    #[test]
    fn assembler_rejects_and_clears_oversized_partial_frame() {
        let mut a = FrameAssembler::default();
        assert!(a.push(b"1234", b'\n', 4).is_ok());
        assert!(a
            .push(b"5", b'\n', 4)
            .unwrap_err()
            .contains("max_frame_bytes"));
        assert_eq!(a.push(b"ok\n", b'\n', 4).unwrap(), vec![b"ok\n".to_vec()]);
    }

    #[test]
    fn exclusive_requires_valid_lease_token() {
        let now = Instant::now();
        let d = admit_write(context(
            TxMode::Exclusive,
            "agent",
            None,
            false,
            None,
            None,
            now,
        ));
        assert!(matches!(d, AdmitDecision::Deny { .. }));

        let lock = lease("agent", "secret", now);
        let d = admit_write(context(
            TxMode::Exclusive,
            "agent",
            None,
            false,
            Some(&lock),
            Some("secret"),
            now,
        ));
        assert_eq!(d, AdmitDecision::Allow);
    }

    #[test]
    fn matching_owner_name_is_not_a_credential() {
        let now = Instant::now();
        let lock = lease("ui", "secret", now);
        for token in [None, Some("wrong")] {
            let d = admit_write(context(
                TxMode::QueueByLine,
                "ui",
                None,
                false,
                Some(&lock),
                token,
                now,
            ));
            assert!(matches!(d, AdmitDecision::Deny { .. }));
        }
    }

    #[test]
    fn expired_lease_does_not_block_normal_mode() {
        let now = Instant::now();
        let lock = WriteLock {
            owner: "ui".into(),
            token: "secret".into(),
            expires_at: now,
        };
        let d = admit_write(context(
            TxMode::QueueByLine,
            "agent",
            None,
            false,
            Some(&lock),
            None,
            now,
        ));
        assert_eq!(d, AdmitDecision::Allow);
    }

    #[test]
    fn primary_wins_only_while_primary_is_connected() {
        let now = Instant::now();
        let denied = admit_write(context(
            TxMode::PrimaryWins,
            "agent",
            Some("ui"),
            true,
            None,
            None,
            now,
        ));
        assert!(matches!(denied, AdmitDecision::Deny { .. }));

        let fallback = admit_write(context(
            TxMode::PrimaryWins,
            "agent",
            Some("ui"),
            false,
            None,
            None,
            now,
        ));
        assert_eq!(fallback, AdmitDecision::Allow);

        let primary = admit_write(context(
            TxMode::PrimaryWins,
            "ui",
            Some("ui"),
            true,
            None,
            None,
            now,
        ));
        assert_eq!(primary, AdmitDecision::Allow);
    }

    #[test]
    fn lease_token_overrides_primary_reservation() {
        let now = Instant::now();
        let lock = lease("automation", "secret", now);
        let d = admit_write(context(
            TxMode::PrimaryWins,
            "agent",
            Some("ui"),
            true,
            Some(&lock),
            Some("secret"),
            now,
        ));
        assert_eq!(d, AdmitDecision::Allow);
    }

    #[test]
    fn caller_label_cannot_claim_primary_capability() {
        let decision = admit_write(AdmitContext {
            mode: TxMode::PrimaryWins,
            client: "ui",
            primary: Some("ui"),
            client_is_primary: false,
            primary_connected: true,
            lock: None,
            lease_token: None,
            now: Instant::now(),
        });
        assert!(matches!(decision, AdmitDecision::Deny { .. }));
    }

    #[test]
    fn parses_all_honest_slow_client_policies() {
        assert_eq!(
            SlowClientPolicy::parse("drop_oldest").unwrap(),
            SlowClientPolicy::DropOldest
        );
        assert_eq!(
            SlowClientPolicy::parse("drop_newest").unwrap(),
            SlowClientPolicy::DropNewest
        );
        assert_eq!(
            SlowClientPolicy::parse("block").unwrap(),
            SlowClientPolicy::Block
        );
    }
}
