//! The proving seam — generic and network-aware.
//!
//! Two real backends implement it: [`crate::native_prover::NativeProver`] (the
//! default — proves on-device) and [`RemoteProver`] (forwards to a remote prover
//! the user configured for the network, with the stored API key). There is no
//! mock backend: if a backend can't produce a real proof (no remote URL set, or
//! no native binary), the prove fails with a clear error rather than returning a
//! fake proof.

use std::sync::Arc;

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

/// Forwards a payload to the remote prover the user configured for the network
/// (with the stored API key). No mock fallback: if no prover URL is set for the
/// network, the prove fails with a clear error instead of returning a fake proof.
pub struct RemoteProver {
    settings: Arc<SettingsStore>,
    http: reqwest::Client,
}

impl RemoteProver {
    pub fn new(settings: Arc<SettingsStore>) -> Self {
        RemoteProver { settings, http: crate::snip36::http_client() }
    }
}

#[async_trait]
impl Prover for RemoteProver {
    async fn prove(&self, req: ProveRequest) -> Result<ProveResult, String> {
        let net = self.settings.for_network(&req.network).await;

        if net.prover_url.is_empty() {
            return Err(format!(
                "no remote prover configured for {} — set a prover URL in Settings, or switch \
                 the prover backend to `native` to prove on-device",
                req.network
            ));
        }

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
        Ok(ProveResult { proof: body })
    }

    fn kind(&self) -> &'static str {
        "remote"
    }

    /// Whether a remote prover is configured for *either* network (sync, no
    /// network call). False ⇒ a `remote` prove will error until a URL is set.
    fn ready(&self) -> bool {
        self.settings
            .try_snapshot()
            .map(|s| !s.mainnet.prover_url.is_empty() || !s.testnet.prover_url.is_empty())
            .unwrap_or(false)
    }
}
