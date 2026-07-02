//! Shared SNIP-36 proving helpers for the native backend.
//!
//! Backend-independent logic lives here: nonce preflight, the env the CLI needs,
//! output parsing, and the process-group-managed CLI runner. (`../dinner` shared
//! this between a native and a docker backend; strkd ships only the native one,
//! so `run_and_parse` — the process-group reaper that lived in `docker_prover` —
//! now lives here next to the other shared helpers.)

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::prover::{ProveRequest, ProveResult};
use crate::settings::SettingsStore;

pub const STRK_FEE_TOKEN: &str =
    "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";

/// Shared HTTP client for RPC preflight + remote proving.
///
/// Redirects are **not** followed: a JSON-RPC POST that 301s (e.g. `http://`→
/// `https://`) gets silently turned into a GET per RFC, the node returns
/// non-JSON, and preflight failed with an opaque decode error. With no-follow we
/// see the 3xx and report it. Only a connect timeout is set (not a request
/// timeout) so slow remote proves aren't cut off; quick RPC calls set their own.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

/// First ~300 chars of a body/stderr, trimmed — enough to diagnose, bounded.
fn snippet(s: &str) -> String {
    s.trim().chars().take(300).collect()
}

/// A JSON-RPC call with explicit handling of redirects, non-2xx, and non-JSON
/// bodies (each of which previously surfaced as `expected value at line 1
/// column 1` with no context).
async fn rpc_call(http: &reqwest::Client, rpc: &str, method: &str, params: Value) -> Result<Value, String> {
    let resp = http
        .post(rpc)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("rpc {method}: request failed ({e})"))?;
    let status = resp.status();
    if status.is_redirection() {
        let loc = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?");
        return Err(format!(
            "rpc {method}: rpc_url returned {status} → {loc}. JSON-RPC needs the final URL — \
             set the https URL directly in Settings (a 301 silently turns the POST into a GET)."
        ));
    }
    let text = resp.text().await.map_err(|e| format!("rpc {method}: reading body failed ({e})"))?;
    if !status.is_success() {
        return Err(format!("rpc {method}: HTTP {status}: {}", snippet(&text)));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|e| format!("rpc {method}: non-JSON response ({e}); body: {}", snippet(&text)))
}

/// Map a known prover-CLI failure to an actionable message; else return the raw
/// stderr. Lets a driving agent react instead of staring at a backtrace.
pub fn classify_prover_error(stderr: &str) -> String {
    let low = stderr.to_lowercase();
    if low.contains("429") || low.contains("too many requests") || low.contains("rpc provider error") {
        format!(
            "upstream RPC throttled (HTTP 429 / provider error) — proving fetches lots of state; \
             use a higher-quota RPC in Settings. raw: {}",
            snippet(stderr)
        )
    } else if low.contains("state_diff_commitment") {
        format!(
            "RPC too old (missing `state_diff_commitment`) — needs a spec ≥0.10 endpoint. raw: {}",
            snippet(stderr)
        )
    } else if low.contains("port") && low.contains("already in use") {
        format!("prover port in use (stale runner) — retry; strkd picks a fresh port per prove. raw: {}", snippet(stderr))
    } else if low.contains("invalid starknet version") || low.contains("convert block header") {
        format!(
            "the bundled prover does not support this network's Starknet protocol version — it \
             rejected the reference block header, so the pinned prover stack predates the network's \
             current protocol. Fix: update the prover pin (desktop/scripts/prover-pin.env) to a \
             release that supports it and re-stage, or set the prover backend to `remote` (Settings) \
             and point at a compatible prover. raw: {}",
            snippet(stderr)
        )
    } else {
        format!("prover failed: {}", snippet(stderr))
    }
}

/// `(chain_id, strk_fee_token)` for a network name.
pub fn net_params(network: &str) -> (&'static str, &'static str) {
    match network {
        "testnet" | "sepolia" => ("SN_SEPOLIA", STRK_FEE_TOKEN),
        _ => ("SN_MAIN", STRK_FEE_TOKEN),
    }
}

/// Validated, backend-independent inputs for one prove.
pub struct ProveInputs {
    pub tx: Value,
    pub block_number: u64,
    pub chain_id: &'static str,
    pub strk: &'static str,
    pub rpc_url: String,
}

/// Validate the request and resolve the reference block + nonce, independent of
/// which backend will run the prover. Returns a clear error early (before any
/// ~minute-long proof) on the common failures.
pub async fn preflight(
    settings: &SettingsStore,
    http: &reqwest::Client,
    req: &ProveRequest,
) -> Result<ProveInputs, String> {
    let net = settings.for_network(&req.network).await;
    if net.rpc_url.is_empty() {
        return Err(format!("no RPC URL configured for {} (set it in Settings)", req.network));
    }

    let tx = req
        .payload
        .get("transaction")
        .cloned()
        .ok_or("payload missing `transaction` (the signed Tx A)")?;

    // Invariant: the prover never signs — reject unsigned transactions up front.
    let signed = tx
        .get("signature")
        .and_then(|s| s.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !signed {
        return Err("transaction is not signed (missing/empty `signature`); the prover proves \
                    pre-signed transactions only and never signs"
            .into());
    }

    let block_number = match req.payload.get("block_number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => latest_block(http, &net.rpc_url).await?.saturating_sub(2),
    };

    check_nonce(http, &net.rpc_url, &tx, block_number).await?;

    let (chain_id, strk) = net_params(&req.network);
    Ok(ProveInputs { tx, block_number, chain_id, strk, rpc_url: net.rpc_url })
}

/// The env vars the `snip36` CLI's shared config requires. `prove virtual-os`
/// only uses RPC + chain id; account/key are for `submit`, so they're dummy
/// `0x1` values — no real key is ever needed (the prover holds none).
pub fn prove_env(inp: &ProveInputs) -> Vec<(&'static str, String)> {
    vec![
        ("STARKNET_CHAIN_ID", inp.chain_id.to_string()),
        ("STARKNET_RPC_URL", inp.rpc_url.clone()),
        ("STARKNET_STRK_TOKEN", inp.strk.to_string()),
        ("STARKNET_ACCOUNT_ADDRESS", "0x1".to_string()),
        ("STARKNET_PRIVATE_KEY", "0x1".to_string()),
    ]
}

/// Read the prover's outputs from `dir` (`out.proof`, `out.proof_facts`,
/// `out.raw_messages.json`) into the result shape callers consume.
pub fn parse_outputs(dir: &Path) -> Result<ProveResult, String> {
    let proof = std::fs::read_to_string(dir.join("out.proof"))
        .map_err(|e| format!("read proof: {e}"))?
        .trim()
        .to_string();
    let facts: Value = std::fs::read_to_string(dir.join("out.proof_facts"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let messages: Value = std::fs::read_to_string(dir.join("out.raw_messages.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("l2_to_l1_messages").cloned())
        .unwrap_or(Value::Array(vec![]));
    Ok(ProveResult {
        proof: json!({ "proof": proof, "proof_facts": facts, "l2_to_l1_messages": messages }),
    })
}

async fn latest_block(http: &reqwest::Client, rpc: &str) -> Result<u64, String> {
    let v = rpc_call(http, rpc, "starknet_blockNumber", json!([])).await?;
    v.get("result")
        .and_then(|r| r.as_u64())
        .ok_or_else(|| format!("rpc blockNumber: unexpected response {v}"))
}

/// Fetch the account nonce at `block` and check it matches the tx nonce — saves
/// a ~minute-long proof when the nonce is wrong.
async fn check_nonce(http: &reqwest::Client, rpc: &str, tx: &Value, block: u64) -> Result<(), String> {
    let (Some(sender), Some(tx_nonce)) = (
        tx.get("sender_address").and_then(|v| v.as_str()),
        tx.get("nonce").and_then(|v| v.as_str()),
    ) else {
        return Ok(()); // can't check without sender + nonce
    };
    let v = rpc_call(http, rpc, "starknet_getNonce", json!([{ "block_number": block }, sender])).await?;
    let Some(chain_nonce) = v.get("result").and_then(|r| r.as_str()) else {
        return Ok(()); // RPC didn't return a nonce; let the prover surface it
    };
    let parse_hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok();
    if let (Some(a), Some(b)) = (parse_hex(tx_nonce), parse_hex(chain_nonce)) {
        if a != b {
            return Err(format!(
                "nonce mismatch: virtual tx has nonce {tx_nonce} but account {sender} had \
                 nonce {chain_nonce} at block {block}. The virtual tx nonce must equal the \
                 account nonce at the reference block, not just the current nonce."
            ));
        }
    }
    Ok(())
}

/// Run a configured `prove virtual-os` command and parse its outputs from `dir`.
///
/// Runs the CLI in its own process group (unix) with a timeout
/// (`STRKD_PROVE_TIMEOUT_SECS`, default 900): on timeout or failure the whole
/// group is killed, so the runner the CLI spawns can't outlive the job and leak
/// its port. Recognized failures get a friendly message via `classify_prover_error`.
pub(crate) async fn run_and_parse(
    mut cmd: tokio::process::Command,
    dir: &std::path::Path,
    launch_hint: &str,
) -> Result<ProveResult, String> {
    let secs: u64 = std::env::var("STRKD_PROVE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);

    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true); // reap the direct child if we bail
    #[cfg(unix)]
    cmd.process_group(0); // child leads its own group so we can reap grandchildren

    let child = cmd.spawn().map_err(|e| format!("failed to launch prover ({launch_hint}): {e}"))?;
    #[cfg(unix)]
    let pgid = child.id().map(|p| p as i32);
    #[cfg(unix)]
    let kill_group = || {
        if let Some(pg) = pgid {
            unsafe { libc::kill(-pg, libc::SIGKILL) };
        }
    };

    let output = match tokio::time::timeout(std::time::Duration::from_secs(secs), child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("prover wait failed: {e}")),
        Err(_) => {
            #[cfg(unix)]
            kill_group();
            return Err(format!("prover timed out after {secs}s (killed)"));
        }
    };

    if !output.status.success() {
        // Reap any runner the CLI may have orphaned on failure.
        #[cfg(unix)]
        kill_group();
        return Err(classify_prover_error(&String::from_utf8_lossy(&output.stderr)));
    }
    parse_outputs(dir)
}

#[cfg(test)]
mod tests {
    use super::classify_prover_error;

    #[test]
    fn unsupported_protocol_version_is_actionable() {
        // The exact shape reported from Sepolia 0.14.3 against the v1.1.3 prover.
        let stderr = r#"starknet_proveTransaction failed: {"code":-32603,"data":"Transaction execution failed: Failed to convert block header to block info: Invalid Starknet version: [0, 14, 3]","message":"Internal error"}"#;
        let msg = classify_prover_error(stderr);
        assert!(msg.contains("does not support this network's Starknet protocol"));
        assert!(msg.contains("prover-pin.env") && msg.contains("remote"));
        // Still carries the raw error for debugging.
        assert!(msg.contains("Invalid Starknet version"));
    }

    #[test]
    fn unknown_error_falls_back() {
        let msg = classify_prover_error("some other failure");
        assert!(msg.starts_with("prover failed:"));
    }
}
