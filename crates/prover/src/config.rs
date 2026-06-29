//! Process-level prover config from env. Per-network proving config (RPC +
//! remote prover URLs/keys) is NOT here — that's user-editable [`crate::settings`],
//! persisted to disk under `data_dir`.
//!
//! Unlike `../dinner`, this crate has no standalone HTTP server (proving is
//! exposed over strkd's loopback JSON-RPC service), so there is no bind address.

pub struct ProverConfig {
    /// Which prover backend to use (`STRKD_PROVER`): `remote` (default; a
    /// configured remote prover, else mock — internally `CompanionProver`) or
    /// `native` (local SNIP-36 CLI). Legacy value `companion` → `remote`.
    pub prover_backend: String,
    /// Simulated proving delay for the mock fallback, ms (`STRKD_MOCK_PROVE_MS`).
    pub mock_prove_ms: u64,
}

impl ProverConfig {
    pub fn from_env() -> Self {
        fn var(k: &str, d: &str) -> String {
            std::env::var(k).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| d.to_string())
        }
        ProverConfig {
            prover_backend: var("STRKD_PROVER", "remote"),
            mock_prove_ms: var("STRKD_MOCK_PROVE_MS", "3000").parse().unwrap_or(3000),
        }
    }
}

impl Default for ProverConfig {
    fn default() -> Self {
        Self::from_env()
    }
}
