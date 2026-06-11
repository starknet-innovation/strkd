//! Starknet node client — the I/O boundary for fee estimation, nonce lookup,
//! and broadcasting (spec §7.4, Phase 2).
//!
//! `StarknetRpc` is an injectable seam (like `Approver`): the dispatch
//! orchestration (fetch nonce → estimate → sign → broadcast) is written against
//! the trait and is fully testable with a mock. `HttpStarknetRpc` is the real
//! reqwest JSON-RPC implementation.
//!
//! Wire format: **verified against a live Sepolia v0.10 node** (2026-06-09) for
//! `getNonce`, `getClassHashAt` (deploy status), and `estimateFee`
//! request+response (the 3-resource fee model, V3 tx JSON, DA modes), including
//! the http→https redirect. The block tag is `latest` (v0.10 dropped `pending`).
//! The only path not yet confirmed end-to-end is the actual **broadcast**
//! (`add_invoke` / `add_deploy_account`), which needs a real signed tx from a
//! funded account; its tx object is the same one `estimateFee` already accepted,
//! so the remaining risk is just the submit wrapper. The trait keeps any future
//! fix localized (or swap in `starknet-rs`).

use async_trait::async_trait;
use serde_json::{json, Value};
use wallet_core::{Felt, ResourceBounds};

/// The three V3 resource bounds.
#[derive(Debug, Clone, Copy)]
pub struct FeeBounds {
    pub l1_gas: ResourceBounds,
    pub l2_gas: ResourceBounds,
    pub l1_data_gas: ResourceBounds,
}

#[derive(Debug, Clone)]
pub enum NodeError {
    Transport(String),
    Rpc(String),
    Decode(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::Transport(m) => write!(f, "transport: {m}"),
            NodeError::Rpc(m) => write!(f, "rpc: {m}"),
            NodeError::Decode(m) => write!(f, "decode: {m}"),
        }
    }
}

/// Minimal Starknet JSON-RPC surface the wallet needs.
#[async_trait]
pub trait StarknetRpc: Send + Sync {
    /// Current nonce of `address` (pending block).
    async fn get_nonce(&self, address: &Felt) -> Result<Felt, NodeError>;

    /// Estimate fee for a V3 invoke and return resource bounds (safety margin
    /// already applied). Estimation uses SKIP_VALIDATE, so no signature needed.
    async fn estimate_invoke(
        &self,
        sender: &Felt,
        calldata: &[Felt],
        nonce: &Felt,
    ) -> Result<FeeBounds, NodeError>;

    /// Broadcast a signed V3 invoke; returns the transaction hash. For a SNIP-36
    /// proof-carrying invoke, `proof_facts` is non-empty and `proof` carries the
    /// base64 STWO proof (both ride along on broadcast).
    #[allow(clippy::too_many_arguments)]
    async fn add_invoke(
        &self,
        sender: &Felt,
        calldata: &[Felt],
        signature: &[Felt],
        nonce: &Felt,
        bounds: &FeeBounds,
        proof_facts: &[Felt],
        proof: Option<&str>,
    ) -> Result<Felt, NodeError>;

    /// Whether `address` has a deployed contract class (false = counterfactual).
    async fn is_deployed(&self, address: &Felt) -> Result<bool, NodeError>;

    /// ERC-20 `balanceOf(holder)` on `token`, as the low 128 bits (fri for STRK).
    /// Realistic balances fit in u128; a non-zero high word is treated as
    /// saturating-max so the UI never under-reports.
    async fn balance_of(&self, token: &Felt, holder: &Felt) -> Result<u128, NodeError>;

    /// Estimate the fee to deploy an OZ account (nonce 0, SKIP_VALIDATE).
    async fn estimate_deploy_account(
        &self,
        address: &Felt,
        class_hash: &Felt,
        constructor_calldata: &[Felt],
        salt: &Felt,
    ) -> Result<FeeBounds, NodeError>;

    /// Broadcast a signed DEPLOY_ACCOUNT v3; returns the transaction hash.
    #[allow(clippy::too_many_arguments)]
    async fn add_deploy_account(
        &self,
        class_hash: &Felt,
        constructor_calldata: &[Felt],
        salt: &Felt,
        signature: &[Felt],
        bounds: &FeeBounds,
    ) -> Result<Felt, NodeError>;

    /// Estimate the fee to declare a class (needs the full Sierra contract class).
    async fn estimate_declare(
        &self,
        sender: &Felt,
        compiled_class_hash: &Felt,
        contract_class: &Value,
        nonce: &Felt,
    ) -> Result<FeeBounds, NodeError>;

    /// Broadcast a signed DECLARE v3; returns the transaction hash.
    #[allow(clippy::too_many_arguments)]
    async fn add_declare(
        &self,
        sender: &Felt,
        compiled_class_hash: &Felt,
        contract_class: &Value,
        signature: &[Felt],
        nonce: &Felt,
        bounds: &FeeBounds,
    ) -> Result<Felt, NodeError>;
}

fn fh(f: &Felt) -> String {
    format!("0x{:x}", f)
}

fn u128_to_hex(v: u128) -> String {
    format!("0x{v:x}")
}

/// A complete, canonical-hex RPC `INVOKE_TXN_V3` object. Shared by broadcast and
/// the sign-only response so callers get a **ready-to-broadcast** transaction
/// (all fields present, hex-normalized) rather than a partial one. With
/// `signature: &[]` and zero bounds it's also the estimate-request body.
/// `proof_facts`/`proof` are added when non-empty (SNIP-36).
pub fn invoke_v3_tx_json(
    sender: &Felt,
    calldata: &[Felt],
    signature: &[Felt],
    nonce: &Felt,
    bounds: &FeeBounds,
    proof_facts: &[Felt],
    proof: Option<&str>,
) -> Value {
    let rb = |b: &ResourceBounds| {
        json!({
            "max_amount": u128_to_hex(b.max_amount as u128),
            "max_price_per_unit": u128_to_hex(b.max_price_per_unit),
        })
    };
    let mut tx = json!({
        "type": "INVOKE",
        "version": "0x3",
        // Zero-padded to 64 hex, matching strkd's account addresses elsewhere so
        // callers can compare sender_address directly.
        "sender_address": format!("0x{:064x}", sender),
        "calldata": calldata.iter().map(fh).collect::<Vec<_>>(),
        "signature": signature.iter().map(fh).collect::<Vec<_>>(),
        "nonce": fh(nonce),
        "resource_bounds": {
            "l1_gas": rb(&bounds.l1_gas),
            "l2_gas": rb(&bounds.l2_gas),
            "l1_data_gas": rb(&bounds.l1_data_gas),
        },
        "tip": "0x0",
        "paymaster_data": [],
        "account_deployment_data": [],
        "nonce_data_availability_mode": "L1",
        "fee_data_availability_mode": "L1",
    });
    if !proof_facts.is_empty() {
        let obj = tx.as_object_mut().expect("object");
        obj.insert(
            "proof_facts".into(),
            json!(proof_facts.iter().map(fh).collect::<Vec<_>>()),
        );
        if let Some(p) = proof {
            obj.insert("proof".into(), json!(p));
        }
    }
    tx
}

/// Block tag for "most recent" queries. v0.10 dropped the legacy `pending` tag;
/// `latest` is valid across spec versions (verified against a v0.10 node).
const BLOCK_TAG: &str = "latest";

/// Real JSON-RPC client over reqwest. See the module-level wire-format caveat.
pub struct HttpStarknetRpc {
    /// Effective endpoint. Some providers redirect http→https; we re-POST to the
    /// redirect target (reqwest would turn a 301 POST into a GET) and cache it.
    url: std::sync::RwLock<String>,
    client: reqwest::Client,
    /// Multiply estimated amounts/prices by this (×100, integer) for headroom.
    margin_pct: u128,
}

impl HttpStarknetRpc {
    pub fn new(url: impl Into<String>) -> Self {
        // Don't auto-follow redirects: reqwest downgrades a 301 POST to GET and
        // drops the body. We follow once manually, preserving the POST.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        HttpStarknetRpc {
            url: std::sync::RwLock::new(url.into()),
            client,
            margin_pct: 150, // 1.5×
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let target = self.url.read().unwrap().clone();
        let mut resp = self
            .client
            .post(&target)
            .json(&body)
            .send()
            .await
            .map_err(|e| NodeError::Transport(e.to_string()))?;

        // One-hop redirect (e.g. http→https), re-POSTing the body, then cache it.
        if resp.status().is_redirection() {
            if let Some(loc) = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
            {
                resp = self
                    .client
                    .post(&loc)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| NodeError::Transport(e.to_string()))?;
                *self.url.write().unwrap() = loc;
            }
        }

        let v: Value = resp
            .json()
            .await
            .map_err(|e| NodeError::Decode(e.to_string()))?;
        if let Some(err) = v.get("error") {
            return Err(NodeError::Rpc(err.to_string()));
        }
        v.get("result")
            .cloned()
            .ok_or_else(|| NodeError::Decode("missing result".into()))
    }

    fn parse_felt(v: &Value, what: &str) -> Result<Felt, NodeError> {
        let s = v
            .as_str()
            .ok_or_else(|| NodeError::Decode(format!("{what}: expected hex string")))?;
        Felt::from_hex(s).map_err(|_| NodeError::Decode(format!("{what}: bad felt {s}")))
    }

    fn parse_u128(v: &Value, what: &str) -> Result<u128, NodeError> {
        let s = v
            .as_str()
            .ok_or_else(|| NodeError::Decode(format!("{what}: expected hex string")))?;
        let s = s.strip_prefix("0x").unwrap_or(s);
        u128::from_str_radix(s, 16).map_err(|_| NodeError::Decode(format!("{what}: bad u128")))
    }

    fn with_margin(&self, v: u128) -> u128 {
        v.saturating_mul(self.margin_pct) / 100
    }


    /// DEPLOY_ACCOUNT v3 tx JSON shared by estimate (empty sig, zero bounds) and
    /// broadcast (real sig + bounds). Nonce is always 0.
    fn deploy_account_tx_json(
        class_hash: &Felt,
        constructor_calldata: &[Felt],
        salt: &Felt,
        signature: &[Felt],
        bounds: &FeeBounds,
    ) -> Value {
        let rb = |b: &ResourceBounds| {
            json!({
                "max_amount": u128_to_hex(b.max_amount as u128),
                "max_price_per_unit": u128_to_hex(b.max_price_per_unit),
            })
        };
        json!({
            "type": "DEPLOY_ACCOUNT",
            "version": "0x3",
            "class_hash": fh(class_hash),
            "contract_address_salt": fh(salt),
            "constructor_calldata": constructor_calldata.iter().map(fh).collect::<Vec<_>>(),
            "signature": signature.iter().map(fh).collect::<Vec<_>>(),
            "nonce": "0x0",
            "resource_bounds": {
                "l1_gas": rb(&bounds.l1_gas),
                "l2_gas": rb(&bounds.l2_gas),
                "l1_data_gas": rb(&bounds.l1_data_gas),
            },
            "tip": "0x0",
            "paymaster_data": [],
            "nonce_data_availability_mode": "L1",
            "fee_data_availability_mode": "L1",
        })
    }

    /// DECLARE v3 tx JSON. `contract_class` is the caller-supplied Sierra class
    /// object (the node derives the class hash from it).
    fn declare_tx_json(
        sender: &Felt,
        compiled_class_hash: &Felt,
        contract_class: &Value,
        signature: &[Felt],
        nonce: &Felt,
        bounds: &FeeBounds,
    ) -> Value {
        let rb = |b: &ResourceBounds| {
            json!({
                "max_amount": u128_to_hex(b.max_amount as u128),
                "max_price_per_unit": u128_to_hex(b.max_price_per_unit),
            })
        };
        json!({
            "type": "DECLARE",
            "version": "0x3",
            "sender_address": fh(sender),
            "compiled_class_hash": fh(compiled_class_hash),
            "contract_class": contract_class,
            "signature": signature.iter().map(fh).collect::<Vec<_>>(),
            "nonce": fh(nonce),
            "resource_bounds": {
                "l1_gas": rb(&bounds.l1_gas),
                "l2_gas": rb(&bounds.l2_gas),
                "l1_data_gas": rb(&bounds.l1_data_gas),
            },
            "tip": "0x0",
            "paymaster_data": [],
            "account_deployment_data": [],
            "nonce_data_availability_mode": "L1",
            "fee_data_availability_mode": "L1",
        })
    }

    fn bounds_from_estimate(&self, est: &Value) -> Result<FeeBounds, NodeError> {
        let bound = |amount_key: &str, price_key: &str| -> Result<ResourceBounds, NodeError> {
            Ok(ResourceBounds {
                max_amount: self.with_margin(Self::parse_u128(&est[amount_key], amount_key)?) as u64,
                max_price_per_unit: self.with_margin(Self::parse_u128(&est[price_key], price_key)?),
            })
        };
        Ok(FeeBounds {
            l1_gas: bound("l1_gas_consumed", "l1_gas_price")?,
            l2_gas: bound("l2_gas_consumed", "l2_gas_price")?,
            l1_data_gas: bound("l1_data_gas_consumed", "l1_data_gas_price")?,
        })
    }
}

const ZERO_BOUNDS: FeeBounds = FeeBounds {
    l1_gas: ResourceBounds {
        max_amount: 0,
        max_price_per_unit: 0,
    },
    l2_gas: ResourceBounds {
        max_amount: 0,
        max_price_per_unit: 0,
    },
    l1_data_gas: ResourceBounds {
        max_amount: 0,
        max_price_per_unit: 0,
    },
};

#[async_trait]
impl StarknetRpc for HttpStarknetRpc {
    async fn get_nonce(&self, address: &Felt) -> Result<Felt, NodeError> {
        let r = self
            .call("starknet_getNonce", json!([BLOCK_TAG, fh(address)]))
            .await?;
        Self::parse_felt(&r, "nonce")
    }

    async fn estimate_invoke(
        &self,
        sender: &Felt,
        calldata: &[Felt],
        nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        let tx = invoke_v3_tx_json(sender, calldata, &[], nonce, &ZERO_BOUNDS, &[], None);
        let r = self
            .call(
                "starknet_estimateFee",
                json!({ "request": [tx], "simulation_flags": ["SKIP_VALIDATE"], "block_id": BLOCK_TAG }),
            )
            .await?;
        let est = r
            .get(0)
            .ok_or_else(|| NodeError::Decode("empty estimate array".into()))?;
        // Spec 0.8 FEE_ESTIMATE fields. Amounts/prices get a safety margin.
        self.bounds_from_estimate(est)
    }

    async fn add_invoke(
        &self,
        sender: &Felt,
        calldata: &[Felt],
        signature: &[Felt],
        nonce: &Felt,
        bounds: &FeeBounds,
        proof_facts: &[Felt],
        proof: Option<&str>,
    ) -> Result<Felt, NodeError> {
        // SNIP-36 proof fields (when present) are attached by invoke_v3_tx_json.
        // NOTE: the exact SNIP-36 field names on a given node are not verified
        // here — see the module-level wire-format note.
        let tx = invoke_v3_tx_json(sender, calldata, signature, nonce, bounds, proof_facts, proof);
        let r = self
            .call("starknet_addInvokeTransaction", json!({ "invoke_transaction": tx }))
            .await?;
        Self::parse_felt(&r["transaction_hash"], "transaction_hash")
    }

    async fn is_deployed(&self, address: &Felt) -> Result<bool, NodeError> {
        // getClassHashAt succeeds for a deployed account; "contract not found"
        // (a normal Rpc error here) means counterfactual/undeployed.
        match self
            .call("starknet_getClassHashAt", json!([BLOCK_TAG, fh(address)]))
            .await
        {
            Ok(_) => Ok(true),
            Err(NodeError::Rpc(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn balance_of(&self, token: &Felt, holder: &Felt) -> Result<u128, NodeError> {
        // balanceOf returns a u256 as [low, high].
        let r = self
            .call(
                "starknet_call",
                json!({
                    "request": {
                        "contract_address": fh(token),
                        "entry_point_selector": fh(&wallet_core::get_selector_from_name("balanceOf")),
                        "calldata": [fh(holder)],
                    },
                    "block_id": BLOCK_TAG,
                }),
            )
            .await?;
        let low = r
            .get(0)
            .ok_or_else(|| NodeError::Decode("balanceOf: empty result".into()))?;
        let high_nonzero = r
            .get(1)
            .and_then(|v| v.as_str())
            .map(|s| Felt::from_hex(s).map(|f| f != Felt::ZERO).unwrap_or(false))
            .unwrap_or(false);
        if high_nonzero {
            return Ok(u128::MAX); // absurdly large; saturate rather than under-report
        }
        Self::parse_u128(low, "balance")
    }

    async fn estimate_deploy_account(
        &self,
        _address: &Felt,
        class_hash: &Felt,
        constructor_calldata: &[Felt],
        salt: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        let tx = Self::deploy_account_tx_json(class_hash, constructor_calldata, salt, &[], &ZERO_BOUNDS);
        let r = self
            .call(
                "starknet_estimateFee",
                json!({ "request": [tx], "simulation_flags": ["SKIP_VALIDATE"], "block_id": BLOCK_TAG }),
            )
            .await?;
        let est = r
            .get(0)
            .ok_or_else(|| NodeError::Decode("empty estimate array".into()))?;
        self.bounds_from_estimate(est)
    }

    async fn add_deploy_account(
        &self,
        class_hash: &Felt,
        constructor_calldata: &[Felt],
        salt: &Felt,
        signature: &[Felt],
        bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        let tx = Self::deploy_account_tx_json(class_hash, constructor_calldata, salt, signature, bounds);
        let r = self
            .call(
                "starknet_addDeployAccountTransaction",
                json!({ "deploy_account_transaction": tx }),
            )
            .await?;
        Self::parse_felt(&r["transaction_hash"], "transaction_hash")
    }

    async fn estimate_declare(
        &self,
        sender: &Felt,
        compiled_class_hash: &Felt,
        contract_class: &Value,
        nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        let tx = Self::declare_tx_json(sender, compiled_class_hash, contract_class, &[], nonce, &ZERO_BOUNDS);
        let r = self
            .call(
                "starknet_estimateFee",
                json!({ "request": [tx], "simulation_flags": ["SKIP_VALIDATE"], "block_id": BLOCK_TAG }),
            )
            .await?;
        let est = r
            .get(0)
            .ok_or_else(|| NodeError::Decode("empty estimate array".into()))?;
        self.bounds_from_estimate(est)
    }

    async fn add_declare(
        &self,
        sender: &Felt,
        compiled_class_hash: &Felt,
        contract_class: &Value,
        signature: &[Felt],
        nonce: &Felt,
        bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        let tx = Self::declare_tx_json(sender, compiled_class_hash, contract_class, signature, nonce, bounds);
        let r = self
            .call("starknet_addDeclareTransaction", json!({ "declare_transaction": tx }))
            .await?;
        Self::parse_felt(&r["transaction_hash"], "transaction_hash")
    }
}
