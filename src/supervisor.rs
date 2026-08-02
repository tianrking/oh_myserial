//! Multi-profile process supervisor.
//!
//! Each profile still owns exactly one real serial handle and one evidence
//! ledger. The supervisor only validates cross-profile resource collisions and
//! coordinates startup/rollback/shutdown; it never routes bytes between hubs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{ClientConfig, Config};
use crate::hub::{run_hub, HubHandle};

pub struct Supervisor {
    hubs: Vec<HubHandle>,
}

impl Supervisor {
    /// Validate every profile and start them in order. If a later profile
    /// fails, all already-started hubs are shut down before the error returns.
    pub async fn start(mut profiles: Vec<Config>) -> anyhow::Result<Self> {
        for profile in &mut profiles {
            profile.expand_fanout()?;
        }
        validate_profiles(&profiles)?;
        let mut hubs = Vec::with_capacity(profiles.len());
        for profile in profiles {
            match run_hub(profile).await {
                Ok(hub) => hubs.push(hub),
                Err(error) => {
                    while let Some(hub) = hubs.pop() {
                        hub.shutdown_gracefully().await;
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self { hubs })
    }

    pub async fn shutdown_gracefully(mut self) {
        while let Some(hub) = self.hubs.pop() {
            hub.shutdown_gracefully().await;
        }
    }

    pub fn len(&self) -> usize {
        self.hubs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hubs.is_empty()
    }
}

/// Load TOML profiles, expand their generated fan-out clients, and perform
/// cross-profile validation before any listener or serial owner starts.
pub fn load_profiles(paths: &[PathBuf]) -> anyhow::Result<Vec<Config>> {
    if paths.is_empty() {
        anyhow::bail!("supervise requires at least one --config path");
    }
    let mut profiles = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("read profile {}: {error}", path.display()))?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|error| anyhow::anyhow!("parse profile {}: {error}", path.display()))?;
        config
            .expand_fanout()
            .map_err(|error| anyhow::anyhow!("expand profile {}: {error}", path.display()))?;
        profiles.push(config);
    }
    validate_profiles(&profiles)?;
    Ok(profiles)
}

/// Check resources that are process-global rather than hub-local.
pub fn validate_profiles(profiles: &[Config]) -> anyhow::Result<()> {
    if profiles.is_empty() {
        anyhow::bail!("supervise requires at least one profile");
    }
    for profile in profiles {
        profile.validate()?;
    }

    let mut resources = HashMap::<String, String>::new();
    for (index, profile) in profiles.iter().enumerate() {
        let label = format!("profile #{}", index + 1);
        if !profile.real.path.trim().is_empty() && !profile.real.path.starts_with("mock:") {
            claim(
                &mut resources,
                format!("real:{}", normalize_key(&profile.real.path)),
                format!("{label} real.path"),
            )?;
        }
        if let Some(selector) = &profile.real.usb {
            let prefix = format!("usb:{:04x}:{:04x}:", selector.vid, selector.pid);
            let serial = selector.serial_number.as_deref();
            let key = format!("{prefix}{}", normalize_key(serial.unwrap_or("*")));
            if serial.is_none()
                && resources.keys().any(|existing| {
                    existing == &format!("{prefix}*") || existing.starts_with(&prefix)
                })
            {
                anyhow::bail!(
                    "resource collision: {label} real.usb conflicts with another selector"
                );
            }
            if serial.is_some() && resources.contains_key(&format!("{prefix}*")) {
                anyhow::bail!(
                    "resource collision: {label} real.usb conflicts with a wildcard selector"
                );
            }
            claim(&mut resources, key, format!("{label} real.usb"))?;
        }
        if profile.api.enabled {
            claim(
                &mut resources,
                format!("listen:{}", normalize_key(&profile.api.bind)),
                format!("{label} api.bind"),
            )?;
        }
        if profile.rfc2217.enabled {
            claim(
                &mut resources,
                format!("listen:{}", normalize_key(&profile.rfc2217.bind)),
                format!("{label} rfc2217.bind"),
            )?;
        }
        if let Some(directory) = &profile.ledger.directory {
            claim(
                &mut resources,
                format!("ledger:{}", normalize_path(directory)),
                format!("{label} ledger.directory"),
            )?;
        }
        for client in &profile.clients {
            match client {
                ClientConfig::Tcp { bind, .. } => claim(
                    &mut resources,
                    format!("listen:{}", normalize_key(bind)),
                    format!("{label} TCP client '{}'.bind", client.name()),
                )?,
                ClientConfig::Websocket {
                    bind: Some(bind), ..
                } if !profile.api.enabled || bind != &profile.api.bind => {
                    claim(
                        &mut resources,
                        format!("listen:{}", normalize_key(bind)),
                        format!("{label} WebSocket client '{}'.bind", client.name()),
                    )?;
                }
                ClientConfig::Pty { link, .. } => claim(
                    &mut resources,
                    format!("pty:{}", normalize_path(link)),
                    format!("{label} PTY client '{}'.link", client.name()),
                )?,
                _ => {}
            }
        }
    }
    Ok(())
}

fn claim(
    resources: &mut HashMap<String, String>,
    key: String,
    owner: String,
) -> anyhow::Result<()> {
    if let Some(previous) = resources.insert(key, owner.clone()) {
        anyhow::bail!("resource collision: {owner} conflicts with {previous}");
    }
    Ok(())
}

fn normalize_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_path(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    normalize_key(&absolute.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(path: &str, api: u16, tcp: u16) -> Config {
        let mut profile = Config::default();
        profile.real.path = path.into();
        profile.api.bind = format!("127.0.0.1:{api}");
        profile.clients = vec![ClientConfig::Tcp {
            name: "tcp".into(),
            bind: format!("127.0.0.1:{tcp}"),
            can_write: true,
            can_read: true,
        }];
        profile
    }

    #[test]
    fn duplicate_network_bind_is_rejected_before_startup() {
        let a = profile("COM3", 18_787, 18_788);
        let b = profile("COM4", 18_789, 18_788);
        let error = validate_profiles(&[a, b]).unwrap_err().to_string();
        assert!(error.contains("resource collision"));
        assert!(error.contains("TCP"));
    }

    #[test]
    fn duplicate_real_path_is_rejected_but_mock_profiles_are_isolated() {
        let a = profile("COM3", 18_787, 18_788);
        let b = profile("COM3", 18_789, 18_790);
        assert!(validate_profiles(&[a, b]).is_err());

        let a = profile("mock:a", 18_787, 18_788);
        let b = profile("mock:a", 18_789, 18_790);
        validate_profiles(&[a, b]).unwrap();
    }

    #[test]
    fn duplicate_usb_identity_is_rejected() {
        let mut a = profile("", 18_787, 18_788);
        let mut b = profile("", 18_789, 18_790);
        let selector = crate::config::UsbSelector {
            vid: 0x10c4,
            pid: 0xea60,
            serial_number: Some("board-01".into()),
        };
        a.real.usb = Some(selector.clone());
        b.real.usb = Some(selector);
        assert!(validate_profiles(&[a, b]).is_err());
    }
}
