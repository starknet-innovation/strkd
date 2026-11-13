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
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default)]
    pub rpc_url: String,
    #[serde(default)]
    pub prover_url: String,
    #[serde(default)]
    pub prover_api_key: String,
    /// STRK20 (privacy pool) settings for this network.
    #[serde(default)]
    pub strk20: Strk20Config,
}

/// Per-network STRK20 settings. Empty strings mean "use the default". The API
/// keys are secrets: like `prover_api_key`, they are only managed over the
/// desktop IPC and never returned over the loopback JSON-RPC service.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Strk20Config {
    /// Pool contract address. Empty = the network's canonical pool.
    #[serde(default)]
    pub pool: String,
    /// Starkscan API key with `prove` scope: deposits are proved (and the
    /// depositor screened) through Starkscan's STRK20 prover relay. Mainnet only.
    #[serde(default)]
    pub starkscan_api_key: String,
    /// A `starknet_proveTransaction` JSON-RPC endpoint that screens deposits,
    /// overriding Starkscan and the bramble gateway (e.g. an operator's Sepolia
    /// prover).
    #[serde(default)]
    pub deposit_prover_url: String,
    /// AVNU paymaster API key: transfers and withdrawals are relayed privately
    /// through AVNU's `sponsored_private` mode. Without it, strkd only prepares
    /// them (`wallet_strk20PrepareInvoke`) for submission from another account.
    #[serde(default)]
    pub avnu_api_key: String,
    /// Paymaster endpoint. Empty = AVNU's endpoint for the network.
    #[serde(default)]
    pub paymaster_url: String,
    /// **Testnet only.** The screener private key of a privacy pool you
    /// deployed yourself (the pool's constructor takes its screener public
    /// key). With it, deposits are proved on this machine and strkd signs the
    /// screening attestation, instead of asking a screening prover. A secret:
    /// IPC-only like the API keys, never logged, never returned over the
    /// loopback service, and masked when read back by the desktop UI. strkd
    /// refuses it on mainnet (settings validation and at deposit time) and
    /// checks it against the pool's `get_screener_public_key` before use.
    #[serde(default)]
    pub screener_key: String,
}

fn redacted(s: &str) -> &'static str {
    if s.is_empty() {
        ""
    } else {
        "<redacted>"
    }
}

// Hand-written so a stray `{:?}` never prints a secret.
impl std::fmt::Debug for Strk20Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Strk20Config")
            .field("pool", &self.pool)
            .field("starkscan_api_key", &redacted(&self.starkscan_api_key))
            .field("deposit_prover_url", &self.deposit_prover_url)
            .field("avnu_api_key", &redacted(&self.avnu_api_key))
            .field("paymaster_url", &self.paymaster_url)
            .field("screener_key", &redacted(&self.screener_key))
            .finish()
    }
}

impl std::fmt::Debug for NetworkConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkConfig")
            .field("rpc_url", &self.rpc_url)
            .field("prover_url", &self.prover_url)
            .field("prover_api_key", &redacted(&self.prover_api_key))
            .field("strk20", &self.strk20)
            .finish()
    }
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
        // API keys and a testnet screener key live here: owner-only, like config.json.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        // Record our own write's mtime so reload_if_changed doesn't re-read it.
        *self.mtime.lock().unwrap() = file_mtime(&self.path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_secrets() {
        let cfg = NetworkConfig {
            prover_api_key: "prover-secret".into(),
            strk20: Strk20Config {
                pool: "0x123".into(),
                starkscan_api_key: "starkscan-secret".into(),
                avnu_api_key: "avnu-secret".into(),
                screener_key: "0xcafebabe".into(),
                ..Strk20Config::default()
            },
            ..NetworkConfig::default()
        };
        let out = format!("{cfg:?}");
        for secret in ["prover-secret", "starkscan-secret", "avnu-secret", "cafebabe"] {
            assert!(!out.contains(secret), "{out}");
        }
        assert!(out.contains("0x123") && out.contains("<redacted>"), "{out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn settings_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("strkd-settings-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SettingsStore::load(dir.join("settings.json"));
        store.update(Settings::default()).await.unwrap();
        let mode = std::fs::metadata(dir.join("settings.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
