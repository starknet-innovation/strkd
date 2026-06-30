//! Process-level prover config from env. Per-network proving config (RPC +
//! remote prover URLs/keys) is NOT here — that's user-editable [`crate::settings`],
//! persisted to disk under `data_dir`.
//!
//! Unlike `../dinner`, this crate has no standalone HTTP server (proving is
//! exposed over strkd's loopback JSON-RPC service), so there is no bind address.

pub struct ProverConfig {
    /// Which prover backend to use (`STRKD_PROVER`): `native` (default — proves
    /// on-device via the bundled SNIP-36 CLI) or `remote` (forward to a remote
    /// prover the user configured per-network). Legacy value `companion` →
    /// `remote`. There is no mock backend.
    pub prover_backend: String,
}

impl ProverConfig {
    pub fn from_env() -> Self {
        fn var(k: &str, d: &str) -> String {
            std::env::var(k).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| d.to_string())
        }
        ProverConfig { prover_backend: var("STRKD_PROVER", "native") }
    }
}

impl Default for ProverConfig {
    fn default() -> Self {
        Self::from_env()
    }
}
