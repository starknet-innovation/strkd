//! User-editable, persisted prover settings: per-network RPC + remote-prover config.
//!
//! These hold secrets (prover API keys), so they are managed only over trusted
//! channels (the desktop app's IPC), never returned over the loopback JSON-RPC
//! service. Persisted as JSON under `data_dir/settings.json`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Proving config for one network.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default)]
    pub rpc_url: String,
    #[serde(default)]
    pub prover_url: String,
    #[serde(default)]
    pub prover_api_key: String,
}

/// All networks the prover knows about, plus the chosen prover backend.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub mainnet: NetworkConfig,
    #[serde(default)]
    pub testnet: NetworkConfig,
    /// Prover backend: `""` (use the `STRKD_PROVER` env / default), `native`, or
    /// `remote` (legacy: `companion`). Applied at startup, so changes take
    /// effect on restart.
    #[serde(default)]
    pub prover_backend: String,
}

pub struct SettingsStore {
    path: PathBuf,
    inner: RwLock<Settings>,
    /// mtime of `path` as last read — to reload on external edits.
    mtime: Mutex<Option<SystemTime>>,
}

fn file_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl SettingsStore {
    /// Load from `path` (missing/corrupt → defaults).
    pub fn load(path: PathBuf) -> Self {
        let settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mtime = file_mtime(&path);
        SettingsStore { path, inner: RwLock::new(settings), mtime: Mutex::new(mtime) }
    }

    /// Reload from disk if the file changed since we last read it — so external
    /// edits AND the desktop IPC write both take effect without a restart. (The
    /// `prover_backend` field is still only applied at startup; the live-read
    /// RPC/prover fields are what hot-reload here.)
    async fn reload_if_changed(&self) {
        let cur = file_mtime(&self.path);
        let changed = cur.is_some() && *self.mtime.lock().unwrap() != cur;
        if changed {
            if let Some(s) = std::fs::read_to_string(&self.path)
                .ok()
                .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            {
                *self.inner.write().await = s;
                *self.mtime.lock().unwrap() = cur;
            }
        }
    }

    pub async fn get(&self) -> Settings {
        self.reload_if_changed().await;
        self.inner.read().await.clone()
    }

    /// Backend choice read synchronously at startup (uncontended). Empty if unset.
    pub fn prover_backend_blocking(&self) -> String {
        self.inner.try_read().map(|s| s.prover_backend.clone()).unwrap_or_default()
    }

    /// Best-effort synchronous snapshot of the settings (uncontended). `None` if
    /// the lock is momentarily held — used by readiness checks that can't await.
    pub fn try_snapshot(&self) -> Option<Settings> {
        self.inner.try_read().ok().map(|s| s.clone())
    }

    /// Config for a network name (`mainnet`/`testnet`; `sepolia` aliases testnet).
    pub async fn for_network(&self, net: &str) -> NetworkConfig {
        self.reload_if_changed().await;
        let s = self.inner.read().await;
        match net {
            "testnet" | "sepolia" => s.testnet.clone(),
            _ => s.mainnet.clone(),
        }
    }

    /// Replace + persist.
    pub async fn update(&self, new: Settings) -> std::io::Result<()> {
        {
            let mut g = self.inner.write().await;
            *g = new;
        }
        self.save().await
    }

    async fn save(&self) -> std::io::Result<()> {
        let s = self.inner.read().await.clone();
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&self.path, serde_json::to_string_pretty(&s).unwrap_or_default())?;
        // Record our own write's mtime so reload_if_changed doesn't re-read it.
        *self.mtime.lock().unwrap() = file_mtime(&self.path);
        Ok(())
    }
}
