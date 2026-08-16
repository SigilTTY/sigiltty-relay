//! Server config file (docs/PROTOCOL.md §6) — written by the app over SSH,
//! read ONCE at startup. The file is the source of truth for opt-in: any
//! change (targets, devices, renewal) is applied by the app rewriting it
//! and restarting the watcher; there is no self-reload path.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct RelayConfig {
    pub v: u32,
    #[serde(rename = "relayURL")]
    pub relay_url: String,
    #[serde(rename = "serverID")]
    pub server_id: String,
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(rename = "herdrBinary")]
    pub herdr_binary: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    /// The app's digest of what it believes this watcher watches (§6). Opaque
    /// here — never parsed, never acted on, only echoed back by `status` so
    /// the app can ask "is the deployed config still the one I would write?"
    /// without shipping the whole target list back. Defaulted, because
    /// configs written before the field existed must still load.
    #[serde(rename = "targetsFingerprint", default)]
    pub targets_fingerprint: String,
    pub devices: Vec<Device>,
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    #[serde(rename = "routingID")]
    pub routing_id: String,
    pub secret: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[allow(dead_code)]
    pub platform: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    #[serde(rename = "paneID")]
    pub pane_id: String,
    #[serde(rename = "herdrSession")]
    pub herdr_session: Option<String>,
    pub label: Option<String>,
}

/// `$XDG_CONFIG_HOME/sigiltty`, falling back to `~/.config/sigiltty`.
pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("sigiltty");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
        .join(".config")
        .join("sigiltty")
}

pub fn default_config_path() -> PathBuf {
    config_dir().join("relay.json")
}

pub fn load(path: &std::path::Path) -> Result<RelayConfig, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let config: RelayConfig =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if config.v != 1 {
        return Err(format!("unsupported config version {}", config.v));
    }
    Ok(config)
}
