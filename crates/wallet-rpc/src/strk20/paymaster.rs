//! AVNU paymaster, `sponsored_private` mode: relays a STRK20 batch so that no
//! public account of the user's submits it.
//!
//! Protocol (as bramble uses it, `packages/features/privacy/src/paymaster`):
//! 1. `paymaster_buildTransaction` with `{type: "apply_action", apply_action:
//!    {pool_address}}` quotes the relay's fee as a **withdraw action** to the
//!    relayer. The wallet adds that withdraw to the private batch, so the relay
//!    is paid from the shielded balance, unlinkably.
//! 2. After proving, `paymaster_executeTransaction` carries the `apply_actions`
//!    call, the proof and its facts; the relay submits and returns the hash.
//!
//! Requests carry the user's AVNU key in `x-paymaster-api-key` (verified
//! against AVNU's mainnet and Sepolia endpoints, which reject a missing key).
//! Only the deposit-free `apply_action` route is supported: deposits are public
//! anyway and strkd submits them from the user's own account.

use std::time::Duration;

use serde_json::{json, Value};
use wallet_core::{ChainId, Felt};

pub const AVNU_MAINNET: &str = "https://starknet.paymaster.avnu.fi";
pub const AVNU_SEPOLIA: &str = "https://sepolia.paymaster.avnu.fi";

pub fn default_url(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Mainnet => AVNU_MAINNET,
        ChainId::Sepolia => AVNU_SEPOLIA,
    }
}

/// The relay's fee, paid by a withdraw from the private batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeAction {
    pub token: Felt,
    pub amount: u128,
    pub recipient: Felt,
}

pub struct Paymaster {
    url: String,
    api_key: String,
    fee_token: Felt,
    http: reqwest::Client,
}

fn hex(f: &Felt) -> String {
    format!("{f:#x}")
}

impl Paymaster {
    pub fn new(url: &str, api_key: &str, fee_token: Felt) -> Self {
        Paymaster {
            url: url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            fee_token,
            http: reqwest::Client::builder().timeout(Duration::from_secs(120)).build().unwrap_or_default(),
        }
    }

    fn parameters(&self) -> Value {
        json!({ "version": "0x1", "fee_mode": { "mode": "sponsored_private", "pool_fee_token": hex(&self.fee_token), "tip": "normal" } })
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp = self
            .http
            .post(&self.url)
            .header("x-paymaster-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("paymaster unreachable: {e}"))?;
        let v: Value = resp.json().await.map_err(|e| format!("paymaster: bad response: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("paymaster refused {method}: {err}"));
        }
        v.get("result").cloned().ok_or_else(|| format!("paymaster: {method} returned no result"))
    }

    /// Quote the relay fee for a batch on `pool`, refusing a quote that would
    /// pay the user's own public address (linking them) or another token.
    pub async fn quote(&self, pool: &Felt, user: &Felt) -> Result<FeeAction, String> {
        let r = self
            .rpc(
                "paymaster_buildTransaction",
                json!({ "transaction": { "type": "apply_action", "apply_action": { "pool_address": hex(pool) } }, "parameters": self.parameters() }),
            )
            .await?;
        parse_fee_action(&r, &self.fee_token, user)
    }

    /// Hand the proven batch to the relay. An error *after* the request left
    /// may mean it was submitted anyway; the message says so.
    pub async fn execute(&self, call: &wallet_core::Call, proof: &str, proof_facts: &[Felt]) -> Result<Felt, String> {
        let r = self
            .rpc(
                "paymaster_executeTransaction",
                json!({
                    "transaction": { "type": "apply_action", "apply_action": {
                        "apply_actions_call": { "to": hex(&call.to), "selector": hex(&call.selector), "calldata": call.calldata.iter().map(hex).collect::<Vec<_>>() },
                        "proof": proof,
                        "proof_facts": proof_facts.iter().map(hex).collect::<Vec<_>>(),
                    } },
                    "parameters": self.parameters(),
                }),
            )
            .await
            .map_err(|e| format!("{e} (the relay may still have submitted it: check before retrying)"))?;
        r.get("transaction_hash")
            .and_then(Value::as_str)
            .and_then(|s| Felt::from_hex(s).ok())
            .filter(|h| *h != Felt::ZERO)
            .ok_or_else(|| "the relay accepted the batch but returned no transaction hash: check before retrying".into())
    }
}

pub(crate) fn parse_fee_action(quote: &Value, fee_token: &Felt, user: &Felt) -> Result<FeeAction, String> {
    if quote.get("type").and_then(Value::as_str) != Some("apply_action") {
        return Err(format!("unexpected paymaster quote: {quote}"));
    }
    let fa = quote.get("fee_action").ok_or("the paymaster quote has no fee_action")?;
    if fa.get("type").and_then(Value::as_str) != Some("withdraw") {
        return Err(format!("unexpected paymaster fee action: {fa}"));
    }
    let felt = |k: &str| fa.get(k).and_then(Value::as_str).and_then(|s| Felt::from_hex(s).ok()).ok_or(format!("fee_action.{k} missing"));
    let token = felt("token")?;
    let recipient = felt("recipient")?;
    let amount = u128::try_from(felt("amount")?).map_err(|_| "fee_action.amount exceeds u128".to_string())?;
    if token != *fee_token {
        return Err(format!("the paymaster asked for its fee in {token:#x}, not the configured {fee_token:#x}"));
    }
    if recipient == *user || recipient == Felt::ZERO {
        return Err("the paymaster's fee recipient is your own address, which would link the batch to you".into());
    }
    Ok(FeeAction { token, amount, recipient })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRK: u64 = 0x4718;

    #[test]
    fn accepts_a_withdraw_fee_to_the_relayer() {
        let q = json!({ "type": "apply_action", "fee_action": { "type": "withdraw", "token": "0x4718", "amount": "0x10", "recipient": "0xfee" } });
        assert_eq!(
            parse_fee_action(&q, &Felt::from(STRK), &Felt::from(0xa11ceu64)),
            Ok(FeeAction { token: Felt::from(STRK), amount: 16, recipient: Felt::from(0xfeeu64) })
        );
    }

    #[test]
    fn refuses_linking_or_foreign_token_fees() {
        let user = Felt::from(0xa11ceu64);
        let to_user = json!({ "type": "apply_action", "fee_action": { "type": "withdraw", "token": "0x4718", "amount": "0x10", "recipient": "0xa11ce" } });
        assert!(parse_fee_action(&to_user, &Felt::from(STRK), &user).is_err());
        let other = json!({ "type": "apply_action", "fee_action": { "type": "withdraw", "token": "0x1", "amount": "0x10", "recipient": "0xfee" } });
        assert!(parse_fee_action(&other, &Felt::from(STRK), &user).is_err());
    }
}
