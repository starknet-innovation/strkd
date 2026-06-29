//! The proving seam — generic and network-aware.
//!
//! [`CompanionProver`] proves a payload for a given network. If that network has
//! a remote prover configured (in [`crate::settings`]), it forwards there with
//! the stored API key; otherwise it falls back to a mock proof so the app is
//! always exercisable. The real on-device backend ([`crate::native_prover`])
//! slots in as another arm of this seam — routes/UI/storage are unaffected.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::settings::SettingsStore;

/// A request to prove an opaque payload on a given network.
#[derive(Clone, Debug)]
pub struct ProveRequest {
    pub payload: Value,
    pub label: Option<String>,
    /// `mainnet` or `testnet` (drives which settings/prover are used).
    pub network: String,
}

/// The proof the backend produced. Opaque to the core.
#[derive(Clone, Debug)]
pub struct ProveResult {
    pub proof: Value,
}

/// The single plug-point.
#[async_trait]
pub trait Prover: Send + Sync {
    async fn prove(&self, req: ProveRequest) -> Result<ProveResult, String>;
    fn kind(&self) -> &'static str;
    fn ready(&self) -> bool;
}

/// Proves remotely when configured per-network, else returns a mock proof.
pub struct CompanionProver {
    settings: Arc<SettingsStore>,
    http: reqwest::Client,
    mock_delay_ms: u64,
}

impl CompanionProver {
    pub fn new(settings: Arc<SettingsStore>, mock_delay_ms: u64) -> Self {
        CompanionProver { settings, http: crate::snip36::http_client(), mock_delay_ms }
    }
}

fn fake_hex(prefix: &str, input: &str) -> String {
    let mut h = DefaultHasher::new();
    prefix.hash(&mut h);
    input.hash(&mut h);
    let a = h.finish();
    let mut h2 = DefaultHasher::new();
    a.hash(&mut h2);
    input.hash(&mut h2);
    format!("0x{a:016x}{:016x}", h2.finish())
}

#[async_trait]
impl Prover for CompanionProver {
    async fn prove(&self, req: ProveRequest) -> Result<ProveResult, String> {
        let net = self.settings.for_network(&req.network).await;

        // Remote proving when a prover URL is configured for this network.
        if !net.prover_url.is_empty() {
            let url = format!("{}/v1/prove", net.prover_url.trim_end_matches('/'));
            let resp = self
                .http
                .post(&url)
                .header("x-api-key", &net.prover_api_key)
                .json(&json!({ "payload": req.payload }))
                .send()
                .await
                .map_err(|e| format!("remote prover request failed: {e}"))?;
            let status = resp.status();
            let body: Value = resp
                .json()
                .await
                .map_err(|e| format!("remote prover bad response: {e}"))?;
            if !status.is_success() {
                return Err(format!("remote prover {status}: {body}"));
            }
            return Ok(ProveResult { proof: body });
        }

        // Mock fallback (no remote configured for this network).
        tokio::time::sleep(Duration::from_millis(self.mock_delay_ms)).await;
        Ok(ProveResult {
            proof: json!({
                "mock": true,
                "network": req.network,
                "proof": fake_hex("proof", &req.payload.to_string()),
            }),
        })
    }

    fn kind(&self) -> &'static str {
        "remote"
    }

    fn ready(&self) -> bool {
        true
    }
}
