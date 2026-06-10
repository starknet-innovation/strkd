//! App settings persisted to `config.json` (spec §10).
//!
//! Currently just the per-network Starknet RPC URLs that power the node client
//! (broadcast + fee estimation). These are managed only in the desktop app
//! (the "control panel"), are IPC-only, and are never exposed over the wallet's
//! HTTP service — an RPC URL may embed an API key.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wallet_core::ChainId;
use wallet_rpc::{HttpStarknetRpc, StarknetRpc};

fn default_auto_lock_minutes() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub sepolia_rpc: String,
    #[serde(default)]
    pub mainnet_rpc: String,
    /// Auto-lock the wallet after this many minutes of inactivity. 0 = never.
    #[serde(default = "default_auto_lock_minutes")]
    pub auto_lock_minutes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sepolia_rpc: String::new(),
            mainnet_rpc: String::new(),
            auto_lock_minutes: default_auto_lock_minutes(),
        }
    }
}

impl Settings {
    /// The RPC URL for a given network ("" if unset).
    pub fn rpc_for(&self, chain: ChainId) -> &str {
        match chain {
            ChainId::Sepolia => &self.sepolia_rpc,
            ChainId::Mainnet => &self.mainnet_rpc,
        }
    }

    /// Build a node client for `chain`, or `None` if no URL is configured.
    pub fn node_for(&self, chain: ChainId) -> Option<Arc<dyn StarknetRpc>> {
        let url = self.rpc_for(chain).trim();
        if url.is_empty() {
            None
        } else {
            Some(Arc::new(HttpStarknetRpc::new(url.to_string())))
        }
    }

    /// Light validation: URLs must be http(s) or empty (empty = disabled).
    pub fn validate(&self) -> Result<(), String> {
        for (name, url) in [("sepolia_rpc", &self.sepolia_rpc), ("mainnet_rpc", &self.mainnet_rpc)] {
            let u = url.trim();
            if !(u.is_empty() || u.starts_with("http://") || u.starts_with("https://")) {
                return Err(format!("{name} must be an http(s) URL or empty"));
            }
        }
        Ok(())
    }
}

/// Load settings from `config.json` (defaults if missing/unreadable).
pub fn load(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

/// Persist settings to `config.json` (pretty JSON, `0600` on unix).
pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, json.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
