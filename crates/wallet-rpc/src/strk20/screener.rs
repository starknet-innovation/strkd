//! Per-network pool settings: validation, the pool check behind Settings →
//! STRK20, and the **testnet-only** local screener for a self-deployed pool.
//!
//! A pool's constructor takes its screener public key, and the pool accepts a
//! deposit only with an attestation that key signed (`strk20::screening`). On
//! the canonical pools only the operator's screening prover holds it. For a
//! pool you deployed yourself on Sepolia you hold it, so strkd can prove the
//! deposit on this machine and sign the attestation itself: nothing leaves the
//! machine, not even the viewing key.
//!
//! Guard rails, each tested:
//! - refused on mainnet, both when the settings are saved and at deposit time;
//! - checked against the pool's `get_screener_public_key` before the approval
//!   prompt, so a key that cannot attest for this pool fails up front;
//! - never echoed: errors name the setting, not its value, and the settings
//!   type's `Debug` redacts it.

use serde_json::{json, Value};
use wallet_core::{get_selector_from_name, ChainId, Felt};

use crate::error::WalletRpcError;
use crate::node::StarknetRpc;

/// The privacy pool class strkd is built against (mainnet's canonical pool,
/// Sepolia's canonical pool and strkd's Sepolia test pool all run it).
pub const KNOWN_POOL_CLASS: &str = "0x6d163f2b27df0f53c5b0d019366261ba8034af1bef949dee920a60fe58bcf83";

fn http_or_empty(name: &str, url: &str) -> Result<(), String> {
    let u = url.trim();
    if u.is_empty() || u.starts_with("http://") || u.starts_with("https://") {
        Ok(())
    } else {
        Err(format!("STRK20 {name} must be an http(s) URL or empty"))
    }
}

/// Parse a screener key without ever putting it in an error message.
fn parse_key(raw: &str) -> Result<Felt, String> {
    let sk = Felt::from_hex(raw.trim()).map_err(|_| "the STRK20 screener key is not a hex felt".to_string())?;
    wallet_core::public_key_of_key(&sk).map_err(|_| "the STRK20 screener key is not a valid Stark private key".to_string())?;
    Ok(sk)
}

/// Check one network's STRK20 settings before they are saved.
pub fn validate(cfg: &prover::Strk20Config, chain: ChainId) -> Result<(), String> {
    let pool = cfg.pool.trim();
    if !pool.is_empty() && Felt::from_hex(pool).is_err() {
        return Err(format!("STRK20 pool {pool} is not a hex felt address"));
    }
    http_or_empty("deposit prover URL", &cfg.deposit_prover_url)?;
    http_or_empty("paymaster URL", &cfg.paymaster_url)?;
    if !cfg.screener_key.trim().is_empty() {
        if chain == ChainId::Mainnet {
            return Err("a local STRK20 screener key is testnet-only: strkd refuses it on mainnet".into());
        }
        parse_key(&cfg.screener_key)?;
    }
    Ok(())
}

/// The local screener key for `chain`, if one is set. Refused on mainnet.
pub fn local_key(cfg: &prover::Strk20Config, chain: ChainId) -> Result<Option<Felt>, WalletRpcError> {
    if cfg.screener_key.trim().is_empty() {
        return Ok(None);
    }
    if chain == ChainId::Mainnet {
        return Err(WalletRpcError::Precondition(
            "a local STRK20 screener key is set for mainnet, and it is testnet-only: remove it in Settings → STRK20"
                .into(),
        ));
    }
    parse_key(&cfg.screener_key).map(Some).map_err(|m| WalletRpcError::InvalidRequest(format!("Settings: {m}")))
}

async fn view(node: &dyn StarknetRpc, pool: &Felt, name: &str, block: Value) -> Result<Felt, String> {
    let request = json!({ "contract_address": format!("{pool:#x}"),
                          "entry_point_selector": format!("{:#x}", get_selector_from_name(name)), "calldata": [] });
    let out = node.raw("starknet_call", json!({ "request": request, "block_id": block })).await.map_err(|e| format!("{name}: {e}"))?;
    out.get(0).and_then(Value::as_str).and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| format!("{name}: bad result {out}"))
}

/// Refuse a screener key that is not the pool's: its attestations would revert
/// with `SCREENING_INVALID_SIGNATURE` after a wasted proof.
pub async fn check_matches_pool(node: &dyn StarknetRpc, pool: &Felt, sk: &Felt) -> Result<(), WalletRpcError> {
    let expected = view(node, pool, "get_screener_public_key", json!("latest")).await.map_err(WalletRpcError::Node)?;
    let ours = wallet_core::public_key_of_key(sk).map_err(|_| WalletRpcError::InvalidRequest("Settings: bad screener key".into()))?;
    if ours != expected {
        return Err(WalletRpcError::Precondition(format!(
            "the local screener key in Settings → STRK20 is not pool {pool:#x}'s screener (the pool expects public key \
             {expected:#x}, the key's is {ours:#x}). It only works for a pool deployed with that key."
        )));
    }
    Ok(())
}

/// Sign a deposit attestation for `depositor`, dated at the latest block's
/// timestamp (the pool measures age against the block timestamp and rejects a
/// future date).
pub async fn attest(
    node: &dyn StarknetRpc,
    chain: ChainId,
    depositor: Felt,
    sk: &Felt,
) -> Result<strk20::apply::ScreeningAttestation, WalletRpcError> {
    let block = node.raw("starknet_getBlockWithTxHashes", json!({ "block_id": "latest" })).await.map_err(|e| WalletRpcError::Node(e.to_string()))?;
    let issued_at = block.get("timestamp").and_then(Value::as_u64).ok_or_else(|| WalletRpcError::Node("latest block: no timestamp".into()))?;
    strk20::screening::attest(sk, chain.as_felt(), depositor, issued_at)
        .map_err(|_| WalletRpcError::Unknown("signing the screening attestation failed".into()))
}

/// What Settings → STRK20 shows about a pool address before it is saved.
pub async fn pool_info(node: &dyn StarknetRpc, pool: &Felt, screener_key: Option<&Felt>) -> Value {
    let class = node
        .raw("starknet_getClassHashAt", json!({ "block_id": "latest", "contract_address": format!("{pool:#x}") }))
        .await
        .map_err(|e| e.to_string())
        .and_then(|v| v.as_str().and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| format!("bad class hash {v}")));
    let class = match class {
        Ok(c) => c,
        Err(e) if e.to_lowercase().contains("contract not found") || e.contains("\"code\":20") => {
            return json!({ "deployed": false, "warnings": [format!("no contract is deployed at {pool:#x} on this network")] })
        }
        Err(e) => return json!({ "deployed": null, "warnings": [format!("could not read {pool:#x}: {e}")] }),
    };
    let known = class == Felt::from_hex(KNOWN_POOL_CLASS).expect("constant felt");
    let screener = view(node, pool, "get_screener_public_key", json!("latest")).await.ok();
    let version = view(node, pool, "get_version", json!("latest")).await.ok();
    let fee = view(node, pool, "get_fee_amount", json!("latest")).await.ok();
    let mut warnings = Vec::new();
    if !known {
        warnings.push(format!(
            "class {class:#x} is not the privacy-pool class strkd is built against ({KNOWN_POOL_CLASS}); STRK20 calls may fail"
        ));
    }
    if screener.is_none() {
        warnings.push("the contract has no get_screener_public_key view: it does not look like a privacy pool".to_string());
    }
    let screener_matches = match (screener_key, screener) {
        (Some(sk), Some(expected)) => {
            let ok = wallet_core::public_key_of_key(sk).map(|pk| pk == expected).unwrap_or(false);
            if !ok {
                warnings.push("the screener key does not match this pool's screener public key".to_string());
            }
            Some(ok)
        }
        _ => None,
    };
    json!({
        "deployed": true,
        "class_hash": format!("{class:#x}"),
        "known_class": known,
        "version": version.map(|v| String::from_utf8_lossy(&v.to_bytes_be()).trim_start_matches('\0').to_string()),
        "screener_public_key": screener.map(|s| format!("{s:#x}")),
        "fee_amount": fee.map(|f| f.to_string()),
        "screener_matches": screener_matches,
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The public test key from starknet-privacy's screening vectors.
    const TEST_SCREENER: &str = "0xCAFEBABE";

    fn cfg(screener_key: &str) -> prover::Strk20Config {
        prover::Strk20Config { screener_key: screener_key.into(), ..Default::default() }
    }

    #[test]
    fn local_key_is_refused_on_mainnet() {
        assert!(matches!(local_key(&cfg(TEST_SCREENER), ChainId::Mainnet), Err(WalletRpcError::Precondition(_))));
        assert!(validate(&cfg(TEST_SCREENER), ChainId::Mainnet).unwrap_err().contains("testnet-only"));
        assert_eq!(local_key(&cfg(TEST_SCREENER), ChainId::Sepolia).unwrap(), Some(Felt::from(0xCAFEBABEu64)));
        assert_eq!(local_key(&cfg(""), ChainId::Mainnet).unwrap(), None);
        assert!(validate(&cfg(TEST_SCREENER), ChainId::Sepolia).is_ok());
    }

    #[test]
    fn errors_never_echo_the_key() {
        for bad in ["0xnot-a-felt-cafe", "0x0"] {
            let e = validate(&cfg(bad), ChainId::Sepolia).unwrap_err();
            assert!(!e.contains(bad), "{e}");
            let e = local_key(&cfg(bad), ChainId::Sepolia).unwrap_err().message();
            assert!(!e.contains(bad), "{e}");
        }
    }

    #[test]
    fn validates_pool_and_urls() {
        let mut c = cfg("");
        c.pool = "pool".into();
        assert!(validate(&c, ChainId::Sepolia).is_err());
        c.pool = "0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323".into();
        assert!(validate(&c, ChainId::Sepolia).is_ok());
        c.deposit_prover_url = "ftp://x".into();
        assert!(validate(&c, ChainId::Sepolia).is_err());
    }
}
