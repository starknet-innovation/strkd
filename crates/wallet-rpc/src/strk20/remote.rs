//! Remote proving for STRK20 **deposits**.
//!
//! The pool accepts a deposit only with a screening attestation signed by its
//! screener, and only the operator's prover issues one (it screens the
//! depositor while proving). So a deposit — unlike register, transfer and
//! withdraw — cannot be proved on this machine. The proof invocation, which
//! carries `user_sk` in its calldata, is sent to one of (first match wins):
//!
//! 1. the user's own `deposit_prover_url` (a `starknet_proveTransaction`
//!    JSON-RPC endpoint, e.g. an operator's Sepolia prover);
//! 2. Starkscan's STRK20 prover relay, with the user's API key (mainnet only;
//!    <https://starkscan.co/docs/api/strk20-prover>);
//! 3. bramble's gateway (mainnet only).
//!
//! That disclosure is inherent to the protocol: the approval prompt says so.

use std::time::Duration;

use serde_json::{json, Value};
use wallet_core::{ChainId, Felt};

use prover::Strk20Config;

/// Bramble's wallet gateway: a `starknet_proveTransaction` proxy (mainnet).
pub const BRAMBLE_PROVER_URL: &str = "https://wallet.nodes.starknet.org/api/strk20/prover";
/// Starkscan's API host; the STRK20 relay lives under `/v1/<chain>/prove`.
pub const STARKSCAN_API: &str = "https://api.starkscan.co";

/// Where a deposit gets proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepositProver {
    JsonRpc { url: String, label: &'static str },
    Starkscan { api_key: String },
}

impl DepositProver {
    /// Pick the prover for `chain` from the user's settings, or explain what's
    /// missing.
    pub fn resolve(cfg: &Strk20Config, chain: ChainId) -> Result<Self, String> {
        if !cfg.deposit_prover_url.trim().is_empty() {
            return Ok(DepositProver::JsonRpc { url: cfg.deposit_prover_url.trim().to_string(), label: "your deposit prover" });
        }
        match chain {
            ChainId::Mainnet if !cfg.starkscan_api_key.trim().is_empty() => {
                Ok(DepositProver::Starkscan { api_key: cfg.starkscan_api_key.trim().to_string() })
            }
            ChainId::Mainnet => Ok(DepositProver::JsonRpc { url: BRAMBLE_PROVER_URL.into(), label: "the bramble gateway" }),
            ChainId::Sepolia => Err("deposits on Sepolia need a screening prover: set a deposit prover URL in \
                                     Settings → STRK20 (Starkscan and the bramble gateway are mainnet-only)"
                .into()),
        }
    }

    /// Who will see the invocation, for the approval prompt.
    pub fn describe(&self) -> &'static str {
        match self {
            DepositProver::JsonRpc { label, .. } => label,
            DepositProver::Starkscan { .. } => "Starkscan's STRK20 prover",
        }
    }
}

/// A deposit prover's failure, classified so the caller can tell a screening
/// verdict (final) from a transient fault (retry later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// The screener refused the depositor. Resubmitting unchanged won't help.
    ScreeningRejected(String),
    /// Screening or the prover is temporarily unavailable.
    Unavailable(String),
    /// The relay may have proved it but the result is lost; don't resubmit.
    DeliveryUnknown(String),
    Other(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RemoteError::ScreeningRejected(m) => write!(f, "deposit screening rejected the depositor: {m}"),
            RemoteError::Unavailable(m) => write!(f, "deposit prover unavailable, try again later: {m}"),
            RemoteError::DeliveryUnknown(m) => {
                write!(f, "the deposit prover's result was lost; do not resubmit before checking: {m}")
            }
            RemoteError::Other(m) => write!(f, "deposit prover error: {m}"),
        }
    }
}

/// Map a JSON-RPC prover error. Code 10000 is overloaded; only the proof
/// interceptor's exact reasons are verdicts (SDK `screeningErrorFromProvingError`).
pub fn classify_rpc_error(err: &Value) -> RemoteError {
    let code = err.get("code").and_then(Value::as_i64);
    let data = err.get("data").and_then(Value::as_str).unwrap_or_default();
    let text = err.to_string();
    match (code, data) {
        (Some(10000), "address_blocked") => RemoteError::ScreeningRejected("address_blocked".into()),
        (Some(10000), "screening_unavailable" | "screening_policy_unavailable") => RemoteError::Unavailable(data.into()),
        (Some(-32005), _) => RemoteError::Unavailable(text),
        _ => RemoteError::Other(text),
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(900)).build().unwrap_or_default()
}

fn chain_slug(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Mainnet => "SN_MAIN",
        ChainId::Sepolia => "SN_SEPOLIA",
    }
}

/// Prove `transaction` at `block_number`. Returns the prover's result object
/// (`proof`, `proof_facts`, `l2_to_l1_messages`, `additional_data`).
pub async fn prove(
    prover: &DepositProver,
    chain: ChainId,
    block_number: u64,
    transaction: &Value,
) -> Result<Value, RemoteError> {
    match prover {
        DepositProver::JsonRpc { url, .. } => prove_json_rpc(url, block_number, transaction).await,
        DepositProver::Starkscan { api_key } => {
            prove_starkscan(STARKSCAN_API, api_key, chain, block_number, transaction).await
        }
    }
}

async fn prove_json_rpc(url: &str, block_number: u64, transaction: &Value) -> Result<Value, RemoteError> {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "starknet_proveTransaction",
        "params": { "block_id": { "block_number": block_number }, "transaction": transaction },
    });
    let resp = http().post(url).json(&body).send().await.map_err(|e| RemoteError::Unavailable(e.to_string()))?;
    let status = resp.status();
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(RemoteError::Unavailable(format!("HTTP {status}")));
    }
    let v: Value = resp.json().await.map_err(|e| RemoteError::Other(format!("HTTP {status}: {e}")))?;
    if let Some(err) = v.get("error") {
        return Err(classify_rpc_error(err));
    }
    v.get("result").cloned().ok_or_else(|| RemoteError::Other("no result".into()))
}

/// Starkscan's relay: submit with an idempotency key, then poll. The proof is
/// delivered once and then dropped, so the caller must keep what this returns.
pub(crate) async fn prove_starkscan(
    base: &str,
    api_key: &str,
    chain: ChainId,
    block_number: u64,
    transaction: &Value,
) -> Result<Value, RemoteError> {
    if chain != ChainId::Mainnet {
        return Err(RemoteError::Other("Starkscan's STRK20 prover is mainnet-only".into()));
    }
    let client = http();
    let root = format!("{}/v1/{}/prove", base.trim_end_matches('/'), chain_slug(chain));
    let idempotency_key = random_key();
    let body = json!({ "block_id": { "block_number": block_number }, "transaction": transaction });

    // Submit; `unavailable` asks for a resubmit with the same key, which is
    // safe (a retry with the same key and body never double-debits).
    for _ in 0..5 {
        let resp = client
            .post(&root)
            .header("X-Starkscan-Api-Key", api_key)
            .header("Idempotency-Key", &idempotency_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| RemoteError::Unavailable(e.to_string()))?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(starkscan_http_error(status, &v));
        }
        let job = v.get("jobId").and_then(Value::as_str).ok_or_else(|| RemoteError::Other(format!("no jobId in {v}")))?;
        let mut wait = v.get("pollAfterSeconds").and_then(Value::as_u64).unwrap_or(5);
        loop {
            tokio::time::sleep(Duration::from_secs(wait.clamp(1, 30))).await;
            let resp = client
                .get(format!("{root}/{job}"))
                .header("X-Starkscan-Api-Key", api_key)
                .send()
                .await
                .map_err(|e| RemoteError::Unavailable(e.to_string()))?;
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                return Err(starkscan_http_error(status, &v));
            }
            match v.get("status").and_then(Value::as_str) {
                Some("succeeded") => {
                    return v.get("result").cloned().ok_or_else(|| {
                        RemoteError::DeliveryUnknown(format!(
                            "job {job} succeeded but carried no result ({})",
                            v.get("resultUnavailableReason").cloned().unwrap_or(Value::Null)
                        ))
                    })
                }
                Some("failed") => {
                    let err = v.get("error").cloned().unwrap_or(Value::Null);
                    return Err(classify_rpc_error(&err));
                }
                Some("unavailable") => break, // resubmit with the same key
                Some("unknown_delivery") => {
                    return Err(RemoteError::DeliveryUnknown(format!("job {job}: prover_delivery_unknown")))
                }
                _ => wait = v.get("pollAfterSeconds").and_then(Value::as_u64).unwrap_or(5),
            }
        }
    }
    Err(RemoteError::Unavailable("Starkscan kept reporting the prover unavailable".into()))
}

fn starkscan_http_error(status: reqwest::StatusCode, body: &Value) -> RemoteError {
    let what = body.get("error").cloned().unwrap_or_else(|| body.clone());
    match status.as_u16() {
        401 => RemoteError::Other(format!("Starkscan rejected the API key ({what})")),
        403 => RemoteError::Other(format!("the Starkscan API key lacks the `prove` scope ({what})")),
        404 => RemoteError::Unavailable(format!("Starkscan's STRK20 relay is not enabled ({what})")),
        429 | 503 => RemoteError::Unavailable(format!("HTTP {status}: {what}")),
        _ => RemoteError::Other(format!("HTTP {status}: {what}")),
    }
}

/// 32 hex chars (16–128 graphic ASCII is required).
fn random_key() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    hex::encode(b)
}

/// Turn a prover result into the pieces strkd uses.
pub struct ProvedInvocation {
    pub proof: String,
    pub proof_facts: Vec<Felt>,
    /// The pool's L2→L1 payload: `[class_hash, ...server_actions]`.
    pub output: Vec<Felt>,
    pub screening: Option<strk20::apply::ScreeningAttestation>,
}

pub fn parse_result(result: &Value, pool: &Felt) -> Result<ProvedInvocation, String> {
    let felts = |v: &Value, what: &str| -> Result<Vec<Felt>, String> {
        v.as_array()
            .ok_or_else(|| format!("{what}: not an array"))?
            .iter()
            .map(|x| x.as_str().and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| format!("{what}: bad felt {x}")))
            .collect()
    };
    let proof = result.get("proof").and_then(Value::as_str).ok_or("no proof in the prover result")?.to_string();
    let proof_facts = felts(result.get("proof_facts").unwrap_or(&Value::Null), "proof_facts")?;
    let messages = result.get("l2_to_l1_messages").and_then(Value::as_array).ok_or("no l2_to_l1_messages")?;
    let message = messages
        .iter()
        .find(|m| m.get("from_address").and_then(Value::as_str).and_then(|s| Felt::from_hex(s).ok()) == Some(*pool))
        .ok_or("the proof carries no message from the pool")?;
    let output = felts(message.get("payload").unwrap_or(&Value::Null), "payload")?;
    let screening = match result.pointer("/additional_data/signature") {
        None | Some(Value::Null) => None,
        Some(sig) => Some(serde_json::from_value(sig.clone()).map_err(|e| format!("bad screening attestation: {e}"))?),
    };
    Ok(ProvedInvocation { proof, proof_facts, output, screening })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_deposit_prover_by_precedence() {
        let mut cfg = Strk20Config::default();
        assert!(matches!(DepositProver::resolve(&cfg, ChainId::Mainnet), Ok(DepositProver::JsonRpc { url, .. }) if url == BRAMBLE_PROVER_URL));
        assert!(DepositProver::resolve(&cfg, ChainId::Sepolia).is_err());
        cfg.starkscan_api_key = "k".into();
        assert_eq!(DepositProver::resolve(&cfg, ChainId::Mainnet), Ok(DepositProver::Starkscan { api_key: "k".into() }));
        cfg.deposit_prover_url = "http://localhost:1".into();
        assert!(matches!(DepositProver::resolve(&cfg, ChainId::Sepolia), Ok(DepositProver::JsonRpc { .. })));
    }

    #[test]
    fn only_exact_interceptor_reasons_are_verdicts() {
        assert_eq!(
            classify_rpc_error(&json!({"code": 10000, "data": "address_blocked"})),
            RemoteError::ScreeningRejected("address_blocked".into())
        );
        assert!(matches!(classify_rpc_error(&json!({"code": 10000, "data": "screening_unavailable"})), RemoteError::Unavailable(_)));
        assert!(matches!(classify_rpc_error(&json!({"code": 10000, "data": "boom"})), RemoteError::Other(_)));
        assert!(matches!(classify_rpc_error(&json!({"code": -32005})), RemoteError::Unavailable(_)));
    }

    #[test]
    fn parses_prover_result_with_attestation() {
        let r = json!({
            "proof": "AAAA", "proof_facts": ["0x1", "0x2"],
            "l2_to_l1_messages": [{"from_address": "0x99", "to_address": "0x0", "payload": ["0xc1a55", "0x1"]}],
            "additional_data": {"signature": {"issued_at": 1700000000, "sig_r": "0x3", "sig_s": "0x4"}},
        });
        let p = parse_result(&r, &Felt::from(0x99u64)).unwrap();
        assert_eq!(p.output, vec![Felt::from(0xc1a55u64), Felt::ONE]);
        assert_eq!(p.screening.unwrap().issued_at, 1_700_000_000);
        assert!(parse_result(&r, &Felt::from(0x98u64)).is_err());
    }
}
