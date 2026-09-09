//! Dispatch tests for the STRK20 (Tongo) privacy methods — Phase 3.
//!
//! Fully offline: pool reads are served by a mock node whose `get_state`
//! answers with a REAL ElGamal encryption of a known balance under the test
//! seed's Tongo key, so decryption, proof generation and calldata assembly all
//! run for real. Uses the public BIP-39 test vector; no real key material.

use std::sync::Arc;

use async_trait::async_trait;
use krusty_kms_crypto::StarkCurve;
use serde_json::{json, Value};
use starknet_types_core::curve::ProjectivePoint;
use tokio::sync::Mutex;
use wallet_core::{
    address_hex, get_selector_from_name, oz_address, tongo_keypair, AccountRef, ChainId, Domain,
    Felt, Registry, ResourceBounds,
};
use wallet_rpc::{
    dispatch, AutoApprover, Decision, FeeBounds, NodeError, Request, Response, ServerState,
    StarknetRpc, WalletSession,
};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

const TOKEN: &str = "0x4718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";
const TX_HASH: &str = "0xcafe";

fn token() -> Felt {
    Felt::from_hex(TOKEN).unwrap()
}
fn pool() -> Felt {
    Felt::from_hex("0xdeadbeef01").unwrap()
}

/// ElGamal-encrypt `amount` under `key` with randomness r = 1:
/// `L = g·amount + key`, `R = g` — valid, decryptable, never at infinity.
fn cipher_felts(key: &ProjectivePoint, amount: u128) -> [Felt; 4] {
    let g = StarkCurve::generator();
    let gb = StarkCurve::mul(&Felt::from(amount), Some(&g));
    let l = StarkCurve::add(&gb, key);
    let la = l.to_affine().unwrap();
    let ga = g.to_affine().unwrap();
    [la.x(), la.y(), ga.x(), ga.y()]
}

/// Mock node: canned answers for the wrapping-tx path plus a Tongo pool that
/// reports the given balances for whoever asks (`get_state` ignores the key —
/// the encryption is bound to the test Tongo key below).
struct PoolNode {
    balance: u128,
    pending: u128,
    rate: u128,
    pool_nonce: u64,
}

impl PoolNode {
    fn state_felts(&self) -> Vec<Felt> {
        let kp = tongo_keypair(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
        let mut out = Vec::with_capacity(9);
        out.extend(cipher_felts(&kp.public_key, self.balance));
        out.extend(cipher_felts(&kp.public_key, self.pending));
        out.push(Felt::from(self.pool_nonce));
        out
    }
}

#[async_trait]
impl StarknetRpc for PoolNode {
    async fn get_nonce(&self, _address: &Felt) -> Result<Felt, NodeError> {
        Ok(Felt::from(1u64))
    }
    async fn estimate_invoke(
        &self,
        _sender: &Felt,
        _calldata: &[Felt],
        _nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(FeeBounds {
            l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 7 },
            l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 11 },
            l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 3 },
        })
    }
    async fn add_invoke(
        &self,
        _sender: &Felt,
        _calldata: &[Felt],
        _signature: &[Felt],
        _nonce: &Felt,
        _bounds: &FeeBounds,
        _proof_facts: &[Felt],
        _proof: Option<&str>,
    ) -> Result<Felt, NodeError> {
        Ok(Felt::from_hex(TX_HASH).unwrap())
    }
    async fn is_deployed(&self, _address: &Felt) -> Result<bool, NodeError> {
        Ok(true)
    }
    async fn balance_of(&self, _token: &Felt, _holder: &Felt) -> Result<u128, NodeError> {
        Ok(u128::MAX)
    }
    async fn estimate_deploy_account(
        &self,
        _address: &Felt,
        _class_hash: &Felt,
        _constructor_calldata: &[Felt],
        _salt: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Err(NodeError::Rpc("not used".into()))
    }
    async fn add_deploy_account(
        &self,
        _class_hash: &Felt,
        _constructor_calldata: &[Felt],
        _salt: &Felt,
        _signature: &[Felt],
        _bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        Err(NodeError::Rpc("not used".into()))
    }
    async fn estimate_declare(
        &self,
        _sender: &Felt,
        _compiled_class_hash: &Felt,
        _contract_class: &Value,
        _nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Err(NodeError::Rpc("not used".into()))
    }
    async fn add_declare(
        &self,
        _sender: &Felt,
        _compiled_class_hash: &Felt,
        _contract_class: &Value,
        _signature: &[Felt],
        _nonce: &Felt,
        _bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        Err(NodeError::Rpc("not used".into()))
    }
    async fn call_contract(
        &self,
        to: &Felt,
        selector: &Felt,
        _calldata: &[Felt],
    ) -> Result<Vec<Felt>, NodeError> {
        assert_eq!(to, &pool(), "only the registered pool should be read");
        let s = |name: &str| get_selector_from_name(name);
        if *selector == s("get_state") {
            Ok(self.state_felts())
        } else if *selector == s("get_rate") {
            Ok(vec![Felt::from(self.rate)])
        } else if *selector == s("get_bit_size") {
            Ok(vec![Felt::from(32u64)])
        } else if *selector == s("ERC20") {
            Ok(vec![token()])
        } else if *selector == s("auditor_key") {
            Ok(vec![Felt::ONE]) // Cairo Option::None
        } else {
            Err(NodeError::Rpc(format!("unexpected selector {selector:#x}")))
        }
    }
}

fn make_state(node: PoolNode) -> Arc<ServerState> {
    let addr = oz_address(TEST_MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap();
    let mut reg = Registry::default();
    reg.add(AccountRef {
        domain: Domain::User,
        index: 0,
        address: address_hex(&addr),
        label: "Main".into(),
        owner_client_id: None,
    });
    let session = WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "test-pass", reg);
    Arc::new(
        ServerState::new(Arc::new(Mutex::new(session)), Arc::new(AutoApprover(Decision::Approve)))
            .with_node(ChainId::Sepolia, Arc::new(node))
            .with_strk20_pool(ChainId::Sepolia, token(), pool()),
    )
}

async fn call(state: &ServerState, token: Option<&str>, method: &str, params: Value) -> Response {
    dispatch(
        state,
        token,
        Request { jsonrpc: Some("2.0".into()), method: method.into(), params, id: json!(1) },
    )
    .await
}

async fn pair(state: &ServerState) -> String {
    let resp = call(
        state,
        None,
        "companion_requestPairing",
        json!({"name": "strk20-tests", "kind": "app"}),
    )
    .await;
    resp.result.unwrap()["token"].as_str().unwrap().to_string()
}

fn err_code(resp: &Response) -> i64 {
    resp.error.as_ref().expect("expected error").code
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn balances_decrypts_pool_state_and_reports_smallest_units() {
    // 7 pool units spendable + 5 pending, at 2 smallest-units per pool unit.
    let state = make_state(PoolNode { balance: 7, pending: 5, rate: 2, pool_nonce: 3 });
    let t = pair(&state).await;

    let resp = call(&state, Some(&t), "wallet_strk20Balances", json!({"tokens": [TOKEN]})).await;
    let entries = resp.result.expect("balances result");
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert_eq!(entries[0]["token"], json!(TOKEN));
    assert_eq!(entries[0]["balance"], json!("0xe")); // 7 × 2
    assert_eq!(entries[0]["pending"], json!("0xa")); // 5 × 2

    // Empty tokens array = all registered shielded tokens (spec #391).
    let all = call(&state, Some(&t), "wallet_strk20Balances", json!({"tokens": []})).await;
    assert_eq!(all.result.unwrap().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn balances_for_unregistered_token_is_not_registered_118() {
    let state = make_state(PoolNode { balance: 0, pending: 0, rate: 1, pool_nonce: 0 });
    let t = pair(&state).await;
    let resp =
        call(&state, Some(&t), "wallet_strk20Balances", json!({"tokens": ["0xffff"]})).await;
    assert_eq!(err_code(&resp), 118);
}

#[tokio::test]
async fn prepare_deposit_builds_approve_plus_fund_with_empty_proof() {
    let state = make_state(PoolNode { balance: 0, pending: 0, rate: 2, pool_nonce: 0 });
    let t = pair(&state).await;

    let resp = call(
        &state,
        Some(&t),
        "wallet_strk20PrepareInvoke",
        json!({"actions": [{"type": "deposit", "token": TOKEN, "amount": "10"}]}),
    )
    .await;
    let r = resp.result.expect("prepare result");

    // Deposit = ERC-20 approve (on the token) + fund (on the pool).
    let calls = r["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["contract_address"], json!(TOKEN));
    assert_eq!(calls[1]["contract_address"].as_str().unwrap(), format!("0x{:x}", pool()));
    // Spec-shaped primary call + always-empty proof (Tongo proofs live in calldata).
    assert_eq!(r["call"], calls[1]);
    assert_eq!(r["proof"], json!({"data": "", "output": [], "proof_facts": []}));
    // Fund calldata: [to.x, to.y, amount(=10/2=5 units), hint(6), proof(3), audit tag]
    assert_eq!(calls[1]["calldata"][2], json!("0x5"));
}

#[tokio::test]
async fn invoke_transfer_submits_and_returns_hash() {
    let state = make_state(PoolNode { balance: 100, pending: 0, rate: 1, pool_nonce: 9 });
    let t = pair(&state).await;

    // Recipient: the agent-branch Tongo key of the same test seed (any valid
    // key works — it only has to be a curve point the wallet can encrypt to).
    let rk = tongo_keypair(TEST_MNEMONIC, Domain::Agent, 0, None).unwrap();
    let recipient = krusty_kms_client::pub_key_to_tongo_address(&rk.public_key).unwrap();

    let resp = call(
        &state,
        Some(&t),
        "wallet_strk20InvokeTransaction",
        json!({"actions": [
            {"type": "transfer", "token": TOKEN, "amount": "4", "recipient": recipient}
        ]}),
    )
    .await;
    let r = resp.result.expect("invoke result");
    assert_eq!(r["transaction_hash"], json!(TX_HASH));
    assert_eq!(r["submitted"], json!(true));
}

#[tokio::test]
async fn withdraw_beyond_balance_is_insufficient_119_and_mentions_rollover() {
    let state = make_state(PoolNode { balance: 3, pending: 8, rate: 1, pool_nonce: 0 });
    let t = pair(&state).await;
    let resp = call(
        &state,
        Some(&t),
        "wallet_strk20InvokeTransaction",
        json!({"actions": [
            {"type": "withdraw", "token": TOKEN, "amount": "10", "recipient": "0xabc"}
        ]}),
    )
    .await;
    assert_eq!(err_code(&resp), 119);
    // Pending funds exist, so the error must point at the rollover fix.
    assert!(resp.error.unwrap().message.contains("rollover"));
}

#[tokio::test]
async fn rollover_makes_one_call_and_zero_pending_is_rejected() {
    let state = make_state(PoolNode { balance: 3, pending: 8, rate: 1, pool_nonce: 0 });
    let t = pair(&state).await;
    let resp = call(
        &state,
        Some(&t),
        "wallet_strk20PrepareInvoke",
        json!({"actions": [{"type": "rollover", "token": TOKEN}]}),
    )
    .await;
    let r = resp.result.expect("rollover prepare");
    assert_eq!(r["calls"].as_array().unwrap().len(), 1);

    let none = make_state(PoolNode { balance: 3, pending: 0, rate: 1, pool_nonce: 0 });
    let t2 = pair(&none).await;
    let resp2 = call(
        &none,
        Some(&t2),
        "wallet_strk20PrepareInvoke",
        json!({"actions": [{"type": "rollover", "token": TOKEN}]}),
    )
    .await;
    assert_eq!(err_code(&resp2), 114);
}

#[tokio::test]
async fn unsupported_spec_surface_is_rejected_clearly() {
    let state = make_state(PoolNode { balance: 0, pending: 0, rate: 1, pool_nonce: 0 });
    let t = pair(&state).await;

    // Open notes (amount "OPEN") — Tongo has no note model.
    let open = call(
        &state,
        Some(&t),
        "wallet_strk20PrepareInvoke",
        json!({"actions": [
            {"type": "transfer", "token": TOKEN, "amount": "OPEN", "recipient": "x"}
        ]}),
    )
    .await;
    assert_eq!(err_code(&open), -32601);

    // invoke / subaccount_invoke actions.
    for ty in ["invoke", "subaccount_invoke"] {
        let resp = call(
            &state,
            Some(&t),
            "wallet_strk20PrepareInvoke",
            json!({"actions": [{"type": ty}]}),
        )
        .await;
        assert_eq!(err_code(&resp), -32601, "action {ty}");
    }

    // The sub-account commitment method stays deferred.
    let sub = call(&state, Some(&t), "wallet_strk20SubaccountCommitment", json!({})).await;
    assert_eq!(err_code(&sub), -32601);

    // A Starknet address as transfer recipient gets a targeted 114.
    let addr = call(
        &state,
        Some(&t),
        "wallet_strk20PrepareInvoke",
        json!({"actions": [
            {"type": "transfer", "token": TOKEN, "amount": "4", "recipient": "0xabc"}
        ]}),
    )
    .await;
    assert_eq!(err_code(&addr), 114);
    assert!(addr.error.unwrap().message.contains("Tongo"));

    // Two actions in one request: pool nonce would go stale — 114.
    let two = call(
        &state,
        Some(&t),
        "wallet_strk20InvokeTransaction",
        json!({"actions": [
            {"type": "deposit", "token": TOKEN, "amount": "2"},
            {"type": "deposit", "token": TOKEN, "amount": "2"}
        ]}),
    )
    .await;
    assert_eq!(err_code(&two), 114);
}
