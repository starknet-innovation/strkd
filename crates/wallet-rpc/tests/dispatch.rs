//! Dispatch-level tests for the wallet service — no HTTP, fully deterministic.
//!
//! Uses the public BIP-39 test vector. No real key material.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use wallet_core::{
    address_hex, declare_v3_hash, oz_address, public_key, AccountRef, ChainId, Domain, Felt,
    InvokeV3Params, Registry, ResourceBounds,
};
use wallet_rpc::{
    dispatch, AutoApprover, ChannelApprover, Decision, FeeBounds, NodeError, Request, Response,
    ServerState, StarknetRpc, WalletSession,
};

/// Deterministic test double for the Starknet node.
struct MockNode {
    nonce: Felt,
    hash: Felt,
    /// Addresses `is_deployed` reports as deployed. `state_with_node` seeds this
    /// with the user-root (manager) address, so funding's manager pre-check
    /// passes while freshly-derived agent accounts read as undeployed.
    deployed: Vec<Felt>,
    /// When true, `is_deployed` is true for every address (exercises the
    /// "already deployed" deploy pre-check).
    all_deployed: bool,
    /// What `balance_of` returns (fri).
    balance: u128,
}

#[async_trait]
impl StarknetRpc for MockNode {
    async fn get_nonce(&self, _address: &Felt) -> Result<Felt, NodeError> {
        Ok(self.nonce)
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
        Ok(self.hash)
    }
    async fn is_deployed(&self, address: &Felt) -> Result<bool, NodeError> {
        Ok(self.all_deployed || self.deployed.iter().any(|a| a == address))
    }
    async fn balance_of(&self, _token: &Felt, _holder: &Felt) -> Result<u128, NodeError> {
        Ok(self.balance)
    }
    async fn estimate_deploy_account(
        &self,
        _address: &Felt,
        _class_hash: &Felt,
        _constructor_calldata: &[Felt],
        _salt: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(FeeBounds {
            l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 7 },
            l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 11 },
            l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 3 },
        })
    }
    async fn add_deploy_account(
        &self,
        _class_hash: &Felt,
        _constructor_calldata: &[Felt],
        _salt: &Felt,
        _signature: &[Felt],
        _bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        Ok(self.hash)
    }
    async fn estimate_declare(
        &self,
        _sender: &Felt,
        _compiled_class_hash: &Felt,
        _contract_class: &serde_json::Value,
        _nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(FeeBounds {
            l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 7 },
            l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 11 },
            l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 3 },
        })
    }
    async fn add_declare(
        &self,
        _sender: &Felt,
        _compiled_class_hash: &Felt,
        _contract_class: &serde_json::Value,
        _signature: &[Felt],
        _nonce: &Felt,
        _bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        Ok(self.hash)
    }
    async fn tx_state(&self, _tx_hash: &Felt) -> Result<wallet_rpc::TxState, NodeError> {
        Ok(wallet_rpc::TxState::Accepted)
    }
}

/// State with a mock node attached (enables auto nonce/fee + broadcast). The
/// manager (user-root) account reads as **deployed** so funding's pre-check
/// passes; freshly-derived agent accounts read as undeployed.
fn state_with_node(decision: Decision, nonce: &str, hash: &str) -> Arc<ServerState> {
    let (_, root) = user_registry();
    state_with_node_opts(
        decision,
        nonce,
        hash,
        vec![Felt::from_hex(&root).unwrap()],
        false,
    )
}

/// Like [`state_with_node`] but with explicit `is_deployed` behaviour: addresses
/// in `deployed` (plus all addresses when `all_deployed`) report as deployed.
fn state_with_node_opts(
    decision: Decision,
    nonce: &str,
    hash: &str,
    deployed: Vec<Felt>,
    all_deployed: bool,
) -> Arc<ServerState> {
    let node = MockNode {
        nonce: Felt::from_hex(nonce).unwrap(),
        hash: Felt::from_hex(hash).unwrap(),
        deployed,
        all_deployed,
        balance: 1_000_000_000_000_000_000, // 1 STRK
    };
    Arc::new(
        ServerState::new(
            Arc::new(Mutex::new(make_session(false))),
            Arc::new(AutoApprover(decision)),
        )
        .with_node(ChainId::Sepolia, Arc::new(node)),
    )
}

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// A minimal valid SNIP-12 typed-data document.
fn sample_typed_data() -> Value {
    json!({
        "types": {
            "StarknetDomain": [
                {"name": "name", "type": "shortstring"},
                {"name": "version", "type": "shortstring"},
                {"name": "chainId", "type": "shortstring"},
                {"name": "revision", "type": "shortstring"}
            ],
            "Message": [
                {"name": "contents", "type": "felt"}
            ]
        },
        "primaryType": "Message",
        "domain": {"name": "strkd", "version": "1", "chainId": "SN_SEPOLIA", "revision": "1"},
        "message": {"contents": "0x1"}
    })
}

fn user_registry() -> (Registry, String) {
    let addr = oz_address(TEST_MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap();
    let address = address_hex(&addr);
    let mut reg = Registry::default();
    reg.add(AccountRef {
        domain: Domain::User,
        index: 0,
        address: address.clone(),
        label: "Main".into(),
        owner_client_id: None,
    });
    (reg, address)
}

fn make_session(locked: bool) -> WalletSession {
    let (reg, _) = user_registry();
    if locked {
        WalletSession::new_locked(ChainId::Sepolia)
    } else {
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "test-pass", reg)
    }
}

/// State whose approver returns the same decision for every prompt.
fn state_with(decision: Decision, locked: bool) -> Arc<ServerState> {
    Arc::new(ServerState::new(
        Arc::new(Mutex::new(make_session(locked))),
        Arc::new(AutoApprover(decision)),
    ))
}

/// State whose approver rejects only the listed methods and approves the rest.
/// Exercises the real `ChannelApprover` + a responder task.
fn state_rejecting(reject_methods: &'static [&'static str]) -> Arc<ServerState> {
    let (approver, mut rx) = ChannelApprover::new(16);
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let decision = if reject_methods.contains(&pending.request.method.as_str()) {
                Decision::Reject
            } else {
                Decision::Approve
            };
            let _ = pending.respond.send(decision);
        }
    });
    Arc::new(ServerState::new(
        Arc::new(Mutex::new(make_session(false))),
        Arc::new(approver),
    ))
}

async fn call(state: &ServerState, token: Option<&str>, method: &str, params: Value) -> Response {
    dispatch(
        state,
        token,
        Request {
            jsonrpc: Some("2.0".into()),
            method: method.into(),
            params,
            id: json!(1),
        },
    )
    .await
}

fn err_code(resp: &Response) -> i64 {
    resp.error.as_ref().expect("expected error").code
}

/// Pair a client of the given kind (auto-approved) and return its token.
async fn pair(state: &ServerState, kind: &str) -> String {
    let resp = call(
        state,
        None,
        "companion_requestPairing",
        json!({"name": format!("{kind}-client"), "kind": kind}),
    )
    .await;
    resp.result.unwrap()["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn public_methods_need_no_auth() {
    let state = state_with(Decision::Approve, false);
    let api = call(&state, None, "wallet_supportedWalletApi", json!({})).await;
    assert!(api.result.unwrap().is_array());

    let status = call(&state, None, "companion_getStatus", json!({})).await;
    let s = status.result.unwrap();
    assert_eq!(s["locked"], json!(false));
    assert_eq!(s["network"], json!("SN_SEPOLIA"));
}

#[tokio::test]
async fn unpaired_caller_is_rejected() {
    let state = state_with(Decision::Approve, false);
    let resp = call(&state, None, "wallet_requestAccounts", json!({})).await;
    assert_eq!(err_code(&resp), 118); // NOT_REGISTERED

    let resp_bad = call(&state, Some("garbage"), "wallet_requestAccounts", json!({})).await;
    assert_eq!(err_code(&resp_bad), 118);
}

#[tokio::test]
async fn get_permissions_reflects_pairing() {
    let state = state_with(Decision::Approve, false);
    let none = call(&state, None, "wallet_getPermissions", json!({})).await;
    assert_eq!(none.result.unwrap(), json!([]));

    let token = pair(&state, "app").await;
    let some = call(&state, Some(&token), "wallet_getPermissions", json!({})).await;
    assert_eq!(some.result.unwrap(), json!(["accounts"]));
}

#[tokio::test]
async fn app_client_sees_user_accounts() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "wallet_requestAccounts", json!({})).await;
    assert_eq!(resp.result.unwrap(), json!([address]));
}

#[tokio::test]
async fn agent_creates_and_sees_only_its_own_accounts() {
    let state = state_with(Decision::Approve, false);
    let agent = pair(&state, "agent").await;

    // No agent accounts yet.
    let before = call(&state, Some(&agent), "wallet_requestAccounts", json!({})).await;
    assert_eq!(before.result.unwrap(), json!([]));

    // Create one.
    let created = call(
        &state,
        Some(&agent),
        "companion_createAgentAccount",
        json!({"label": "bot"}),
    )
    .await;
    let acct = created.result.unwrap();
    assert_eq!(acct["domain"], json!("agent"));
    let agent_addr = acct["address"].as_str().unwrap().to_string();

    // The agent now sees exactly its account.
    let after = call(&state, Some(&agent), "companion_listAccounts", json!({})).await;
    let list = after.result.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["address"], json!(agent_addr));

    // A separate app client does NOT see the agent's account.
    let app = pair(&state, "app").await;
    let app_view = call(&state, Some(&app), "wallet_requestAccounts", json!({})).await;
    let app_list = app_view.result.unwrap();
    assert!(!app_list
        .as_array()
        .unwrap()
        .iter()
        .any(|v| *v == json!(agent_addr)));
}

#[tokio::test]
async fn app_client_cannot_create_agent_accounts() {
    let state = state_with(Decision::Approve, false);
    let app = pair(&state, "app").await;
    let resp = call(&state, Some(&app), "companion_createAgentAccount", json!({})).await;
    assert_eq!(err_code(&resp), -32002); // Forbidden
}

#[tokio::test]
async fn sign_typed_data_returns_signature_when_approved() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_signTypedData",
        json!({"account_address": address, "typed_data": sample_typed_data()}),
    )
    .await;
    let sig = resp.result.expect("expected signature");
    let arr = sig.as_array().unwrap();
    assert_eq!(arr.len(), 2); // [r, s]
    assert!(arr[0].as_str().unwrap().starts_with("0x"));
}

/// strkd #7 regression: `companion_typedDataHash` reports the exact SNIP-12
/// rev-1 digest that `wallet_signTypedData` signs, and the account's own public
/// key validates the `[r, s]` over that digest — i.e. the account's on-chain
/// `is_valid_signature(hash, [r, s])` would accept it. Before the krusty-kms
/// prefix fix (short-string 'StarkNet Message', not its keccak) the signed
/// digest matched no SNIP-12 verifier, so this whole flow was impossible.
#[tokio::test]
async fn typed_data_hash_is_the_signed_digest_and_verifies_under_account_key() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let td = sample_typed_data();

    // The hash strkd would sign (no prompt, no key).
    let hash_resp = call(
        &state,
        Some(&token),
        "companion_typedDataHash",
        json!({"account_address": address, "typed_data": td.clone()}),
    )
    .await;
    let hres = hash_resp.result.expect("expected hash");
    assert_eq!(hres["revision"], "1");
    let hash = Felt::from_hex(hres["hash"].as_str().unwrap()).unwrap();

    // The signature over the same typed data.
    let sig_resp = call(
        &state,
        Some(&token),
        "wallet_signTypedData",
        json!({"account_address": address, "typed_data": td}),
    )
    .await;
    let arr = sig_resp.result.expect("expected signature");
    let arr = arr.as_array().unwrap();
    let r = Felt::from_hex(arr[0].as_str().unwrap()).unwrap();
    let s = Felt::from_hex(arr[1].as_str().unwrap()).unwrap();

    // The account's own key must validate [r, s] over the reported hash — the
    // on-chain is_valid_signature check, done locally with a test seed.
    let pk = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    assert!(
        starknet_crypto::verify(&pk, &hash, &r, &s).unwrap(),
        "signature must verify against the reported SNIP-12 digest under the account key",
    );

    // Guard the pin against SNIP-12 regressions: this exact digest was
    // cross-checked against starknet.py TypedData.message_hash (== starknet.js
    // and on-chain is_valid_signature). If the krusty pin regressed (keccak
    // prefix, or shortstring not going through parse_felt), this would change.
    assert_eq!(
        hres["hash"].as_str().unwrap(),
        "0x68b4250d022dce3e45e64683935b0e0f8bf95e3dbf17eb9839e255578ddc061",
        "SNIP-12 rev-1 digest changed — check the krusty-kms pin",
    );
}

#[tokio::test]
async fn typed_data_hash_needs_no_unlock() {
    // Pure hash: available while locked (it never touches the vault). The
    // approver only gates pairing here; typedDataHash itself prompts nothing.
    let state = state_with(Decision::Approve, true);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "companion_typedDataHash",
        json!({"account_address": address, "typed_data": sample_typed_data()}),
    )
    .await;
    let res = resp.result.expect("hash works while locked");
    assert!(res["hash"].as_str().unwrap().starts_with("0x"));
}

#[tokio::test]
async fn sign_typed_data_rejected_maps_to_113() {
    // Pairing is approved; only the sign prompt is rejected.
    let state = state_rejecting(&["wallet_signTypedData"]);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_signTypedData",
        json!({"account_address": address, "typed_data": sample_typed_data()}),
    )
    .await;
    assert_eq!(err_code(&resp), 113); // USER_REFUSED_OP
}

#[tokio::test]
async fn agent_cannot_sign_with_a_user_account() {
    let state = state_with(Decision::Approve, false);
    let (_, user_address) = user_registry();
    let agent = pair(&state, "agent").await;
    // The user account is out of the agent's scope.
    let resp = call(
        &state,
        Some(&agent),
        "wallet_signTypedData",
        json!({"account_address": user_address, "typed_data": sample_typed_data()}),
    )
    .await;
    assert_eq!(err_code(&resp), -32002); // Forbidden
}

#[tokio::test]
async fn deferred_methods_report_not_implemented() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addStarknetChain",
        json!({}),
    )
    .await;
    assert_eq!(err_code(&resp), -32601);
}

/// A well-formed sign-only invoke request for the user account.
fn invoke_params(address: &str) -> Value {
    json!({
        "account_address": address,
        "calls": [{
            "contract_address": "0x49d36",
            "entry_point_selector": "transfer",
            "calldata": ["0xdead", "0x2710", "0x0"]
        }],
        "nonce": "0x0",
        "resource_bounds": {
            "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
            "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
            "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
        }
    })
}

#[tokio::test]
async fn add_invoke_sign_only_returns_signed_payload() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addInvokeTransaction",
        invoke_params(&address),
    )
    .await;
    let r = resp.result.expect("expected signed payload");
    assert_eq!(r["submitted"], json!(false));
    assert!(r["transaction_hash"].as_str().unwrap().starts_with("0x"));
    assert_eq!(r["signature"].as_array().unwrap().len(), 2);
    // Encoded calldata: [n_calls, to, selector, calldata_len, ...3] = 7 felts.
    assert_eq!(
        r["signed_transaction"]["calldata"].as_array().unwrap().len(),
        7
    );
    assert_eq!(r["signed_transaction"]["version"], json!("0x3"));
}

#[tokio::test]
async fn add_invoke_accepts_entrypoint_name_alias() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    // Use `entrypoint` with a function NAME (starknet.js style) instead of
    // `entry_point_selector` with a hashed selector.
    let p = json!({
        "account_address": address,
        "calls": [{ "contractAddress": "0x49d36", "entrypoint": "transfer",
                    "calldata": ["0xdead", "0x2710", "0x0"] }],
        "nonce": "0x0",
        "resource_bounds": {
            "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
            "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
            "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
        }
    });
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    let r = resp.result.expect("entrypoint-name alias should be accepted");
    assert_eq!(r["submitted"], json!(false));
    assert_eq!(r["signed_transaction"]["calldata"].as_array().unwrap().len(), 7);
}

#[tokio::test]
async fn deploy_account_sign_only_returns_deploy_tx() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    // App client deploys a user account; supply bounds so no node is needed.
    let (_, address) = user_registry();
    let p = json!({
        "account": address,
        "resource_bounds": {
            "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
            "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
            "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
        }
    });
    let resp = call(&state, Some(&token), "companion_deployAccount", p).await;
    let r = resp.result.expect("signed deploy tx");
    assert_eq!(r["submitted"], json!(false));
    assert_eq!(r["contract_address"], json!(address));
    assert_eq!(r["signed_transaction"]["type"], json!("DEPLOY_ACCOUNT"));
    assert_eq!(r["signature"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn deploy_account_submit_with_node_broadcasts() {
    let state = state_with_node(Decision::Approve, "0x0", "0xdeed");
    let agent = pair(&state, "agent").await;
    // Agent creates an account, then deploys it (auto fee estimate + broadcast).
    let created = call(&state, Some(&agent), "companion_createAgentAccount", json!({"label": "bot"})).await;
    let _ = created.result.unwrap();
    let resp = call(&state, Some(&agent), "companion_deployAccount", json!({ "submit": true })).await;
    let r = resp.result.expect("broadcast deploy");
    assert_eq!(r["submitted"], json!(true));
    assert_eq!(r["transaction_hash"], json!("0xdeed"));
}

/// The minimal Counter class compiled with scarb 2.18.0 (same artifact the
/// wallet-core class-hash tests pin); its hash is the value starknet.js
/// `computeSierraContractClassHash` derives, which is what a node derives too.
const COUNTER_CLASS: &str =
    include_str!("../../wallet-core/tests/fixtures/minimal_counter.contract_class.json");
const COUNTER_CLASS_HASH: &str =
    "0x50b7a36b2af957551f6b829a690666c0f212a324dbc8ccff325e38a1e0843a5";

fn counter_class() -> Value {
    serde_json::from_str(COUNTER_CLASS).unwrap()
}

#[tokio::test]
async fn add_declare_sign_only_returns_a_broadcastable_transaction() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    // Hash-only signing: class_hash + compiled_class_hash + bounds, no class.
    let p = json!({
        "account_address": address,
        "class_hash": "0x1234",
        "compiled_class_hash": "0x5678",
        "nonce": "0x0",
        "resource_bounds": {
            "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
            "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
            "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
        }
    });
    let resp = call(&state, Some(&token), "wallet_addDeclareTransaction", p).await;
    let r = resp.result.expect("signed declare");
    assert_eq!(r["submitted"], json!(false));
    assert_eq!(r["class_hash"], json!("0x1234"));
    assert_eq!(r["signature"].as_array().unwrap().len(), 2);

    // strkd #9: the sign-only transaction must be COMPLETE — every field the
    // RPC's BROADCASTED_DECLARE_TXN_V3 requires, signature included — not the
    // handful of fields the caller happened to pass in.
    let tx = &r["signed_transaction"];
    assert_eq!(tx["type"], json!("DECLARE"));
    assert_eq!(tx["version"], json!("0x3"));
    assert_eq!(
        Felt::from_hex(tx["sender_address"].as_str().unwrap()).unwrap(),
        Felt::from_hex(&address).unwrap()
    );
    assert_eq!(tx["compiled_class_hash"], json!("0x5678"));
    assert_eq!(tx["nonce"], json!("0x0"));
    assert_eq!(tx["signature"], r["signature"]);
    assert_eq!(tx["tip"], json!("0x0"));
    assert_eq!(tx["paymaster_data"], json!([]));
    assert_eq!(tx["account_deployment_data"], json!([]));
    assert_eq!(tx["nonce_data_availability_mode"], json!("L1"));
    assert_eq!(tx["fee_data_availability_mode"], json!("L1"));
    assert_eq!(tx["resource_bounds"]["l2_gas"]["max_amount"], json!("0xf4240"));
    // No class was supplied, so none is echoed (the caller splices theirs in).
    assert!(tx.get("contract_class").is_none());
}

#[tokio::test]
async fn add_declare_signs_the_hash_the_account_will_validate() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let p = json!({
        "account_address": address,
        "contract_class": counter_class(),
        "compiled_class_hash": "0x5678",
        "nonce": "0x7",
        "resource_bounds": {
            "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
            "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x2"},
            "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x3"}
        }
    });
    let resp = call(&state, Some(&token), "wallet_addDeclareTransaction", p).await;
    let r = resp.result.expect("signed declare");

    // The class hash is DERIVED from the class (the node does the same), and the
    // echoed transaction carries the canonical RPC CONTRACT_CLASS.
    assert_eq!(r["class_hash"], json!(COUNTER_CLASS_HASH));
    let tx = &r["signed_transaction"];
    assert!(tx["contract_class"]["abi"].is_string());
    assert!(tx["contract_class"].get("sierra_program_debug_info").is_none());

    // The reported tx hash is the canonical DECLARE_V3 hash over that class
    // hash, and the account's own key validates the signature over it — the
    // on-chain __validate_declare__ check, done locally with a test seed
    // (strkd #9: this is what was failing as "invalid signature" on Sepolia).
    let expected = declare_v3_hash(
        &Felt::from_hex(&address).unwrap(),
        &Felt::from_hex(COUNTER_CLASS_HASH).unwrap(),
        &Felt::from_hex("0x5678").unwrap(),
        ChainId::Sepolia,
        &InvokeV3Params {
            nonce: Felt::from(7u64),
            l1_gas: ResourceBounds { max_amount: 0x3e8, max_price_per_unit: 0x1 },
            l2_gas: ResourceBounds { max_amount: 0xf4240, max_price_per_unit: 0x2 },
            l1_data_gas: ResourceBounds { max_amount: 0x3e8, max_price_per_unit: 0x3 },
            ..Default::default()
        },
    );
    let reported = Felt::from_hex(r["transaction_hash"].as_str().unwrap()).unwrap();
    assert_eq!(reported, expected);

    let sig = r["signature"].as_array().unwrap();
    let (rr, ss) = (
        Felt::from_hex(sig[0].as_str().unwrap()).unwrap(),
        Felt::from_hex(sig[1].as_str().unwrap()).unwrap(),
    );
    let pk = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    assert!(
        starknet_crypto::verify(&pk, &reported, &rr, &ss).unwrap(),
        "signature must verify against the declare hash under the account key",
    );
}

#[tokio::test]
async fn add_declare_rejects_a_class_hash_that_contradicts_the_class() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addDeclareTransaction",
        json!({
            "account_address": address,
            "class_hash": "0xdead",
            "contract_class": counter_class(),
            "compiled_class_hash": "0x5678",
            "nonce": "0x0",
            "resource_bounds": {
                "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
                "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
                "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
            }
        }),
    )
    .await;
    // Refused up front with both hashes named, rather than signing something
    // the node would reject as an invalid signature.
    assert_eq!(err_code(&resp), 114);
    let msg = resp.error.unwrap().message;
    assert!(msg.contains("0xdead"), "{msg}");
    assert!(msg.contains(COUNTER_CLASS_HASH), "{msg}");
}

#[tokio::test]
async fn add_declare_agreeing_class_hash_is_accepted() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addDeclareTransaction",
        json!({
            "account_address": address,
            "class_hash": COUNTER_CLASS_HASH,
            "contract_class": counter_class(),
            "compiled_class_hash": "0x5678",
            "nonce": "0x0",
            "resource_bounds": {
                "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
                "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
                "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
            }
        }),
    )
    .await;
    assert_eq!(resp.result.expect("signed declare")["class_hash"], json!(COUNTER_CLASS_HASH));
}

#[tokio::test]
async fn add_declare_submit_with_node_broadcasts() {
    let state = state_with_node(Decision::Approve, "0x0", "0xdec1a");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    // submit:true needs contract_class; node auto-estimates + broadcasts.
    let p = json!({
        "account_address": address,
        "compiled_class_hash": "0x5678",
        "contract_class": counter_class(),
        "submit": true
    });
    let resp = call(&state, Some(&token), "wallet_addDeclareTransaction", p).await;
    let r = resp.result.expect("broadcast declare");
    assert_eq!(r["submitted"], json!(true));
    assert_eq!(r["transaction_hash"], json!("0xdec1a"));
    assert_eq!(r["class_hash"], json!(COUNTER_CLASS_HASH));
}

#[tokio::test]
async fn add_declare_without_class_or_class_hash_is_114() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addDeclareTransaction",
        json!({"account_address": address, "compiled_class_hash": "0x5678", "nonce": "0x0"}),
    )
    .await;
    assert_eq!(err_code(&resp), 114);
    assert!(resp.error.unwrap().message.contains("class_hash"));
}

#[tokio::test]
async fn add_declare_malformed_contract_class_is_114_with_a_reason() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addDeclareTransaction",
        json!({
            "account_address": address,
            "compiled_class_hash": "0x5678",
            // The compressed gateway form, not the RPC CONTRACT_CLASS object.
            "contract_class": {"sierra_program": "H4sIAAAAAAAA", "abi": "[]"},
            "nonce": "0x0"
        }),
    )
    .await;
    assert_eq!(err_code(&resp), 114);
    let msg = resp.error.unwrap().message;
    assert!(msg.contains("contract_class") && msg.contains("compressed"), "{msg}");
}

#[tokio::test]
async fn deploy_account_without_node_or_bounds_errors() {
    let state = state_with(Decision::Approve, false); // no node
    let token = pair(&state, "app").await;
    // No resource_bounds and no node → can't estimate.
    let resp = call(&state, Some(&token), "companion_deployAccount", json!({})).await;
    assert_eq!(err_code(&resp), -32005); // NoNode
}

#[tokio::test]
async fn add_invoke_with_proof_facts_changes_hash_and_echoes() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    let plain = call(&state, Some(&token), "wallet_addInvokeTransaction", invoke_params(&address)).await;
    let plain_hash = plain.result.unwrap()["transaction_hash"].as_str().unwrap().to_string();

    // Same tx + proof_facts → different (SNIP-36 extended) hash, and the facts
    // are echoed back for the caller to assemble a broadcast.
    let mut p = invoke_params(&address);
    p["proof_facts"] = json!(["0x1", "0x2", "0x3"]);
    let proofed = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    let r = proofed.result.expect("signed proof-carrying invoke");
    assert_ne!(r["transaction_hash"].as_str().unwrap(), plain_hash, "proof_facts must extend the hash");
    // proof_facts live inside the complete signed_transaction.
    assert_eq!(r["signed_transaction"]["proof_facts"], json!(["0x1", "0x2", "0x3"]));
    assert_eq!(r["submitted"], json!(false));
}

#[tokio::test]
async fn add_invoke_submit_proof_carrying_requires_proof() {
    let state = state_with_node(Decision::Approve, "0x0", "0xc0ffee");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["submit"] = json!(true);
    p["proof_facts"] = json!(["0x1"]);
    // No `proof` → 114.
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p.clone()).await;
    assert_eq!(err_code(&resp), 114);

    // With `proof` → broadcasts (mock node returns its hash).
    p["proof"] = json!("BASE64PROOF==");
    let ok = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(ok.result.unwrap()["submitted"], json!(true));
}

#[tokio::test]
async fn add_invoke_proof_normalizes_whitespace() {
    // Prover CLI artifacts are often newline-terminated / line-wrapped. strkd
    // strips ASCII whitespace so the broadcast carries clean base64 (a stray
    // newline is a likely cause of node error 69 'proof field invalid').
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    let mut p = invoke_params(&address);
    p["proof_facts"] = json!(["0x1"]);
    p["proof"] = json!("  QUJD\nREVG  ");
    let r = call(&state, Some(&token), "wallet_addInvokeTransaction", p)
        .await
        .result
        .expect("signed proof-carrying invoke");
    assert_eq!(r["signed_transaction"]["proof"], json!("QUJDREVG"));
}

#[tokio::test]
async fn add_invoke_rejects_non_standard_base64_proof() {
    // A url-safe-alphabet ('-'/'_') or otherwise malformed proof is rejected up
    // front (114) rather than round-tripping to an opaque node rejection.
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    let mut p = invoke_params(&address);
    p["proof_facts"] = json!(["0x1"]);
    p["proof"] = json!("abc-def_");
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&resp), 114);
}

#[tokio::test]
async fn add_invoke_proof_carrying_without_bounds_refuses_estimation() {
    // A proof-carrying invoke must carry explicit resource_bounds. Auto fee
    // estimation simulates the call without proof_facts in tx_info, so a contract
    // reading them (e.g. proof_facts.at(8)) reverts during estimation. A node is
    // configured here, so this proves strkd refuses to estimate rather than
    // merely lacking a node to estimate against.
    let state = state_with_node(Decision::Approve, "0x0", "0xc0ffee");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    let mut p = invoke_params(&address);
    p.as_object_mut().unwrap().remove("resource_bounds");
    p["proof_facts"] = json!(["0x1", "0x2", "0x3"]);

    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&resp), 114);
}

#[tokio::test]
async fn add_invoke_per_request_chain_id_changes_hash() {
    // A per-request chainId picks the network without mutating the shared default.
    // Since chain_id is bound into the V3 hash, switching it must change the
    // signed hash; and passing the default explicitly must match omitting it.
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    // Default network (Sepolia).
    let base = call(&state, Some(&token), "wallet_addInvokeTransaction", invoke_params(&address)).await;
    let base_hash = base.result.unwrap()["transaction_hash"].as_str().unwrap().to_string();

    // Explicit chainId = SN_MAIN → different signed hash.
    let mut mainnet = invoke_params(&address);
    mainnet["chainId"] = json!("0x534e5f4d41494e");
    let r = call(&state, Some(&token), "wallet_addInvokeTransaction", mainnet).await;
    let mainnet_hash = r.result.expect("signed on mainnet")["transaction_hash"].as_str().unwrap().to_string();
    assert_ne!(mainnet_hash, base_hash, "per-request chainId must flow into the signed hash");

    // Explicit chainId = the default (SN_SEPOLIA) → same hash as omitting it.
    let mut sep = invoke_params(&address);
    sep["chainId"] = json!("0x534e5f5345504f4c4941");
    let r2 = call(&state, Some(&token), "wallet_addInvokeTransaction", sep).await;
    assert_eq!(r2.result.unwrap()["transaction_hash"].as_str().unwrap(), base_hash);
}

#[tokio::test]
async fn add_invoke_unsupported_chain_id_maps_to_117() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["chainId"] = json!("0x1234"); // not SN_SEPOLIA / SN_MAIN
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&resp), 117);
}

#[tokio::test]
async fn add_invoke_malformed_chain_id_maps_to_114() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["chainId"] = json!("not-hex");
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&resp), 114);
}

#[tokio::test]
async fn add_invoke_rejected_maps_to_113() {
    let state = state_rejecting(&["wallet_addInvokeTransaction"]);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_addInvokeTransaction",
        invoke_params(&address),
    )
    .await;
    assert_eq!(err_code(&resp), 113);
}

#[tokio::test]
async fn add_invoke_submit_without_node_errors() {
    let state = state_with(Decision::Approve, false); // no node configured
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["submit"] = json!(true);
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&resp), -32005); // NoNode
}

#[tokio::test]
async fn add_invoke_submit_with_node_broadcasts() {
    let state = state_with_node(Decision::Approve, "0x0", "0xdeadbeef");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["submit"] = json!(true);
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    let r = resp.result.expect("broadcast result");
    assert_eq!(r["submitted"], json!(true));
    // Returns the node's tx hash.
    assert_eq!(r["transaction_hash"], json!("0xdeadbeef"));
}

#[tokio::test]
async fn add_invoke_auto_nonce_and_fee_when_omitted() {
    let state = state_with_node(Decision::Approve, "0x7", "0xabc");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    // No nonce, no resource_bounds → node supplies them; sign-only.
    let p = json!({
        "account_address": address,
        "calls": [{ "contract_address": "0x49d36", "entry_point_selector": "transfer",
                    "calldata": ["0xdead", "0x2710", "0x0"] }]
    });
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    let r = resp.result.expect("signed payload with auto nonce/fee");
    assert_eq!(r["submitted"], json!(false));
    // The node's nonce (0x7) was used.
    assert_eq!(r["signed_transaction"]["nonce"], json!("0x7"));
    // signed_transaction is a complete, canonical-hex INVOKE_TXN_V3: estimated
    // bounds (l2 max_amount 200000 = 0x30d40) appear as hex, plus all v3 fields.
    let tx = &r["signed_transaction"];
    assert_eq!(tx["resource_bounds"]["l2_gas"]["max_amount"], json!("0x30d40"));
    assert_eq!(tx["nonce_data_availability_mode"], json!("L1"));
    assert!(tx["signature"].as_array().unwrap().len() == 2);
    assert_eq!(tx["tip"], json!("0x0"));
}

#[tokio::test]
async fn request_grant_approved_then_status_shows_active() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "agent").await;
    let resp = call(&state, Some(&token), "companion_requestGrant", json!({"days": 30})).await;
    let r = resp.result.expect("grant request approved");
    assert_eq!(r["granted"], json!(true));
    assert!(r["expires_at"].as_u64().unwrap() > wallet_rpc::now_unix_ms());

    // getStatus now reports the active grant.
    let s = call(&state, Some(&token), "companion_getStatus", json!({})).await;
    assert_eq!(s.result.unwrap()["grant"]["active"], json!(true));
}

#[tokio::test]
async fn request_grant_always_prompts_even_under_active_grant() {
    // Reject approver; an existing grant must NOT auto-approve a grant request
    // (escalation always prompts).
    let state = state_rejecting(&["companion_requestGrant"]);
    let token = pair(&state, "agent").await;
    let id = state.clients.lock().await.list().into_iter()
        .find(|c| c.label == "agent-client").map(|c| c.id).unwrap();
    grant_forever(&state, &id).await; // already granted

    let resp = call(&state, Some(&token), "companion_requestGrant", json!({"days": 90})).await;
    assert_eq!(err_code(&resp), 113, "requesting a grant must always prompt");
}

#[tokio::test]
async fn dispatch_records_activity_for_autolock() {
    let state = state_with(Decision::Approve, false);
    let before = state.last_activity_ms();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    // Any RPC request counts as activity (resets the auto-lock idle timer).
    call(&state, None, "companion_getStatus", json!({})).await;
    assert!(state.last_activity_ms() > before, "an RPC request should record activity");
}

#[tokio::test]
async fn estimate_fee_returns_canonical_hex_bounds() {
    let state = state_with_node(Decision::Approve, "0x9", "0xabc");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "companion_estimateFee",
        json!({
            "account_address": address,
            "calls": [{ "contract_address": "0x49d36", "entrypoint": "transfer",
                        "calldata": ["0xdead", "0x2710", "0x0"] }]
        }),
    )
    .await;
    let r = resp.result.expect("estimate");
    assert_eq!(r["nonce"], json!("0x9")); // from the mock node
    assert_eq!(r["resource_bounds"]["l2_gas"]["max_amount"], json!("0x30d40")); // 200000 hex
    // No prompt was needed (estimate doesn't sign) and no node → error.
}

#[tokio::test]
async fn estimate_fee_without_node_errors() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "companion_estimateFee",
        json!({ "account_address": address, "calls": [{ "contract_address": "0x49d36", "entrypoint": "transfer", "calldata": [] }] }),
    )
    .await;
    assert_eq!(err_code(&resp), -32005);
}

#[tokio::test]
async fn get_status_reports_grant_state() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "agent").await;
    let id = state.clients.lock().await.list().into_iter()
        .find(|c| c.label == "agent-client").map(|c| c.id).unwrap();

    // Unauthenticated getStatus → grant is null.
    let anon = call(&state, None, "companion_getStatus", json!({})).await;
    assert_eq!(anon.result.unwrap()["grant"], json!(null));

    // With token, no grant yet → active:false.
    let s1 = call(&state, Some(&token), "companion_getStatus", json!({})).await;
    assert_eq!(s1.result.unwrap()["grant"]["active"], json!(false));

    // After a grant → active:true with an expiry.
    grant_forever(&state, &id).await;
    let s2 = call(&state, Some(&token), "companion_getStatus", json!({})).await;
    let g = s2.result.unwrap();
    assert_eq!(g["grant"]["active"], json!(true));
    assert!(g["grant"]["expires_at"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn reattach_keeps_the_clients_accounts() {
    let state = state_with(Decision::Approve, false);
    // First pairing as "bot": create an agent account.
    let (token1, _id1, addr) = agent_with_account_and_id(&state).await;
    // Note the original client only saw its own account:
    let before = call(&state, Some(&token1), "wallet_requestAccounts", json!({})).await;
    assert_eq!(before.result.unwrap(), json!([addr]));

    // Re-pair with the SAME name + reattach:true → new token, same scope.
    let re = call(
        &state,
        None,
        "companion_requestPairing",
        json!({"name": "bot", "kind": "agent", "reattach": true}),
    )
    .await;
    let rr = re.result.expect("reattach");
    assert_eq!(rr["reattached"], json!(true));
    let token2 = rr["token"].as_str().unwrap().to_string();
    assert_ne!(token2, token1, "a fresh token is issued");

    // The re-attached client sees the SAME account (not stranded).
    let after = call(&state, Some(&token2), "wallet_requestAccounts", json!({})).await;
    assert_eq!(after.result.unwrap(), json!([addr]));
}

#[tokio::test]
async fn node_can_be_set_and_cleared_at_runtime() {
    // Start with no node → submit fails.
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["submit"] = json!(true);
    let before = call(&state, Some(&token), "wallet_addInvokeTransaction", p.clone()).await;
    assert_eq!(err_code(&before), -32005);
    assert!(!state.has_node_for(ChainId::Sepolia));

    // Attach a node at runtime (as the settings panel would) → submit broadcasts.
    state.set_node(
        ChainId::Sepolia,
        Some(Arc::new(MockNode {
            nonce: Felt::from_hex("0x0").unwrap(),
            hash: Felt::from_hex("0xcafe").unwrap(),
            deployed: vec![],
            all_deployed: false,
            balance: 1_000_000_000_000_000_000,
        })),
    );
    assert!(state.has_node_for(ChainId::Sepolia));
    let after = call(&state, Some(&token), "wallet_addInvokeTransaction", p.clone()).await;
    assert_eq!(after.result.unwrap()["transaction_hash"], json!("0xcafe"));

    // Clear it again → back to NoNode.
    state.set_node(ChainId::Sepolia, None);
    let cleared = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&cleared), -32005);
}

#[tokio::test]
async fn switch_chain_changes_active_network() {
    let state = state_with(Decision::Approve, false); // starts on Sepolia
    let token = pair(&state, "app").await;

    // SN_MAIN felt.
    let sn_main = format!("0x{:x}", wallet_core::Felt::from_bytes_be_slice(b"SN_MAIN"));
    let resp = call(
        &state,
        Some(&token),
        "wallet_switchStarknetChain",
        json!({ "chainId": sn_main }),
    )
    .await;
    assert_eq!(resp.result.unwrap(), json!(true));

    let status = call(&state, None, "companion_getStatus", json!({})).await;
    assert_eq!(status.result.unwrap()["network"], json!("SN_MAIN"));
}

#[tokio::test]
async fn switch_chain_rejects_unknown_chain() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_switchStarknetChain",
        json!({ "chainId": "0x1234" }),
    )
    .await;
    assert_eq!(err_code(&resp), 117); // CHAIN_ID_NOT_SUPPORTED
}

#[tokio::test]
async fn switch_chain_rejected_maps_to_113() {
    let state = state_rejecting(&["wallet_switchStarknetChain"]);
    let token = pair(&state, "app").await;
    let sn_main = format!("0x{:x}", wallet_core::Felt::from_bytes_be_slice(b"SN_MAIN"));
    let resp = call(&state, Some(&token), "wallet_switchStarknetChain", json!({ "chainId": sn_main })).await;
    assert_eq!(err_code(&resp), 113);
}

#[tokio::test]
async fn watch_asset_records_and_returns_true() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    let resp = call(
        &state,
        Some(&token),
        "wallet_watchAsset",
        json!({ "asset": { "address": "0x49d36", "symbol": "FOO" } }),
    )
    .await;
    assert_eq!(resp.result.unwrap(), json!(true));
    assert_eq!(state.watched_assets().len(), 1);
}

/// Grant `client_id` an auto-approval window far in the future.
async fn grant_forever(state: &ServerState, client_id: &str) {
    let until = wallet_rpc::now_unix_ms() + 90 * 24 * 60 * 60 * 1000;
    assert!(state.clients.lock().await.grant(client_id, until));
}

/// Pair an agent, create an account, and return (token, client_id, account_addr).
async fn agent_with_account_and_id(state: &ServerState) -> (String, String, String) {
    let resp = call(
        state,
        None,
        "companion_requestPairing",
        json!({"name": "bot", "kind": "agent"}),
    )
    .await;
    let r = resp.result.unwrap();
    let token = r["token"].as_str().unwrap().to_string();
    let id = r["client_id"].as_str().unwrap().to_string();
    let created = call(state, Some(&token), "companion_createAgentAccount", json!({"label": "a"})).await;
    let addr = created.result.unwrap()["address"].as_str().unwrap().to_string();
    (token, id, addr)
}

#[tokio::test]
async fn active_grant_auto_approves_agent_signing() {
    // Approver would REJECT every prompt — so success proves no prompt happened.
    let state = state_rejecting(&[
        "wallet_signTypedData",
        "wallet_addInvokeTransaction",
        "companion_createAgentAccount",
    ]);
    // Create the account before granting (creation would be rejected otherwise),
    // so pair+create via an approve path: use a separate approve helper.
    let agent = pair(&state, "agent").await;
    // Grant first so createAgentAccount + sign auto-approve.
    let client_id = {
        // Recover the client id by listing.
        let list = state.clients.lock().await.list();
        list.into_iter().find(|c| c.label == "agent-client").map(|c| c.id).unwrap()
    };
    grant_forever(&state, &client_id).await;

    let created = call(&state, Some(&agent), "companion_createAgentAccount", json!({"label": "a"})).await;
    let addr = created.result.expect("create auto-approved under grant")["address"]
        .as_str()
        .unwrap()
        .to_string();

    // signTypedData auto-approves despite the Reject approver.
    let sig = call(
        &state,
        Some(&agent),
        "wallet_signTypedData",
        json!({"account_address": addr, "typed_data": sample_typed_data()}),
    )
    .await;
    assert!(sig.result.is_some(), "grant should auto-approve signing");
}

#[tokio::test]
async fn grant_does_not_auto_approve_funding() {
    // Reject approver; funding must STILL prompt (and thus be rejected) under a grant.
    let state = state_rejecting(&["companion_requestFunding"]);
    let (token, id, _addr) = agent_with_account_and_id(&state).await;
    grant_forever(&state, &id).await;

    let resp = call(&state, Some(&token), "companion_requestFunding", funding_params("1000")).await;
    assert_eq!(err_code(&resp), 113, "funding must require approval even with a grant");
}

#[tokio::test]
async fn revoked_or_expired_grant_prompts_again() {
    let state = state_rejecting(&["wallet_signTypedData"]);
    let (token, id, addr) = agent_with_account_and_id(&state).await;

    // Expired grant (until in the past) → still prompts → rejected.
    state.clients.lock().await.grant(&id, 1);
    let r1 = call(&state, Some(&token), "wallet_signTypedData",
        json!({"account_address": addr, "typed_data": sample_typed_data()})).await;
    assert_eq!(err_code(&r1), 113);

    // Active grant → auto-approves.
    grant_forever(&state, &id).await;
    let r2 = call(&state, Some(&token), "wallet_signTypedData",
        json!({"account_address": addr, "typed_data": sample_typed_data()})).await;
    assert!(r2.result.is_some());

    // Revoke → prompts again → rejected.
    assert!(state.clients.lock().await.revoke_grant(&id));
    let r3 = call(&state, Some(&token), "wallet_signTypedData",
        json!({"account_address": addr, "typed_data": sample_typed_data()})).await;
    assert_eq!(err_code(&r3), 113);
}

#[tokio::test]
async fn funding_is_turnkey_with_a_node() {
    let state = state_with_node(Decision::Approve, "0x3", "0xfeed");
    let (token, agent_addr) = agent_with_account(&state).await;
    // No nonce, no resource_bounds, submit=true → fully automatic.
    let resp = call(
        &state,
        Some(&token),
        "companion_requestFunding",
        json!({ "amount": "1000000000000000000", "submit": true }),
    )
    .await;
    let r = resp.result.expect("funding broadcast");
    assert_eq!(r["submitted"], json!(true));
    assert_eq!(r["transaction_hash"], json!("0xfeed"));
    assert_eq!(r["recipient"], json!(agent_addr));
    assert_eq!(r["amount"], json!("1000000000000000000"));
}

#[tokio::test]
async fn add_invoke_enforces_scope() {
    let state = state_with(Decision::Approve, false);
    let (_, user_address) = user_registry();
    let agent = pair(&state, "agent").await;
    // Agent tries to invoke from the user account → forbidden.
    let resp = call(
        &state,
        Some(&agent),
        "wallet_addInvokeTransaction",
        invoke_params(&user_address),
    )
    .await;
    assert_eq!(err_code(&resp), -32002);
}

#[tokio::test]
async fn add_invoke_rejects_empty_calls() {
    let state = state_with(Decision::Approve, false);
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let mut p = invoke_params(&address);
    p["calls"] = json!([]);
    let resp = call(&state, Some(&token), "wallet_addInvokeTransaction", p).await;
    assert_eq!(err_code(&resp), 114); // INVALID_REQUEST_PAYLOAD
}

#[tokio::test]
async fn locked_wallet_blocks_account_access() {
    let state = state_with(Decision::Approve, true); // locked
    let status = call(&state, None, "companion_getStatus", json!({})).await;
    assert_eq!(status.result.unwrap()["locked"], json!(true));

    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "wallet_requestAccounts", json!({})).await;
    assert_eq!(err_code(&resp), -32001); // Locked
}

/// Funding-request params with the manager's nonce + fee bounds (sign-only).
fn funding_params(amount: &str) -> Value {
    json!({
        "amount": amount,
        "nonce": "0x0",
        "resource_bounds": {
            "l1_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"},
            "l2_gas": {"max_amount": "0xf4240", "max_price_per_unit": "0x1"},
            "l1_data_gas": {"max_amount": "0x3e8", "max_price_per_unit": "0x1"}
        }
    })
}

/// Pair an agent and give it one account; return (token, agent_account_address).
async fn agent_with_account(state: &ServerState) -> (String, String) {
    let token = pair(state, "agent").await;
    let created = call(state, Some(&token), "companion_createAgentAccount", json!({"label": "bot"})).await;
    let addr = created.result.unwrap()["address"].as_str().unwrap().to_string();
    (token, addr)
}

#[tokio::test]
async fn funding_errors_clearly_when_manager_not_deployed() {
    // Manager (user-root) reads as NOT deployed on the active network → a clear,
    // actionable precondition error (-32006) naming the manager + the fix, NOT an
    // opaque node "Contract not found" (GitHub issue #1).
    let state = state_with_node_opts(Decision::Approve, "0x3", "0xfeed", vec![], false);
    let (token, _) = agent_with_account(&state).await;
    let resp = call(&state, Some(&token), "companion_requestFunding", funding_params("1000")).await;
    assert_eq!(err_code(&resp), -32006);
    let msg = resp.error.unwrap().message;
    assert!(msg.contains("not deployed"), "message should explain the cause: {msg}");
}

#[tokio::test]
async fn deploy_account_already_deployed_errors() {
    // Account already on-chain → a clear precondition error (-32006) instead of a
    // confusing "invalid nonce" from reusing nonce 0 (the bug the maintainer hit
    // when the UI let them click Deploy twice).
    let state = state_with_node_opts(Decision::Approve, "0x0", "0xdeed", vec![], true);
    let (agent, _) = agent_with_account(&state).await;
    let resp = call(&state, Some(&agent), "companion_deployAccount", json!({ "submit": true })).await;
    assert_eq!(err_code(&resp), -32006);
    assert!(resp.error.unwrap().message.contains("already deployed"));
}

#[tokio::test]
async fn funding_source_is_the_user_root_account() {
    let state = state_with(Decision::Approve, false);
    let (_, root_addr) = user_registry(); // user index 0
    let token = pair(&state, "agent").await;
    let resp = call(&state, Some(&token), "companion_fundingSource", json!({})).await;
    let r = resp.result.expect("funding source");
    assert_eq!(r["address"], json!(root_addr));
    assert_eq!(r["index"], json!(0));
}

#[tokio::test]
async fn agent_requests_funding_and_gets_signed_transfer() {
    let state = state_with(Decision::Approve, false);
    let (_, manager_addr) = user_registry();
    let (token, agent_addr) = agent_with_account(&state).await;

    let resp = call(&state, Some(&token), "companion_requestFunding", funding_params("5000000000000000000")).await;
    let r = resp.result.expect("expected signed funding transfer");

    assert_eq!(r["submitted"], json!(false));
    assert_eq!(r["amount"], json!("5000000000000000000"));
    // Sender is the manager (user root), NOT chosen by the agent.
    assert_eq!(r["signed_transaction"]["sender_address"], json!(manager_addr));
    // Recipient defaults to the agent's own account.
    assert_eq!(r["recipient"], json!(agent_addr));
    // One transfer call → encoded calldata [1, token, selector, 3, to, low, high] = 7.
    assert_eq!(r["signed_transaction"]["calldata"].as_array().unwrap().len(), 7);
    assert_eq!(r["signature"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn funding_rejected_maps_to_113() {
    let state = state_rejecting(&["companion_requestFunding"]);
    let (token, _) = agent_with_account(&state).await;
    let resp = call(&state, Some(&token), "companion_requestFunding", funding_params("1000")).await;
    assert_eq!(err_code(&resp), 113);
}

#[tokio::test]
async fn app_client_cannot_request_funding() {
    let state = state_with(Decision::Approve, false);
    let app = pair(&state, "app").await;
    let resp = call(&state, Some(&app), "companion_requestFunding", funding_params("1000")).await;
    assert_eq!(err_code(&resp), -32002); // Forbidden
}

#[tokio::test]
async fn agent_cannot_fund_an_account_it_does_not_own() {
    let state = state_with(Decision::Approve, false);
    let (_, user_address) = user_registry();
    let (token, _) = agent_with_account(&state).await;
    // Try to direct funds to the user's (out-of-scope) account.
    let mut p = funding_params("1000");
    p["recipient"] = json!(user_address);
    let resp = call(&state, Some(&token), "companion_requestFunding", p).await;
    assert_eq!(err_code(&resp), 114); // not in the agent's scope → invalid recipient
}

#[tokio::test]
async fn funding_rejects_zero_amount() {
    let state = state_with(Decision::Approve, false);
    let (token, _) = agent_with_account(&state).await;
    let resp = call(&state, Some(&token), "companion_requestFunding", funding_params("0")).await;
    assert_eq!(err_code(&resp), 114);
}

#[tokio::test]
async fn report_issue_builds_prefilled_github_url() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "agent").await;
    let resp = call(
        &state,
        Some(&token),
        "companion_reportIssue",
        json!({
            "goal": "sponsor an account deploy with a paymaster",
            "needed": "a companion_deployAccount option that accepts a paymaster",
            "observed": "error -32601",
            "impact": "blocked",
            "workaround_avoided": "none — did not handle keys myself"
        }),
    )
    .await;
    let v = resp.result.expect("report_issue should succeed");
    let url = v["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://github.com/starknet-innovation/strkd/issues/new?title="),
        "url = {url}"
    );
    assert!(url.contains("&body="));
    // Agent-authored text is percent-encoded — no raw space/newline escapes the query.
    assert!(!url.contains(' '));
    assert!(!url.contains('\n'));
    assert!(v["title"].as_str().unwrap().starts_with("[agent-feedback] "));
    assert_eq!(v["filed"], json!(false));
    assert_eq!(v["repo"], json!("starknet-innovation/strkd"));
    let body = v["body"].as_str().unwrap();
    assert!(body.contains("paymaster"));
    assert!(body.contains("**Needed:**"));
    // Optional fields appear only when supplied.
    assert!(body.contains("**Impact:**"));
    assert!(!body.contains("**Limitation:**"));
}

#[tokio::test]
async fn report_issue_requires_goal_and_needed() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "agent").await;
    // Missing `needed`.
    let resp = call(
        &state,
        Some(&token),
        "companion_reportIssue",
        json!({ "goal": "do a thing" }),
    )
    .await;
    assert_eq!(err_code(&resp), 114);
}

#[tokio::test]
async fn report_issue_never_prompts() {
    // Under an approver that would REJECT companion_reportIssue, building the
    // feedback link still succeeds — it posts nothing and never consults the user.
    let state = state_rejecting(&["companion_reportIssue"]);
    let token = pair(&state, "agent").await;
    let resp = call(
        &state,
        Some(&token),
        "companion_reportIssue",
        json!({ "goal": "g", "needed": "n" }),
    )
    .await;
    assert!(
        resp.result.is_some(),
        "report_issue must not depend on approval"
    );
}

#[tokio::test]
async fn report_issue_requires_pairing() {
    let state = state_with(Decision::Approve, false);
    let resp = call(
        &state,
        None,
        "companion_reportIssue",
        json!({ "goal": "g", "needed": "n" }),
    )
    .await;
    assert_eq!(err_code(&resp), 118); // NOT_REGISTERED
}

#[tokio::test]
async fn requests_are_logged() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await; // 1 logged
    call(&state, Some(&token), "wallet_requestChainId", json!({})).await; // 2
    let log = state.log.lock().await;
    assert!(log.count() >= 2);
    let recent = log.recent(50);
    let chain = recent
        .iter()
        .find(|e| e.method == "wallet_requestChainId")
        .expect("chain-id call logged");
    // Network and timing are captured; outcome is success.
    assert_eq!(chain.network.as_deref(), Some("SN_SEPOLIA"));
    assert_eq!(chain.outcome, "ok");
    // Full payloads on by default → result captured.
    assert!(chain.result_json.is_some());
}

// ── On-device proving (companion_prove*) ─────────────────────────────────────

/// A test-only `Prover` that returns a canned proof. The shipped crate has no
/// mock backend (the real backends fail honestly when unconfigured), so the
/// success path is exercised here with a stub instead of a fake in production.
struct StubProver;

#[async_trait]
impl prover::Prover for StubProver {
    async fn prove(&self, _req: prover::ProveRequest) -> Result<prover::ProveResult, String> {
        Ok(prover::ProveResult {
            proof: json!({ "proof": "0xstub", "proof_facts": [], "l2_to_l1_messages": [] }),
        })
    }
    fn kind(&self) -> &'static str {
        "stub"
    }
    fn ready(&self) -> bool {
        true
    }
}

/// `ProverState` backed by the stub, attached to a fresh server (no native
/// binary or network needed).
fn state_with_prover(tag: &str) -> Arc<ServerState> {
    let data_dir =
        std::env::temp_dir().join(format!("strkd-rpc-prove-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let pstate = Arc::new(prover::ProverState {
        prover: Arc::new(StubProver),
        jobs: Arc::new(prover::Jobs::new(0)),
        settings: Arc::new(prover::SettingsStore::load(data_dir.join("settings.json"))),
        storage: Arc::new(prover::Storage::new(data_dir.join("storage"))),
    });
    Arc::new(
        ServerState::new(
            Arc::new(Mutex::new(make_session(false))),
            Arc::new(AutoApprover(Decision::Approve)),
        )
        .with_prover(pstate),
    )
}

#[tokio::test]
async fn companion_prove_enqueues_and_status_reports_success() {
    let state = state_with_prover("ok");
    let token = pair(&state, "app").await;

    // Proving requires a paired token like every other operational method.
    let unauth = call(&state, None, "companion_prove", json!({ "payload": { "x": 1 } })).await;
    assert_eq!(err_code(&unauth), 118); // NOT_REGISTERED

    let resp = call(
        &state,
        Some(&token),
        "companion_prove",
        json!({ "payload": { "transaction": { "x": 1 } }, "network": "testnet", "label": "t" }),
    )
    .await;
    let r = resp.result.expect("prove enqueued");
    assert_eq!(r["status"], json!("queued"));
    let job_id = r["job_id"].as_str().expect("job_id").to_string();

    // Poll companion_proveStatus until terminal.
    let mut job = json!({});
    for _ in 0..200 {
        let s = call(&state, Some(&token), "companion_proveStatus", json!({ "job_id": job_id }))
            .await
            .result
            .expect("status ok");
        if s["status"] == json!("succeeded") || s["status"] == json!("failed") {
            job = s;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(job["status"], json!("succeeded"));
    assert_eq!(job["result"]["proof"], json!("0xstub"));

    // The activity feed lists the job.
    let act = call(&state, Some(&token), "companion_proofActivity", json!({}))
        .await
        .result
        .expect("activity ok");
    assert!(act["activity"].as_array().map(|a| !a.is_empty()).unwrap_or(false));
}

#[tokio::test]
async fn companion_prove_unknown_job_is_invalid_request() {
    let state = state_with_prover("unknown");
    let token = pair(&state, "app").await;
    let resp =
        call(&state, Some(&token), "companion_proveStatus", json!({ "job_id": "p999" })).await;
    assert_eq!(err_code(&resp), 114); // INVALID_REQUEST
}

#[tokio::test]
async fn companion_prove_without_prover_is_not_implemented() {
    // No prover attached → clean -32601, not a panic.
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "companion_prove", json!({ "payload": {} })).await;
    assert_eq!(err_code(&resp), -32601); // NOT_IMPLEMENTED
}

#[tokio::test]
async fn sign_and_prove_signs_the_virtual_tx_and_enqueues_it() {
    // The wallet signs Tx A (a normal v3 invoke, NOT proof-carrying) and hands it
    // to the in-process prover in one call. The stub prover succeeds regardless of
    // tx content, so this exercises sign → enqueue → prove without a native binary.
    let state = state_with_prover("signprove");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    let resp = call(&state, Some(&token), "companion_signAndProve", invoke_params(&address)).await;
    let r = resp.result.expect("sign-and-prove enqueued");
    assert_eq!(r["status"], json!("queued"));
    assert!(r["transaction_hash"].as_str().unwrap().starts_with("0x"));
    let job_id = r["job_id"].as_str().expect("job_id").to_string();

    let mut job = json!({});
    for _ in 0..200 {
        let s = call(&state, Some(&token), "companion_proveStatus", json!({ "job_id": job_id }))
            .await
            .result
            .expect("status ok");
        if s["status"] == json!("succeeded") || s["status"] == json!("failed") {
            job = s;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(job["status"], json!("succeeded"));
    assert_eq!(job["result"]["proof"], json!("0xstub"));
}

#[tokio::test]
async fn sign_and_prove_requires_explicit_resource_bounds() {
    // The virtual tx carries private calldata, so strkd must NOT fee-estimate it
    // online — missing resource_bounds is rejected up front (114), not estimated.
    let state = state_with_prover("signprove-bounds");
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;

    let mut p = invoke_params(&address);
    p.as_object_mut().unwrap().remove("resource_bounds");
    let resp = call(&state, Some(&token), "companion_signAndProve", p).await;
    assert_eq!(err_code(&resp), 114); // INVALID_REQUEST
}

#[tokio::test]
async fn sign_and_prove_rejected_maps_to_113() {
    // Signing a real tx on the user's account is approval-gated even though it's
    // never broadcast. Reject only signAndProve (pairing must still go through).
    let (approver, mut rx) = ChannelApprover::new(16);
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let decision = if pending.request.method == "companion_signAndProve" {
                Decision::Reject
            } else {
                Decision::Approve
            };
            let _ = pending.respond.send(decision);
        }
    });
    let dir = std::env::temp_dir().join(format!("strkd-rpc-prove-rej-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let pstate = Arc::new(prover::ProverState {
        prover: Arc::new(StubProver),
        jobs: Arc::new(prover::Jobs::new(0)),
        settings: Arc::new(prover::SettingsStore::load(dir.join("settings.json"))),
        storage: Arc::new(prover::Storage::new(dir.join("storage"))),
    });
    let state = Arc::new(
        ServerState::new(Arc::new(Mutex::new(make_session(false))), Arc::new(approver))
            .with_prover(pstate),
    );
    let (_, address) = user_registry();
    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "companion_signAndProve", invoke_params(&address)).await;
    assert_eq!(err_code(&resp), 113); // USER_REFUSED
}
