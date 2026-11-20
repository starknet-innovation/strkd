//! Process-level prover config from env. Per-network proving config (RPC +
//! remote prover URLs/keys) is NOT here — that's user-editable [`crate::settings`],
//! persisted to disk under `data_dir`.
//!
//! Unlike `../dinner`, this crate has no standalone HTTP server (proving is
//! exposed over strkd's loopback JSON-RPC service), so there is no bind address.

/// Prefixes an environment variable may carry, current name first. Entries after
/// the first are legacy names that are still read, so a rename does not break an
/// existing setup.
const ENV_PREFIXES: &[&str] = &["STRKD_"];

/// The current name of the environment variable `suffix` (for messages).
pub fn env_name(suffix: &str) -> String {
    format!("{}{suffix}", ENV_PREFIXES[0])
}

/// Read environment variable `suffix` under each prefix in [`ENV_PREFIXES`], in
/// order. Unset and empty values are skipped.
pub fn env_var(suffix: &str) -> Option<String> {
    env_var_with(suffix, |k| std::env::var(k).ok())
}

fn env_var_with(suffix: &str, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ENV_PREFIXES.iter().find_map(|p| get(&format!("{p}{suffix}")).filter(|v| !v.is_empty()))
}

pub struct ProverConfig {
    /// Which prover backend to use (`STRKD_PROVER`): `native` (default — proves
    /// on-device via the bundled SNIP-36 CLI) or `remote` (forward to a remote
    /// prover the user configured per-network). Legacy value `companion` →
    /// `remote`. There is no mock backend.
    pub prover_backend: String,
}

impl ProverConfig {
    pub fn from_env() -> Self {
        ProverConfig { prover_backend: env_var("PROVER").unwrap_or_else(|| "native".into()) }
    }
}

impl Default for ProverConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_reads_prefixes_in_order_and_skips_empty() {
        let first = env_name("X");
        assert_eq!(env_var_with("X", |k| (k == first).then(|| "a".to_string())), Some("a".into()));
        assert_eq!(env_var_with("X", |_| Some(String::new())), None);
        assert_eq!(env_var_with("X", |_| None), None);
    }
}
