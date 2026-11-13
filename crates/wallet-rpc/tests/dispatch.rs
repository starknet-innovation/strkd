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
        contract: Default::default(),
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

    // Guard the pin against SNIP-12 regressions. The digest binds the account
    // address, so it moved when #16 changed the OZ salt and the account axis —
    // the value below was re-derived for the new address and cross-checked
    // against starknet.js `typedData.getMessageHash`, which agrees exactly. The
    // hashing itself is unchanged. If the krusty pin regressed (keccak prefix,
    // or shortstring not going through parse_felt), this would change *without*
    // the address changing.
    assert_eq!(
        hres["hash"].as_str().unwrap(),
        "0x2e3d857f3f9e5d3c23b3268017e1c1ea65baba457b0ec82ec2e63e8de7d4869",
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
    assert!(resp.error.unwrap().message.contains("later phase"));
}

/// STRK20 methods validate their params before touching the node, and the
/// shadow-account commitment is the one part still to come.
#[tokio::test]
async fn strk20_methods_validate_params_first() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    for method in ["wallet_strk20PrepareInvoke", "wallet_strk20InvokeTransaction", "wallet_strk20Balances"] {
        let resp = call(&state, Some(&token), method, json!({})).await;
        assert_eq!(err_code(&resp), 114, "{method}");
    }
    let open = json!({ "actions": [{ "type": "transfer", "token": "0x4718", "amount": "OPEN", "recipient": "0x1" }] });
    assert_eq!(err_code(&call(&state, Some(&token), "wallet_strk20PrepareInvoke", open).await), -32601);
    // Valid params, but no node to read the pool from.
    let ok = json!({ "tokens": ["0x4718"] });
    assert_eq!(err_code(&call(&state, Some(&token), "wallet_strk20Balances", ok).await), -32005);
    let resp = call(&state, Some(&token), "wallet_strk20ShadowAccountCommitment", json!({ "dapp_name": "x" })).await;
    assert_eq!(err_code(&resp), -32601);
    assert!(resp.error.unwrap().message.contains("later phase"));
}

/// Revealing private balances is gated like a signature: a refusal returns 113
/// before the pool is read.
#[tokio::test]
async fn strk20_balances_need_approval() {
    let state = state_rejecting(&["wallet_strk20Balances"]);
    let node = MockNode {
        nonce: Felt::ONE,
        hash: Felt::TWO,
        deployed: vec![],
        all_deployed: false,
        balance: 0,
    };
    state.set_node(ChainId::Sepolia, Some(Arc::new(node)));
    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "wallet_strk20Balances", json!({ "tokens": ["0x4718"] })).await;
    assert_eq!(err_code(&resp), 113);
}

/// The loopback service must never expose seed reveal. It is IPC-only by
/// design (see `wallet_rpc::reveal_mnemonic`): this service is reachable by any
/// local process, including the AI agents strkd exists to serve, and spec §5.1
/// is that it never returns key material.
///
/// This test exists so that adding such a method is a deliberate act that
/// breaks a test naming the reason, rather than a plausible-looking addition.
#[tokio::test]
async fn the_service_exposes_no_way_to_reveal_the_seed() {
    let state = state_with(Decision::Approve, false);
    let token = pair(&state, "app").await;
    for method in [
        "companion_revealSeed",
        "companion_exportSeed",
        "companion_revealMnemonic",
        "wallet_exportSeed",
        "companion_getMnemonic",
        "companion_exportPrivateKey",
    ] {
        let resp = call(&state, Some(&token), method, json!({})).await;
        assert_eq!(
            err_code(&resp),
            -32601,
            "{method} must not exist on the loopback service",
        );
        assert!(resp.result.is_none(), "{method} returned a result");
    }
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
async fn a_funding_source_index_on_the_agent_branch_is_refused() {
    // User account n sits at account index n, so an unbounded index reached the
    // agent branch: 0x41474E54 derived agent #0's key, 0xC1474E54 wrapped onto
    // it via the hardened bit, and a u64 truncated onto it. None may sign.
    let state = state_with_node_opts(Decision::Approve, "0x3", "0xfeed", vec![], true);
    let (token, _) = agent_with_account(&state).await;
    for idx in [json!(0x4147_4E54u64), json!(0xC147_4E54u64), json!(0x1_4147_4E54u64), json!("1")] {
        let mut p = funding_params("1000");
        p["funding_source_index"] = idx.clone();
        let resp = call(&state, Some(&token), "companion_requestFunding", p).await;
        assert_eq!(err_code(&resp), 114, "{idx} must be refused as an invalid request");
    }
    // Control: the default index is accepted (so the refusals above are the bound).
    let resp = call(&state, Some(&token), "companion_requestFunding", funding_params("1000")).await;
    assert!(resp.error.as_ref().map(|e| e.code != 114).unwrap_or(true), "{:?}", resp.error);
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

// ── STRK20 proving-block preconditions ───────────────────────────────────────

/// A node for the STRK20 register path: the pool reports the account as not
/// registered, the chain head is `head`, and the account has a class from
/// block `deployed_from` on. `block_error` makes `getClassHashAt` fail with an
/// RPC error other than "contract not found".
struct Strk20Node {
    head: u64,
    deployed_from: u64,
    block_error: bool,
    /// The `block_id` of each `getClassHashAt` call.
    class_hash_queries: std::sync::Mutex<Vec<Value>>,
}

#[async_trait]
impl StarknetRpc for Strk20Node {
    async fn raw(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        match method {
            "starknet_blockNumber" => Ok(json!(self.head)),
            // Every pool view (`get_public_key`, ...) reads zero: not registered.
            "starknet_call" => Ok(json!(["0x0"])),
            "starknet_getClassHashAt" => {
                self.class_hash_queries.lock().unwrap().push(params["block_id"].clone());
                if self.block_error {
                    return Err(NodeError::Rpc(r#"{"code":24,"message":"Block not found"}"#.into()));
                }
                match params["block_id"]["block_number"].as_u64() {
                    Some(b) if b >= self.deployed_from => Ok(json!("0x1234")),
                    _ => Err(NodeError::Rpc(r#"{"code":20,"message":"Contract not found"}"#.into())),
                }
            }
            m => Err(NodeError::Rpc(format!("{m}: unexpected in this test"))),
        }
    }
    async fn get_nonce(&self, _: &Felt) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_invoke(&self, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_invoke(
        &self,
        _: &Felt,
        _: &[Felt],
        _: &[Felt],
        _: &Felt,
        _: &FeeBounds,
        _: &[Felt],
        _: Option<&str>,
    ) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn is_deployed(&self, _: &Felt) -> Result<bool, NodeError> {
        unimplemented!()
    }
    async fn balance_of(&self, _: &Felt, _: &Felt) -> Result<u128, NodeError> {
        unimplemented!()
    }
    async fn estimate_deploy_account(
        &self,
        _: &Felt,
        _: &Felt,
        _: &[Felt],
        _: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_deploy_account(
        &self,
        _: &Felt,
        _: &[Felt],
        _: &Felt,
        _: &[Felt],
        _: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_declare(
        &self,
        _: &Felt,
        _: &Felt,
        _: &Value,
        _: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_declare(
        &self,
        _: &Felt,
        _: &Felt,
        _: &Value,
        _: &[Felt],
        _: &Felt,
        _: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        unimplemented!()
    }
}

fn strk20_node(head: u64, deployed_from: u64, block_error: bool) -> Arc<Strk20Node> {
    Arc::new(Strk20Node {
        head,
        deployed_from,
        block_error,
        class_hash_queries: Default::default(),
    })
}

/// Run `companion_strk20Register` against `node`, with an approver that
/// refuses the register prompt: a call that passes the preconditions stops at
/// the prompt with 113, before anything is proved.
async fn strk20_register_with(tag: &str, node: Arc<Strk20Node>) -> Response {
    let (approver, mut rx) = ChannelApprover::new(16);
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            let decision = if pending.request.method == "companion_strk20Register" {
                Decision::Reject
            } else {
                Decision::Approve
            };
            let _ = pending.respond.send(decision);
        }
    });
    let dir = std::env::temp_dir().join(format!("strkd-rpc-strk20-{tag}-{}", std::process::id()));
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
    state.set_node(ChainId::Sepolia, Some(node));
    let token = pair(&state, "app").await;
    call(&state, Some(&token), "companion_strk20Register", json!({})).await
}

/// #50: registering right after deploying the account. Proofs run 12 blocks
/// behind the head, where the account doesn't exist yet: the call says so up
/// front (-32006), before prompting, instead of failing in the prover.
#[tokio::test]
async fn strk20_register_refuses_an_account_not_deployed_at_the_proving_block() {
    // Deployed at block 995; head 1000 → proving block 988.
    let node = strk20_node(1000, 995, false);
    let resp = strk20_register_with("undeployed", node.clone()).await;
    assert_eq!(err_code(&resp), -32006, "{:?}", resp.error);
    let msg = resp.error.unwrap().message;
    assert!(msg.contains("not deployed at reference block 988"), "{msg}");
    assert!(msg.contains("12 blocks behind") && msg.contains("about a minute"), "{msg}");
    assert_eq!(
        *node.class_hash_queries.lock().unwrap(),
        vec![json!({ "block_number": 988 })]
    );
}

/// Deployed at the proving block: the check passes and the call reaches the
/// approval prompt (refused here, so nothing is proved).
#[tokio::test]
async fn strk20_register_passes_the_check_once_deployed_at_the_proving_block() {
    let resp = strk20_register_with("deployed", strk20_node(1000, 988, false)).await;
    assert_eq!(err_code(&resp), 113, "{:?}", resp.error);
}

/// Any other failure of that lookup is a node error, not a deploy-timing hint.
#[tokio::test]
async fn strk20_register_reports_other_node_errors_as_node_errors() {
    let resp = strk20_register_with("node-error", strk20_node(1000, 0, true)).await;
    assert_eq!(err_code(&resp), -32004, "{:?}", resp.error);
    assert!(resp.error.unwrap().message.contains("Block not found"));
}

// ── STRK20 external owners (#48, part 1: enrolment) ─────────────────────────

const OWNER: &str = "0x0000000000000000000000000000000000000000000000000000000000c0ffee";

/// How the mock owner contract answers `supports_interface`.
#[derive(Clone, Copy)]
enum Src5 {
    Yes,
    No,
    /// No SRC5 at all: the call reverts (entry point not found).
    Reverts,
}

/// A node with one owner contract at [`OWNER`] and the pool reporting
/// `registered_key` from `get_public_key` (zero: not registered).
struct OwnerNode {
    deployed: bool,
    src5: Src5,
    registered_key: Felt,
}

#[async_trait]
impl StarknetRpc for OwnerNode {
    async fn raw(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        match method {
            "starknet_getClassHashAt" => {
                assert_eq!(params["contract_address"], json!(format!("{:#x}", Felt::from_hex(OWNER).unwrap())));
                if self.deployed {
                    Ok(json!("0x1234"))
                } else {
                    Err(NodeError::Rpc(r#"{"code":20,"message":"Contract not found"}"#.into()))
                }
            }
            "starknet_call" => {
                let selector = params["request"]["entry_point_selector"].as_str().unwrap();
                let src5 = format!("{:#x}", wallet_core::get_selector_from_name("supports_interface"));
                if selector == src5 {
                    assert_eq!(
                        params["request"]["calldata"],
                        json!([wallet_rpc::strk20::owners::CUSTOM_SIGNATURE_INTERFACE_ID])
                    );
                    match self.src5 {
                        Src5::Yes => Ok(json!(["0x1"])),
                        Src5::No => Ok(json!(["0x0"])),
                        // What the Sepolia node answers for a contract without SRC5 (2026-11-13).
                        Src5::Reverts => Err(NodeError::Rpc(
                            r#"{"code":21,"message":"Requested entry point does not exist in the contract"}"#.into(),
                        )),
                    }
                } else {
                    // The pool's `get_public_key(owner)`.
                    Ok(json!([format!("{:#x}", self.registered_key)]))
                }
            }
            m => Err(NodeError::Rpc(format!("{m}: unexpected in this test"))),
        }
    }
    async fn get_nonce(&self, _: &Felt) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_invoke(&self, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_invoke(
        &self,
        _: &Felt,
        _: &[Felt],
        _: &[Felt],
        _: &Felt,
        _: &FeeBounds,
        _: &[Felt],
        _: Option<&str>,
    ) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn is_deployed(&self, _: &Felt) -> Result<bool, NodeError> {
        unimplemented!()
    }
    async fn balance_of(&self, _: &Felt, _: &Felt) -> Result<u128, NodeError> {
        unimplemented!()
    }
    async fn estimate_deploy_account(&self, _: &Felt, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_deploy_account(&self, _: &Felt, _: &[Felt], _: &Felt, _: &[Felt], _: &FeeBounds) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_declare(&self, _: &Felt, _: &Felt, _: &Value, _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_declare(&self, _: &Felt, _: &Felt, _: &Value, _: &[Felt], _: &Felt, _: &FeeBounds) -> Result<Felt, NodeError> {
        unimplemented!()
    }
}

/// An approver that records every prompt and rejects the listed methods.
struct RecordingApprover {
    reject: &'static [&'static str],
    prompts: std::sync::Mutex<Vec<wallet_rpc::ApprovalRequest>>,
}

#[async_trait]
impl wallet_rpc::Approver for RecordingApprover {
    async fn request_approval(&self, req: wallet_rpc::ApprovalRequest) -> Decision {
        let decision = if self.reject.contains(&req.method.as_str()) { Decision::Reject } else { Decision::Approve };
        self.prompts.lock().unwrap().push(req);
        decision
    }
}

fn owner_state(node: OwnerNode, reject: &'static [&'static str]) -> (Arc<ServerState>, Arc<RecordingApprover>) {
    let approver = Arc::new(RecordingApprover { reject, prompts: Default::default() });
    let state = Arc::new(
        ServerState::new(Arc::new(Mutex::new(make_session(false))), approver.clone())
            .with_node(ChainId::Sepolia, Arc::new(node)),
    );
    (state, approver)
}

fn owner_node(deployed: bool, src5: Src5) -> OwnerNode {
    OwnerNode { deployed, src5, registered_key: Felt::ZERO }
}

fn prompted(approver: &RecordingApprover, method: &str) -> Vec<String> {
    approver.prompts.lock().unwrap().iter().filter(|p| p.method == method).map(|p| p.summary.clone()).collect()
}

fn reason(resp: &Response) -> Option<String> {
    resp.error.as_ref()?.data.as_ref()?["reason"].as_str().map(str::to_string)
}

/// The owner's viewing public key as strkd derives it from user account 0
/// (test seed) on Sepolia's canonical pool.
fn expected_owner_public_key() -> Felt {
    let key = wallet_core::strk20_owner_viewing_key(
        TEST_MNEMONIC,
        Domain::User,
        0,
        None,
        &ChainId::Sepolia.as_felt(),
        &Felt::from_hex(wallet_rpc::strk20::SEPOLIA_POOL).unwrap(),
        &Felt::from_hex(OWNER).unwrap(),
    )
    .unwrap();
    strk20::crypto::public_key(key)
}

#[tokio::test]
async fn add_owner_enrols_after_a_prompt_and_lists_it() {
    let (state, approver) = owner_state(owner_node(true, Src5::Yes), &[]);
    let token = pair(&state, "app").await;
    let (_, controller) = user_registry();

    let resp = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": "0xc0ffee", "label": "vault" })).await;
    let r = resp.result.unwrap_or_else(|| panic!("{:?}", resp.error));
    assert_eq!(r["owner_address"], json!(OWNER));
    assert_eq!(r["controller"]["address"], json!(controller));
    assert_eq!(r["chain"], json!("SN_SEPOLIA"));
    assert_eq!(r["registered"], json!(false));
    assert_eq!(r["viewing_public_key"], json!(format!("{:#x}", expected_owner_public_key())));
    // Only the public key comes back: no field carries the viewing key.
    assert!(r.as_object().unwrap().keys().all(|k| !k.contains("sk") && k != "viewing_key"), "{r}");

    let prompts = prompted(&approver, "companion_strk20AddOwner");
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains(OWNER) && prompts[0].contains(&controller), "{}", prompts[0]);
    assert!(prompts[0].contains("cannot check who controls this contract"), "{}", prompts[0]);

    let list = call(&state, Some(&token), "companion_strk20ListOwners", json!({})).await.result.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["owner_address"], json!(OWNER));
    assert_eq!(list[0]["label"], json!("vault"));

    // Enrolling it again in the same scope is refused.
    let again = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&again), -32006);
    assert_eq!(reason(&again).as_deref(), Some("OWNER_ALREADY_ENROLLED"));
}

/// An owner already registered with the pool under the key this controller
/// derives enrols, and reports `registered: true`.
#[tokio::test]
async fn add_owner_accepts_a_registration_under_the_derived_key() {
    let node = OwnerNode { deployed: true, src5: Src5::Yes, registered_key: expected_owner_public_key() };
    let (state, _) = owner_state(node, &[]);
    let token = pair(&state, "app").await;
    let r = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await.result.unwrap();
    assert_eq!(r["registered"], json!(true));
}

/// D6: enrolment always prompts. An agent whose grant auto-approves its other
/// operations still gets the prompt (refused here → 113, nothing recorded).
#[tokio::test]
async fn add_owner_prompts_even_under_an_active_grant() {
    let (state, approver) = owner_state(owner_node(true, Src5::Yes), &["companion_strk20AddOwner"]);
    let (token, id, _) = agent_with_account_and_id(&state).await;
    grant_forever(&state, &id).await;

    let resp = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&resp), 113, "{:?}", resp.error);
    assert_eq!(prompted(&approver, "companion_strk20AddOwner").len(), 1);
    let list = call(&state, Some(&token), "companion_strk20ListOwners", json!({})).await.result.unwrap();
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn add_owner_refuses_an_undeployed_owner() {
    let (state, approver) = owner_state(owner_node(false, Src5::Yes), &[]);
    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&resp), -32006, "{:?}", resp.error);
    assert_eq!(reason(&resp).as_deref(), Some("OWNER_NOT_DEPLOYED"));
    assert!(resp.error.as_ref().unwrap().message.starts_with("OWNER_NOT_DEPLOYED: "));
    assert!(prompted(&approver, "companion_strk20AddOwner").is_empty());
}

#[tokio::test]
async fn add_owner_refuses_an_owner_without_custom_validation() {
    for src5 in [Src5::No, Src5::Reverts] {
        let (state, approver) = owner_state(owner_node(true, src5), &[]);
        let token = pair(&state, "app").await;
        let resp = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await;
        assert_eq!(err_code(&resp), -32006, "{:?}", resp.error);
        assert_eq!(reason(&resp).as_deref(), Some("OWNER_NO_CUSTOM_VALIDATION"));
        assert!(prompted(&approver, "companion_strk20AddOwner").is_empty());
    }
}

/// D8: registered with the pool under a key this controller doesn't derive.
#[tokio::test]
async fn add_owner_refuses_a_foreign_registration() {
    let node = OwnerNode { deployed: true, src5: Src5::Yes, registered_key: Felt::from_hex("0x1234abcd").unwrap() };
    let (state, approver) = owner_state(node, &[]);
    let token = pair(&state, "app").await;
    let resp = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&resp), -32006, "{:?}", resp.error);
    assert_eq!(reason(&resp).as_deref(), Some("OWNER_KEY_MISMATCH"));
    assert!(prompted(&approver, "companion_strk20AddOwner").is_empty());
}

#[tokio::test]
async fn add_owner_validates_its_params() {
    let (state, _) = owner_state(owner_node(true, Src5::Yes), &[]);
    let token = pair(&state, "app").await;
    let (_, controller) = user_registry();
    for params in [json!({}), json!({ "owner_address": "0x0" }), json!({ "owner_address": "nope" })] {
        assert_eq!(err_code(&call(&state, Some(&token), "companion_strk20AddOwner", params).await), 114);
    }
    // A strkd account is not an external owner.
    let own = call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": controller })).await;
    assert_eq!(err_code(&own), 114);
    // The controller must be in the caller's scope.
    let foreign = json!({ "owner_address": OWNER, "account_address": "0x123" });
    assert_eq!(err_code(&call(&state, Some(&token), "companion_strk20AddOwner", foreign).await), -32002);
}

/// Owners are scoped like accounts: agent B neither lists nor uses nor removes
/// agent A's owner; the user's (app) owners are separate from both.
#[tokio::test]
async fn owners_are_scoped_to_the_enrolling_client() {
    let (state, _) = owner_state(owner_node(true, Src5::Yes), &[]);
    let (token_a, _, account_a) = agent_with_account_and_id(&state).await;
    let (token_b, _, _) = agent_with_account_and_id(&state).await;
    let app = pair(&state, "app").await;

    let r = call(&state, Some(&token_a), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await;
    let r = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(r["controller"]["address"], json!(account_a), "an agent's controller is its own account");

    let listed = |token: String| {
        let state = state.clone();
        async move { call(&state, Some(&token), "companion_strk20ListOwners", json!({})).await.result.unwrap() }
    };
    assert_eq!(listed(token_a.clone()).await.as_array().unwrap().len(), 1);
    assert_eq!(listed(token_b.clone()).await, json!([]));
    assert_eq!(listed(app.clone()).await, json!([]));

    // B can't act on A's owner: OWNER_NOT_ENROLLED, same as an unknown owner.
    let use_b = call(&state, Some(&token_b), "wallet_strk20Balances", json!({ "tokens": ["0x4718"], "owner_address": OWNER })).await;
    assert_eq!(err_code(&use_b), -32002);
    assert_eq!(reason(&use_b).as_deref(), Some("OWNER_NOT_ENROLLED"));
    let rm_b = call(&state, Some(&token_b), "companion_strk20RemoveOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&rm_b), -32002);
    assert_eq!(reason(&rm_b).as_deref(), Some("OWNER_NOT_ENROLLED"));
    assert_eq!(listed(token_a.clone()).await.as_array().unwrap().len(), 1);

    // A's owner resolves for A; the one-shot methods don't take owners yet.
    let use_a = call(&state, Some(&token_a), "wallet_strk20Balances", json!({ "tokens": ["0x4718"], "owner_address": OWNER })).await;
    assert_eq!(err_code(&use_a), -32601, "{:?}", use_a.error);
}

#[tokio::test]
async fn remove_owner_prompts_and_forgets_it() {
    let (state, approver) = owner_state(owner_node(true, Src5::Yes), &[]);
    let token = pair(&state, "app").await;
    call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await.result.unwrap();

    let resp = call(&state, Some(&token), "companion_strk20RemoveOwner", json!({ "owner_address": "0xc0ffee" })).await;
    assert_eq!(resp.result, Some(json!(true)), "{:?}", resp.error);
    assert_eq!(prompted(&approver, "companion_strk20RemoveOwner").len(), 1);
    let list = call(&state, Some(&token), "companion_strk20ListOwners", json!({})).await.result.unwrap();
    assert_eq!(list, json!([]));

    let again = call(&state, Some(&token), "companion_strk20RemoveOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&again), -32002);
    assert_eq!(reason(&again).as_deref(), Some("OWNER_NOT_ENROLLED"));
}

#[tokio::test]
async fn remove_owner_refused_keeps_it() {
    let (state, _) = owner_state(owner_node(true, Src5::Yes), &["companion_strk20RemoveOwner"]);
    let token = pair(&state, "app").await;
    call(&state, Some(&token), "companion_strk20AddOwner", json!({ "owner_address": OWNER })).await.result.unwrap();
    let resp = call(&state, Some(&token), "companion_strk20RemoveOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(err_code(&resp), 113);
    let list = call(&state, Some(&token), "companion_strk20ListOwners", json!({})).await.result.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

/// §3.1: a STRK20 call names an owner or an account, never both.
#[tokio::test]
async fn owner_and_account_address_together_is_114() {
    let (state, _) = owner_state(owner_node(true, Src5::Yes), &[]);
    let token = pair(&state, "app").await;
    let (_, account) = user_registry();
    let both = json!({ "tokens": ["0x4718"], "owner_address": OWNER, "account_address": account });
    let resp = call(&state, Some(&token), "wallet_strk20Balances", both).await;
    assert_eq!(err_code(&resp), 114, "{:?}", resp.error);
    assert!(resp.error.unwrap().message.contains("not both"));
    // An unknown owner on its own is OWNER_NOT_ENROLLED.
    let unknown = call(&state, Some(&token), "wallet_strk20Balances", json!({ "tokens": ["0x4718"], "owner_address": OWNER })).await;
    assert_eq!(reason(&unknown).as_deref(), Some("OWNER_NOT_ENROLLED"));
}

/// `data` is additive: errors without a reason serialize exactly as before.
#[tokio::test]
async fn errors_without_a_reason_have_no_data_member() {
    let state = state_with(Decision::Approve, false);
    let resp = call(&state, None, "wallet_requestAccounts", json!({})).await;
    let v = serde_json::to_value(&resp).unwrap();
    assert_eq!(v["error"], json!({ "code": 118, "message": "caller is not a registered client" }));
}

// ── STRK20 plans (#48, part 2: the plan step) ───────────────────────────────

/// A node for the plan step: head 1000, every contract deployed, the owner
/// answering `supports_interface` with `true` at `latest` and with
/// `src5_at_block` at a numbered block, and the pool's `get_public_key`
/// returning `registered_key` (zero: not registered). Every other pool view
/// reads zero. Records every call.
struct PlanNode {
    head: u64,
    src5_at_block: bool,
    registered_key: Felt,
    calls: std::sync::Mutex<Vec<(String, Value)>>,
}

impl PlanNode {
    fn methods(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }
    /// The `block_id` of every `supports_interface` call.
    fn src5_blocks(&self) -> Vec<Value> {
        let src5 = format!("{:#x}", wallet_core::get_selector_from_name("supports_interface"));
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "starknet_call" && p["request"]["entry_point_selector"] == json!(src5))
            .map(|(_, p)| p["block_id"].clone())
            .collect()
    }
}

#[async_trait]
impl StarknetRpc for PlanNode {
    async fn raw(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        self.calls.lock().unwrap().push((method.to_string(), params.clone()));
        match method {
            "starknet_blockNumber" => Ok(json!(self.head)),
            "starknet_getClassHashAt" => Ok(json!("0x1234")),
            "starknet_call" => {
                let selector = params["request"]["entry_point_selector"].as_str().unwrap();
                let hex_sel = |n: &str| format!("{:#x}", wallet_core::get_selector_from_name(n));
                if selector == hex_sel("supports_interface") {
                    let yes = params["block_id"] == json!("latest") || self.src5_at_block;
                    Ok(json!([if yes { "0x1" } else { "0x0" }]))
                } else if selector == hex_sel("get_public_key") {
                    Ok(json!([format!("{:#x}", self.registered_key)]))
                } else {
                    Ok(json!(["0x0"]))
                }
            }
            m => Err(NodeError::Rpc(format!("{m}: unexpected in this test"))),
        }
    }
    async fn get_nonce(&self, _: &Felt) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_invoke(&self, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_invoke(
        &self,
        _: &Felt,
        _: &[Felt],
        _: &[Felt],
        _: &Felt,
        _: &FeeBounds,
        _: &[Felt],
        _: Option<&str>,
    ) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn is_deployed(&self, _: &Felt) -> Result<bool, NodeError> {
        unimplemented!()
    }
    async fn balance_of(&self, _: &Felt, _: &Felt) -> Result<u128, NodeError> {
        unimplemented!()
    }
    async fn estimate_deploy_account(&self, _: &Felt, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_deploy_account(&self, _: &Felt, _: &[Felt], _: &Felt, _: &[Felt], _: &FeeBounds) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_declare(&self, _: &Felt, _: &Felt, _: &Value, _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_declare(&self, _: &Felt, _: &Felt, _: &Value, _: &[Felt], _: &Felt, _: &FeeBounds) -> Result<Felt, NodeError> {
        unimplemented!()
    }
}

const PLAN: &str = "companion_strk20Plan";
const REVEAL: &str = "companion_strk20Plan (reveal_actions)";

fn plan_node(src5_at_block: bool, registered_key: Felt) -> Arc<PlanNode> {
    Arc::new(PlanNode { head: 1000, src5_at_block, registered_key, calls: Default::default() })
}

/// A server with the stub prover, `node` on Sepolia, and a recording approver
/// that rejects `reject`.
fn plan_state(
    tag: &str,
    node: Arc<PlanNode>,
    reject: &'static [&'static str],
) -> (Arc<ServerState>, Arc<RecordingApprover>) {
    let dir = std::env::temp_dir().join(format!("strkd-rpc-plan-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let pstate = Arc::new(prover::ProverState {
        prover: Arc::new(StubProver),
        jobs: Arc::new(prover::Jobs::new(0)),
        settings: Arc::new(prover::SettingsStore::load(dir.join("settings.json"))),
        storage: Arc::new(prover::Storage::new(dir.join("storage"))),
    });
    let approver = Arc::new(RecordingApprover { reject, prompts: Default::default() });
    let state = Arc::new(
        ServerState::new(Arc::new(Mutex::new(make_session(false))), approver.clone())
            .with_node(ChainId::Sepolia, node)
            .with_prover(pstate),
    );
    (state, approver)
}

async fn enrol(state: &ServerState, token: &str, label: &str) {
    let r = call(state, Some(token), "companion_strk20AddOwner", json!({ "owner_address": OWNER, "label": label })).await;
    r.result.unwrap_or_else(|| panic!("enrol: {:?}", r.error));
}

/// `companion_typedDataHash` of the plan's typed data, signed as the owner.
async fn typed_data_hash(state: &ServerState, token: &str, plan: &Value) -> Felt {
    let r = call(
        state,
        Some(token),
        "companion_typedDataHash",
        json!({ "typed_data": plan["typed_data"], "account_address": plan["owner"] }),
    )
    .await;
    Felt::from_hex(r.result.unwrap()["hash"].as_str().unwrap()).unwrap()
}

fn felt_of(v: &Value) -> Felt {
    Felt::from_hex(v.as_str().unwrap_or_else(|| panic!("not a felt: {v}"))).unwrap()
}

fn sepolia_pool() -> Felt {
    Felt::from_hex(wallet_rpc::strk20::SEPOLIA_POOL).unwrap()
}

#[tokio::test]
async fn plan_for_an_owner_prompts_signs_nothing_and_returns_a_reproducible_hash() {
    let node = plan_node(true, Felt::ZERO);
    let (state, approver) = plan_state("owner-ok", node.clone(), &[]);
    let token = pair(&state, "app").await;
    let (_, controller) = user_registry();
    enrol(&state, &token, "vault").await;
    let before = node.calls.lock().unwrap().len();

    let resp = call(&state, Some(&token), PLAN, json!({ "owner_address": OWNER, "register": true })).await;
    let p = resp.result.unwrap_or_else(|| panic!("{:?}", resp.error));
    assert_eq!(p["plan_version"], json!(1));
    assert_eq!(p["plan_block"], json!(988));
    assert_eq!(p["chain_id"], json!("SN_SEPOLIA"));
    assert_eq!(p["owner"], json!(OWNER));
    assert_eq!(p["subject"], json!("owner"));
    assert_eq!(p["controller"], json!(controller));
    assert_eq!(p["pool"], json!(address_hex(&sepolia_pool())));
    assert_eq!(p["plan_id"].as_str().unwrap().len(), 32);
    let now = wallet_rpc::now_unix_ms();
    let expires = p["expires_at"].as_u64().unwrap();
    assert!(expires > now + 3_590_000 && expires <= now + 3_600_000, "{expires}");
    // Register = SetViewingKey + the self-channel.
    assert_eq!(p["summary"]["action_count"], json!(2));
    assert_eq!(p["summary"]["register"], json!(true));
    // No actions without reveal_actions.
    for k in ["client_actions", "client_actions_felts", "actions_revealed"] {
        assert!(p.get(k).is_none(), "{k} returned without reveal_actions: {p}");
    }
    // The typed data reproduces plan_hash through companion_typedDataHash.
    assert_eq!(typed_data_hash(&state, &token, &p).await, felt_of(&p["plan_hash"]));
    assert_eq!(felt_of(&p["typed_data"]["message"]["actions_hash"]), felt_of(&p["actions_hash"]));
    assert_eq!(felt_of(&p["typed_data"]["message"]["owner"]), Felt::from_hex(OWNER).unwrap());
    assert_eq!(p["typed_data"]["domain"]["name"], json!("strkd.Strk20Plan"));

    // One prompt, naming owner, label, controller, chain, action count, pool
    // and the on-chain approval.
    let prompts = prompted(&approver, PLAN);
    assert_eq!(prompts.len(), 1);
    let text = &prompts[0];
    let pool = format!("{:#x}", sepolia_pool());
    for want in [OWNER, "\"vault\"", &controller, "SN_SEPOLIA", "2 action(s)", &pool, "approve this plan on-chain", "Nothing is signed"] {
        assert!(text.contains(want), "prompt lacks {want:?}: {text}");
    }
    assert!(prompted(&approver, REVEAL).is_empty());

    // SRC5 was re-checked at the reference block. Nothing was signed or
    // proved: no pool-nonce read (it precedes signing), no proving job.
    let calls = node.calls.lock().unwrap()[before..].to_vec();
    assert!(calls.iter().all(|(m, _)| m != "starknet_getNonce"), "{calls:?}");
    assert!(node.src5_blocks().contains(&json!({ "block_number": 988 })));
    let act = call(&state, Some(&token), "companion_proofActivity", json!({})).await.result.unwrap();
    assert_eq!(act["activity"], json!([]), "{act}");
}

/// D3/D6: reveal_actions always prompts, even when a grant covers the plan
/// prompt. The actions come back to the caller but not into the request log.
#[tokio::test]
async fn reveal_actions_prompts_even_under_a_grant_and_returns_the_hashed_actions() {
    let (state, approver) = plan_state("reveal", plan_node(true, Felt::ZERO), &[]);
    let (token, id, _) = agent_with_account_and_id(&state).await;
    enrol(&state, &token, "").await;
    grant_forever(&state, &id).await;

    let params = json!({ "owner_address": OWNER, "register": true, "reveal_actions": true });
    let resp = call(&state, Some(&token), PLAN, params).await;
    let p = resp.result.unwrap_or_else(|| panic!("{:?}", resp.error));
    // The grant covered the plan prompt; the reveal prompt still showed.
    assert!(prompted(&approver, PLAN).is_empty());
    let reveal = prompted(&approver, REVEAL);
    assert_eq!(reveal.len(), 1);
    for want in ["channel key", "every past and", "future private payment", "cannot be revoked", "does not let anyone spend"] {
        assert!(reveal[0].contains(want), "reveal prompt lacks {want:?}: {}", reveal[0]);
    }

    assert_eq!(p["actions_revealed"], json!(true));
    let actions = p["client_actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0]["type"], json!("set_viewing_key"));
    assert_eq!(actions[1]["type"], json!("open_channel"));
    assert_eq!(felt_of(&actions[1]["recipient_addr"]), Felt::from_hex(OWNER).unwrap());
    // The felts are serialize_actions: the Span length prefix, then each action.
    let felts: Vec<Felt> = p["client_actions_felts"].as_array().unwrap().iter().map(felt_of).collect();
    assert_eq!(felts[0], Felt::TWO);
    assert_eq!(felts.len(), 1 + 2 + 5);
    // Recompute: actions_hash = poseidon(felts), plan_hash = SNIP-12 of the typed data.
    assert_eq!(strk20::hashes::poseidon_hash_many(&felts), felt_of(&p["actions_hash"]));
    let owner = Felt::from_hex(OWNER).unwrap();
    let again = wallet_rpc::strk20::plan::plan_hash(ChainId::Sepolia, &sepolia_pool(), &owner, &felts).unwrap();
    assert_eq!(again.plan_hash, felt_of(&p["plan_hash"]));
    assert_eq!(typed_data_hash(&state, &token, &p).await, felt_of(&p["plan_hash"]));

    // The request log keeps the plan but not its actions.
    let log = state.log.lock().await.recent(20);
    let entry = log.iter().find(|e| e.method == PLAN).expect("plan logged");
    let logged = entry.result_json.as_deref().unwrap();
    assert!(logged.contains("<redacted>") && logged.contains(p["plan_hash"].as_str().unwrap()), "{logged}");
    let random = actions[0]["random"].as_str().unwrap();
    assert!(!logged.contains(random), "the log holds a planned random value: {logged}");
}

/// Refusing the reveal prompt still returns (and keeps) the plan, without actions.
#[tokio::test]
async fn refusing_the_reveal_returns_the_plan_without_actions() {
    let (state, approver) = plan_state("reveal-no", plan_node(true, Felt::ZERO), &[REVEAL]);
    let token = pair(&state, "app").await;
    enrol(&state, &token, "").await;
    let params = json!({ "owner_address": OWNER, "register": true, "reveal_actions": true });
    let p = call(&state, Some(&token), PLAN, params).await.result.unwrap();
    assert_eq!(prompted(&approver, PLAN).len(), 1);
    assert_eq!(prompted(&approver, REVEAL).len(), 1);
    assert_eq!(p["actions_revealed"], json!(false));
    assert!(p.get("client_actions").is_none() && p.get("client_actions_felts").is_none());
    let d = call(&state, Some(&token), "companion_strk20DiscardPlan", json!({ "plan_id": p["plan_id"] })).await;
    assert_eq!(d.result, Some(json!(true)), "{:?}", d.error);
}

/// A grant covers the plan prompt (D6).
#[tokio::test]
async fn a_grant_covers_the_plan_prompt() {
    let (state, approver) = plan_state("grant", plan_node(true, Felt::ZERO), &[PLAN]);
    let (token, id, _) = agent_with_account_and_id(&state).await;
    enrol(&state, &token, "").await;
    // Without the grant the (rejecting) prompt shows.
    let refused = call(&state, Some(&token), PLAN, json!({ "owner_address": OWNER, "register": true })).await;
    assert_eq!(err_code(&refused), 113);
    assert_eq!(prompted(&approver, PLAN).len(), 1);
    // A refused plan is not stored, so the next one isn't capped by it.
    grant_forever(&state, &id).await;
    let p = call(&state, Some(&token), PLAN, json!({ "owner_address": OWNER, "register": true })).await;
    assert!(p.result.is_some(), "{:?}", p.error);
    assert_eq!(prompted(&approver, PLAN).len(), 1, "the grant covered the second plan");
}

/// One live plan per owner and pool; discarding frees the slot.
#[tokio::test]
async fn one_live_plan_per_owner_and_discard_frees_it() {
    let (state, approver) = plan_state("cap", plan_node(true, Felt::ZERO), &[]);
    let token = pair(&state, "app").await;
    enrol(&state, &token, "").await;
    let params = json!({ "owner_address": OWNER, "register": true });
    let first = call(&state, Some(&token), PLAN, params.clone()).await.result.unwrap();
    let second = call(&state, Some(&token), PLAN, params.clone()).await;
    assert_eq!(err_code(&second), -32006);
    assert_eq!(reason(&second).as_deref(), Some("PLAN_LIMIT"));
    let msg = second.error.unwrap().message;
    assert!(msg.contains(first["plan_id"].as_str().unwrap()), "{msg}");
    // Refused before prompting.
    assert_eq!(prompted(&approver, PLAN).len(), 1);

    let id = json!({ "plan_id": first["plan_id"] });
    let d = call(&state, Some(&token), "companion_strk20DiscardPlan", id.clone()).await;
    assert_eq!(d.result, Some(json!(true)));
    let again = call(&state, Some(&token), "companion_strk20DiscardPlan", id).await;
    assert_eq!(err_code(&again), 114);
    assert_eq!(reason(&again).as_deref(), Some("PLAN_NOT_FOUND"));
    let third = call(&state, Some(&token), PLAN, params).await;
    assert!(third.result.is_some(), "{:?}", third.error);
}

#[tokio::test]
async fn plans_expire_after_their_ttl() {
    let (state, _) = plan_state("ttl", plan_node(true, Felt::ZERO), &[]);
    let token = pair(&state, "app").await;
    enrol(&state, &token, "").await;
    for bad in [json!(0), json!(86_401), json!("60"), json!(1.5)] {
        let params = json!({ "owner_address": OWNER, "register": true, "ttl_secs": bad });
        assert_eq!(err_code(&call(&state, Some(&token), PLAN, params).await), 114, "ttl_secs {bad}");
    }
    let params = json!({ "owner_address": OWNER, "register": true, "ttl_secs": 1 });
    let p = call(&state, Some(&token), PLAN, params).await.result.unwrap();
    assert!(p["expires_at"].as_u64().unwrap() <= wallet_rpc::now_unix_ms() + 1000);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let d = call(&state, Some(&token), "companion_strk20DiscardPlan", json!({ "plan_id": p["plan_id"] })).await;
    assert_eq!(reason(&d).as_deref(), Some("PLAN_NOT_FOUND"));
    // The expired plan no longer holds the owner's slot.
    let again = call(&state, Some(&token), PLAN, json!({ "owner_address": OWNER, "register": true })).await;
    assert!(again.result.is_some(), "{:?}", again.error);
}

/// Plans are bound to their client: another client gets PLAN_NOT_FOUND.
#[tokio::test]
async fn another_client_cannot_discard_a_plan() {
    let (state, _) = plan_state("client", plan_node(true, Felt::ZERO), &[]);
    let (token_a, _, _) = agent_with_account_and_id(&state).await;
    let (token_b, _, _) = agent_with_account_and_id(&state).await;
    enrol(&state, &token_a, "").await;
    let params = json!({ "owner_address": OWNER, "register": true });
    let p = call(&state, Some(&token_a), PLAN, params.clone()).await.result.unwrap();
    let id = json!({ "plan_id": p["plan_id"] });
    // B doesn't have the owner in scope, and can't touch A's plan.
    let b_plan = call(&state, Some(&token_b), PLAN, params).await;
    assert_eq!(reason(&b_plan).as_deref(), Some("OWNER_NOT_ENROLLED"));
    let b = call(&state, Some(&token_b), "companion_strk20DiscardPlan", id.clone()).await;
    assert_eq!(err_code(&b), 114);
    assert_eq!(reason(&b).as_deref(), Some("PLAN_NOT_FOUND"));
    let a = call(&state, Some(&token_a), "companion_strk20DiscardPlan", id).await;
    assert_eq!(a.result, Some(json!(true)));
}

/// D2: locking wipes every plan, and unlocking doesn't bring them back.
#[tokio::test]
async fn locking_the_wallet_wipes_plans() {
    let (state, _) = plan_state("lock", plan_node(true, Felt::ZERO), &[]);
    let token = pair(&state, "app").await;
    enrol(&state, &token, "").await;
    let params = json!({ "owner_address": OWNER, "register": true });
    let p = call(&state, Some(&token), PLAN, params).await.result.unwrap();
    let id = json!({ "plan_id": p["plan_id"] });

    let vault = state.session.lock().await.reseal().unwrap();
    state.session.lock().await.lock();
    let locked = call(&state, Some(&token), "companion_strk20DiscardPlan", id.clone()).await;
    assert_eq!(err_code(&locked), -32001);
    state.session.lock().await.unlock(&vault, "test-pass").unwrap();
    let gone = call(&state, Some(&token), "companion_strk20DiscardPlan", id).await;
    assert_eq!(reason(&gone).as_deref(), Some("PLAN_NOT_FOUND"));
}

/// §2.1: plans also work for strkd accounts, without an SRC5 check; strkd
/// signs at prove time.
#[tokio::test]
async fn plans_work_for_strkd_accounts() {
    let node = plan_node(false, Felt::ZERO);
    let (state, approver) = plan_state("account", node.clone(), &[]);
    let token = pair(&state, "app").await;
    let (_, account) = user_registry();
    let p = call(&state, Some(&token), PLAN, json!({ "register": true })).await;
    let p = p.result.unwrap_or_else(|| panic!("{:?}", p.error));
    assert_eq!(p["subject"], json!("account"));
    assert_eq!(p["owner"], json!(account));
    assert_eq!(p["controller"], json!(account));
    assert_eq!(typed_data_hash(&state, &token, &p).await, felt_of(&p["plan_hash"]));
    let text = &prompted(&approver, PLAN)[0];
    assert!(text.contains("strkd signs with this account") && text.contains(&account), "{text}");
    assert!(node.src5_blocks().is_empty());
    assert!(node.methods().iter().all(|m| m != "starknet_getNonce"));
}

/// The plan checks the owner's state for the intent before prompting.
#[tokio::test]
async fn plan_checks_src5_and_registration_for_the_intent() {
    // SRC5 gone at the reference block.
    let (state, approver) = plan_state("nosrc5", plan_node(false, Felt::ZERO), &[]);
    let token = pair(&state, "app").await;
    enrol(&state, &token, "").await;
    let r = call(&state, Some(&token), PLAN, json!({ "owner_address": OWNER, "register": true })).await;
    assert_eq!(err_code(&r), -32006, "{:?}", r.error);
    assert_eq!(reason(&r).as_deref(), Some("OWNER_NO_CUSTOM_VALIDATION"));
    assert!(r.error.unwrap().message.contains("reference block 988"));
    assert!(prompted(&approver, PLAN).is_empty());

    // Withdrawing for an unregistered owner: 118.
    let (state2, approver2) = plan_state("unreg", plan_node(true, Felt::ZERO), &[]);
    let token2 = pair(&state2, "app").await;
    enrol(&state2, &token2, "").await;
    let w = json!({ "owner_address": OWNER,
                    "actions": [{ "type": "withdraw", "token": "0x4718", "amount": "0x5", "recipient": "0xa11ce" }] });
    assert_eq!(err_code(&call(&state2, Some(&token2), PLAN, w).await), 118);
    assert!(prompted(&approver2, PLAN).is_empty());

    // Registering an owner that is already registered.
    let (state3, approver3) = plan_state("reg", plan_node(true, expected_owner_public_key()), &[]);
    let token3 = pair(&state3, "app").await;
    enrol(&state3, &token3, "").await;
    let r = call(&state3, Some(&token3), PLAN, json!({ "owner_address": OWNER, "register": true })).await;
    assert_eq!(err_code(&r), -32006);
    assert_eq!(reason(&r).as_deref(), Some("ALREADY_REGISTERED"));
    assert!(prompted(&approver3, PLAN).is_empty());
}

#[tokio::test]
async fn plan_validates_its_params() {
    let (state, _) = plan_state("params", plan_node(true, Felt::ZERO), &[]);
    let token = pair(&state, "app").await;
    enrol(&state, &token, "").await;
    let transfer = json!([{ "type": "transfer", "token": "0x4718", "amount": "0x5", "recipient": "0xb0b" }]);
    for params in [
        json!({ "owner_address": OWNER }),
        json!({ "owner_address": OWNER, "register": true, "actions": transfer }),
        json!({ "owner_address": OWNER, "register": "yes" }),
        json!({ "owner_address": OWNER, "register": true, "reveal_actions": 1 }),
    ] {
        assert_eq!(err_code(&call(&state, Some(&token), PLAN, params.clone()).await), 114, "{params}");
    }
    let deposit = json!({ "owner_address": OWNER, "actions": [{ "type": "deposit", "token": "0x4718", "amount": "0x5" }] });
    let r = call(&state, Some(&token), PLAN, deposit).await;
    assert_eq!(err_code(&r), -32601);
    assert_eq!(reason(&r).as_deref(), Some("DEPOSIT_UNSUPPORTED"));
    assert_eq!(err_code(&call(&state, Some(&token), "companion_strk20DiscardPlan", json!({})).await), 114);
}

// ── STRK20 prove (#48, part 3: the prove step) ──────────────────────────────

const PROVE: &str = "companion_strk20Prove";
/// The `apply_actions` payload the recording prover's pool message carries
/// (class hash, then the actions).
const POOL_MESSAGE: [&str; 3] = ["0xc1a55", "0x1", "0x7"];

/// A node for the prove step. Every contract is deployed; the owner advertises
/// SRC5 at `latest` and at numbered blocks up to `src5_until`; the pool's
/// `get_public_key` returns `registered_key` (zero: not registered), its proof
/// window is `validity` blocks and its fee is zero. Submissions are recorded.
struct ProveNode {
    head: std::sync::atomic::AtomicU64,
    validity: u64,
    src5_until: u64,
    registered_key: std::sync::Mutex<Felt>,
    calls: std::sync::Mutex<Vec<(String, Value)>>,
    added: std::sync::Mutex<Vec<AddedInvoke>>,
}

/// `(sender, calldata, signature, proof)` of one `add_invoke`.
type AddedInvoke = (Felt, Vec<Felt>, Vec<Felt>, Option<String>);

impl ProveNode {
    fn set_head(&self, h: u64) {
        self.head.store(h, std::sync::atomic::Ordering::Relaxed);
    }
}

#[async_trait]
impl StarknetRpc for ProveNode {
    async fn raw(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        self.calls.lock().unwrap().push((method.to_string(), params.clone()));
        match method {
            "starknet_blockNumber" => Ok(json!(self.head.load(std::sync::atomic::Ordering::Relaxed))),
            "starknet_getClassHashAt" => Ok(json!("0x1234")),
            "starknet_getNonce" => Ok(json!("0x0")),
            "starknet_call" => {
                let selector = params["request"]["entry_point_selector"].as_str().unwrap();
                let hex_sel = |n: &str| format!("{:#x}", wallet_core::get_selector_from_name(n));
                if selector == hex_sel("supports_interface") {
                    let yes = match params["block_id"]["block_number"].as_u64() {
                        Some(b) => b <= self.src5_until,
                        None => true,
                    };
                    Ok(json!([if yes { "0x1" } else { "0x0" }]))
                } else if selector == hex_sel("get_public_key") {
                    Ok(json!([format!("{:#x}", *self.registered_key.lock().unwrap())]))
                } else if selector == hex_sel("get_proof_validity_blocks") {
                    Ok(json!([format!("{:#x}", self.validity)]))
                } else if selector == hex_sel("get_outgoing_channel_info") {
                    Ok(json!(["0x0", "0x0"]))
                } else {
                    Ok(json!(["0x0"]))
                }
            }
            m => Err(NodeError::Rpc(format!("{m}: unexpected in this test"))),
        }
    }
    async fn get_nonce(&self, _: &Felt) -> Result<Felt, NodeError> {
        Ok(Felt::from(7u64))
    }
    async fn estimate_invoke(&self, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        Ok(FeeBounds {
            l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 7 },
            l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 11 },
            l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 3 },
        })
    }
    async fn add_invoke(
        &self,
        sender: &Felt,
        calldata: &[Felt],
        signature: &[Felt],
        _: &Felt,
        _: &FeeBounds,
        _: &[Felt],
        proof: Option<&str>,
    ) -> Result<Felt, NodeError> {
        self.added.lock().unwrap().push((*sender, calldata.to_vec(), signature.to_vec(), proof.map(str::to_string)));
        Ok(Felt::from_hex("0xabc").unwrap())
    }
    async fn is_deployed(&self, _: &Felt) -> Result<bool, NodeError> {
        Ok(true)
    }
    async fn balance_of(&self, _: &Felt, _: &Felt) -> Result<u128, NodeError> {
        unimplemented!()
    }
    async fn estimate_deploy_account(&self, _: &Felt, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_deploy_account(&self, _: &Felt, _: &[Felt], _: &Felt, _: &[Felt], _: &FeeBounds) -> Result<Felt, NodeError> {
        unimplemented!()
    }
    async fn estimate_declare(&self, _: &Felt, _: &Felt, _: &Value, _: &Felt) -> Result<FeeBounds, NodeError> {
        unimplemented!()
    }
    async fn add_declare(&self, _: &Felt, _: &Felt, _: &Value, _: &[Felt], _: &Felt, _: &FeeBounds) -> Result<Felt, NodeError> {
        unimplemented!()
    }
}

fn prove_node() -> Arc<ProveNode> {
    prove_node_src5_until(u64::MAX)
}

fn prove_node_src5_until(src5_until: u64) -> Arc<ProveNode> {
    Arc::new(ProveNode {
        head: std::sync::atomic::AtomicU64::new(1000),
        validity: 450,
        src5_until,
        registered_key: std::sync::Mutex::new(Felt::ZERO),
        calls: Default::default(),
        added: Default::default(),
    })
}

/// A prover that records each payload and returns a parseable proof whose
/// pool message is [`POOL_MESSAGE`], or fails with `fail_with` (where
/// `{sk}` is replaced by the invocation's `user_sk`, as a prover echoing the
/// transaction would). Its kind can change mid-test.
struct RecordingProver {
    kind: std::sync::Mutex<&'static str>,
    fail_with: std::sync::Mutex<Option<String>>,
    payloads: std::sync::Mutex<Vec<Value>>,
}

#[async_trait]
impl prover::Prover for RecordingProver {
    async fn prove(&self, req: prover::ProveRequest) -> Result<prover::ProveResult, String> {
        let sk = req.payload["transaction"]["calldata"][5].as_str().unwrap_or_default().to_string();
        self.payloads.lock().unwrap().push(req.payload);
        if let Some(e) = self.fail_with.lock().unwrap().clone() {
            return Err(e.replace("{sk}", &sk));
        }
        Ok(prover::ProveResult {
            proof: json!({
                "proof": "AAAA",
                "proof_facts": ["0x1", "0x2"],
                "l2_to_l1_messages": [{ "from_address": wallet_rpc::strk20::SEPOLIA_POOL, "to_address": "0x0",
                                        "payload": POOL_MESSAGE }],
            }),
        })
    }
    fn kind(&self) -> &'static str {
        *self.kind.lock().unwrap()
    }
    fn ready(&self) -> bool {
        true
    }
}

struct ProveFixture {
    state: Arc<ServerState>,
    approver: Arc<RecordingApprover>,
    node: Arc<ProveNode>,
    prover: Arc<RecordingProver>,
    storage_dir: std::path::PathBuf,
}

fn prove_fixture(tag: &str, node: Arc<ProveNode>, reject: &'static [&'static str]) -> ProveFixture {
    let dir = std::env::temp_dir().join(format!("strkd-rpc-prove48-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prover = Arc::new(RecordingProver {
        kind: std::sync::Mutex::new("stub"),
        fail_with: Default::default(),
        payloads: Default::default(),
    });
    let pstate = Arc::new(prover::ProverState {
        prover: prover.clone(),
        jobs: Arc::new(prover::Jobs::new(0)),
        settings: Arc::new(prover::SettingsStore::load(dir.join("settings.json"))),
        storage: Arc::new(prover::Storage::new(dir.join("storage"))),
    });
    let approver = Arc::new(RecordingApprover { reject, prompts: Default::default() });
    let state = Arc::new(
        ServerState::new(Arc::new(Mutex::new(make_session(false))), approver.clone())
            .with_node(ChainId::Sepolia, node.clone())
            .with_prover(pstate),
    );
    ProveFixture { state, approver, node, prover, storage_dir: dir.join("storage") }
}

/// Plan `register: true` for the owner (enrolled first) or, with `None`, for
/// the user's account; returns the plan id.
async fn register_plan(f: &ProveFixture, token: &str, owner: bool) -> String {
    let params = if owner {
        enrol(&f.state, token, "vault").await;
        json!({ "owner_address": OWNER, "register": true })
    } else {
        json!({ "register": true })
    };
    let p = call(&f.state, Some(token), PLAN, params).await;
    p.result.unwrap_or_else(|| panic!("plan: {:?}", p.error))["plan_id"].as_str().unwrap().to_string()
}

/// Poll a job until it is terminal.
async fn poll_job(state: &ServerState, token: &str, job_id: &str) -> Value {
    for _ in 0..500 {
        let r = call(state, Some(token), "companion_proveStatus", json!({ "job_id": job_id })).await;
        let job = r.result.unwrap_or_else(|| panic!("status: {:?}", r.error));
        if job["status"] == json!("succeeded") || job["status"] == json!("failed") {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("job {job_id} did not finish");
}

fn hexes(v: &Value) -> Vec<Felt> {
    v.as_array().unwrap().iter().map(felt_of).collect()
}

/// The happy path for an owner: the caller's signature goes into the virtual
/// tx verbatim (strkd signs nothing), the job is unrecorded, only its client
/// sees it, the result is `{call, proof}`, and the plan is consumed.
#[tokio::test]
async fn owner_prove_passes_the_signature_through_verbatim() {
    let f = prove_fixture("owner-ok", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let other = pair(&f.state, "agent").await;

    let sig = json!(["0xa", "0xb0b", "0x0"]);
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": sig })).await;
    let r = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(r["status"], json!("queued"));
    assert_eq!(r["block_number"], json!(988));
    assert_eq!(r["subject"], json!("owner"));
    assert_eq!(r["submit"], json!("none"));
    let job_id = r["job_id"].as_str().unwrap().to_string();

    // Another client can't see the job.
    let peek = call(&f.state, Some(&other), "companion_proveStatus", json!({ "job_id": job_id })).await;
    assert_eq!(err_code(&peek), 114);

    let job = poll_job(&f.state, &token, &job_id).await;
    assert_eq!(job["status"], json!("succeeded"), "{job}");
    let res = &job["result"];
    assert_eq!(res["call"]["entry_point"], json!("apply_actions"));
    assert_eq!(felt_of(&res["call"]["contract_address"]), sepolia_pool());
    // The pool message minus its class hash, then Option::None (no screening).
    assert_eq!(hexes(&res["call"]["calldata"]), vec![Felt::ONE, Felt::from(7u64), Felt::ONE]);
    assert_eq!(res["proof"]["data"], json!("AAAA"));
    assert_eq!(hexes(&res["proof"]["proof_facts"]), vec![Felt::ONE, Felt::TWO]);

    // What the prover got: the caller's felts as the signature, nothing else.
    let payloads = f.prover.payloads.lock().unwrap().clone();
    assert_eq!(payloads.len(), 1);
    let tx = &payloads[0]["transaction"];
    assert_eq!(hexes(&tx["signature"]), hexes(&sig), "the signature must pass through verbatim");
    assert_eq!(payloads[0]["block_number"], json!(988));
    assert_eq!(felt_of(&tx["sender_address"]), sepolia_pool());
    // calldata = [1, pool, compile_actions, len, owner, user_sk, ...actions]
    assert_eq!(felt_of(&tx["calldata"][4]), Felt::from_hex(OWNER).unwrap());
    // The pool nonce was read at B.
    let nonce_reads: Vec<Value> = f
        .node
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == "starknet_getNonce")
        .map(|(_, p)| p.clone())
        .collect();
    assert_eq!(nonce_reads, vec![json!({ "block_id": { "block_number": 988 }, "contract_address": format!("{:#x}", sepolia_pool()) })]);

    // No prompt: same prover kind, unsubmitted, as the plan prompt said.
    assert!(prompted(&f.approver, PROVE).is_empty());
    // Nothing reached proof storage.
    assert_eq!(std::fs::read_dir(&f.storage_dir).map(|d| d.count()).unwrap_or(0), 0);
    // The plan is consumed.
    let again = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(err_code(&again), 114);
    assert_eq!(reason(&again).as_deref(), Some("PLAN_NOT_FOUND"));
}

/// An empty signature (a contract that only checks its on-chain approval)
/// stays empty.
#[tokio::test]
async fn owner_prove_with_an_empty_signature_signs_nothing() {
    let f = prove_fixture("owner-empty", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    let job_id = r.result.unwrap_or_else(|| panic!("{:?}", r.error))["job_id"].as_str().unwrap().to_string();
    poll_job(&f.state, &token, &job_id).await;
    let payloads = f.prover.payloads.lock().unwrap().clone();
    assert_eq!(payloads[0]["transaction"]["signature"], json!([]));
}

/// For a strkd account, strkd signs the invocation hash with the account key.
#[tokio::test]
async fn account_prove_signs_with_the_account_key() {
    let f = prove_fixture("account", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let (_, account) = user_registry();
    let plan_id = register_plan(&f, &token, false).await;
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id })).await;
    let r = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(r["subject"], json!("account"));
    let job = poll_job(&f.state, &token, r["job_id"].as_str().unwrap()).await;
    assert_eq!(job["status"], json!("succeeded"), "{job}");

    let payloads = f.prover.payloads.lock().unwrap().clone();
    let tx = &payloads[0]["transaction"];
    assert_eq!(felt_of(&tx["calldata"][4]), Felt::from_hex(&account).unwrap());
    // Rebuild the invocation hash and check the signature under the account key.
    let invocation = strk20::invocation::ProofInvocation {
        pool: sepolia_pool(),
        chain: ChainId::Sepolia,
        nonce: felt_of(&tx["nonce"]),
        calldata: hexes(&tx["calldata"]),
    };
    let sig = hexes(&tx["signature"]);
    assert_eq!(sig.len(), 2);
    let pk = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    assert!(starknet_crypto::verify(&pk, &invocation.hash(), &sig[0], &sig[1]).unwrap());
}

/// Owners need a caller signature; accounts must not pass one. Both are 114
/// with a reason, and leave the plan live.
#[tokio::test]
async fn signature_rules_per_subject() {
    let f = prove_fixture("sig-owner", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id })).await;
    assert_eq!(err_code(&r), 114);
    assert_eq!(reason(&r).as_deref(), Some("SIGNATURE_REQUIRED"));
    for bad in [json!("0x1"), json!(["nope"]), json!([1])] {
        let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": bad })).await;
        assert_eq!(err_code(&r), 114, "{bad}");
    }
    let ok = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": ["0x1"] })).await;
    assert!(ok.result.is_some(), "the plan stayed live: {:?}", ok.error);

    let g = prove_fixture("sig-account", prove_node(), &[]);
    let token = pair(&g.state, "app").await;
    let plan_id = register_plan(&g, &token, false).await;
    let r = call(&g.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": ["0x1", "0x2"] })).await;
    assert_eq!(err_code(&r), 114);
    assert_eq!(reason(&r).as_deref(), Some("SIGNATURE_NOT_ALLOWED"));
    assert!(g.prover.payloads.lock().unwrap().is_empty());
}

/// A plan whose subject's state moved since it was made is PLAN_STALE, and
/// is dropped.
#[tokio::test]
async fn a_stale_plan_is_refused_and_dropped() {
    let f = prove_fixture("stale", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    // Someone registered the owner in the meantime.
    *f.node.registered_key.lock().unwrap() = Felt::from(0x1234u64);
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(err_code(&r), -32006, "{:?}", r.error);
    assert_eq!(reason(&r).as_deref(), Some("PLAN_STALE"));
    assert!(f.prover.payloads.lock().unwrap().is_empty());
    let again = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(reason(&again).as_deref(), Some("PLAN_NOT_FOUND"));
}

/// The reference block's bounds, each with its reason; a refused block keeps
/// the plan live.
#[tokio::test]
async fn reference_block_bounds_are_checked() {
    let f = prove_fixture("bounds", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await; // plan_block 988, head 1000
    let prove_at = |b: Value| json!({ "plan_id": plan_id, "signature": [], "block_number": b });

    let r = call(&f.state, Some(&token), PROVE, prove_at(json!(990))).await;
    assert_eq!(reason(&r).as_deref(), Some("BLOCK_TOO_RECENT"));
    assert!(r.error.unwrap().message.contains("Retry in 2 block"));
    let r = call(&f.state, Some(&token), PROVE, prove_at(json!(950))).await;
    assert_eq!(reason(&r).as_deref(), Some("BLOCK_BEFORE_PLAN"));
    assert_eq!(err_code(&call(&f.state, Some(&token), PROVE, prove_at(json!("988"))).await), 114);

    // The head moves on: 988 is now too old for the 450-block window (with
    // 120 kept in hand), the default B is fine.
    f.node.set_head(1400);
    let r = call(&f.state, Some(&token), PROVE, prove_at(json!(988))).await;
    assert_eq!(reason(&r).as_deref(), Some("BLOCK_TOO_OLD"));
    assert!(f.prover.payloads.lock().unwrap().is_empty());
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(r.result.unwrap_or_else(|| panic!("{:?}", r.error))["block_number"], json!(1388));
}

/// The owner dropped SRC5 after the plan (say, an upgrade): refused at B.
#[tokio::test]
async fn src5_is_rechecked_at_the_reference_block() {
    let f = prove_fixture("src5", prove_node_src5_until(995), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    f.node.set_head(1100);
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(err_code(&r), -32006);
    assert_eq!(reason(&r).as_deref(), Some("OWNER_NO_CUSTOM_VALIDATION"));
    assert!(r.error.unwrap().message.contains("reference block 1088"));
    // An explicit B inside the SRC5 range passes.
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [], "block_number": 990 })).await;
    assert!(r.result.is_some(), "{:?}", r.error);
}

/// An owner removed between plan and prove is OWNER_NOT_ENROLLED.
#[tokio::test]
async fn an_owner_removed_after_planning_cannot_be_proved() {
    let f = prove_fixture("removed", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let rm = call(&f.state, Some(&token), "companion_strk20RemoveOwner", json!({ "owner_address": OWNER })).await;
    assert_eq!(rm.result, Some(json!(true)), "{:?}", rm.error);
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(err_code(&r), -32002);
    assert_eq!(reason(&r).as_deref(), Some("OWNER_NOT_ENROLLED"));
    assert!(f.prover.payloads.lock().unwrap().is_empty());
}

/// Another client can't prove (or learn about) a plan.
#[tokio::test]
async fn another_client_cannot_prove_a_plan() {
    let f = prove_fixture("other", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let other = pair(&f.state, "agent").await;
    let r = call(&f.state, Some(&other), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert_eq!(reason(&r).as_deref(), Some("PLAN_NOT_FOUND"));
}

/// Results are wiped when the wallet locks (and stay gone after unlocking);
/// the status is kept.
#[tokio::test]
async fn locking_wipes_prove_results() {
    let f = prove_fixture("lock", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    let job_id = r.result.unwrap()["job_id"].as_str().unwrap().to_string();
    let job = poll_job(&f.state, &token, &job_id).await;
    assert!(job["result"]["proof"].is_object());
    let expires = job["result_expires_at_ms"].as_u64().unwrap() as u128;
    let finished = job["finished_at_ms"].as_u64().unwrap() as u128;
    assert_eq!(expires - finished, prover::UNRECORDED_RESULT_TTL_MS);

    let vault = f.state.session.lock().await.reseal().unwrap();
    f.state.session.lock().await.lock();
    f.state.session.lock().await.unlock(&vault, "test-pass").unwrap();
    let job = poll_job(&f.state, &token, &job_id).await;
    assert_eq!(job["status"], json!("succeeded"));
    assert!(job.get("result").is_none(), "{job}");
    assert_eq!(job["result_dropped"], json!("wiped_on_lock"));
}

/// `submit: "account"` prompts (naming the public link), submits from the
/// owner's controller, and returns the transaction hash.
#[tokio::test]
async fn submit_account_prompts_and_submits_from_the_controller() {
    let f = prove_fixture("submit", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let (_, controller) = user_registry();
    let plan_id = register_plan(&f, &token, true).await;

    // Validation first.
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [], "submit": "relay" })).await;
    assert_eq!(err_code(&r), -32601);
    assert_eq!(reason(&r).as_deref(), Some("RELAY_UNSUPPORTED"));
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [], "submitter_address": controller })).await;
    assert_eq!(err_code(&r), 114);
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [], "submit": "account", "submitter_address": "0xdead" })).await;
    assert_eq!(err_code(&r), -32002);

    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": ["0x5"], "submit": "account" })).await;
    let r = r.result.unwrap_or_else(|| panic!("{:?}", r.error));
    assert_eq!(r["submit"], json!("account"));
    assert_eq!(r["submitter_address"], json!(controller));
    let prompts = prompted(&f.approver, PROVE);
    assert_eq!(prompts.len(), 1);
    for want in [&controller, OWNER, "links", "publicly", "pool's fee"] {
        assert!(prompts[0].contains(want), "prompt lacks {want:?}: {}", prompts[0]);
    }

    let job = poll_job(&f.state, &token, r["job_id"].as_str().unwrap()).await;
    assert_eq!(job["status"], json!("succeeded"), "{job}");
    assert_eq!(felt_of(&job["result"]["transaction_hash"]), Felt::from_hex("0xabc").unwrap());
    assert_eq!(job["result"]["submitter_address"], json!(controller));
    let added = f.node.added.lock().unwrap().clone();
    assert_eq!(added.len(), 1);
    let (sender, calldata, signature, proof) = &added[0];
    assert_eq!(*sender, Felt::from_hex(&controller).unwrap());
    assert_eq!(proof.as_deref(), Some("AAAA"));
    // One call (the pool fee is zero, so no approve): apply_actions on the pool.
    assert_eq!(calldata[0], Felt::ONE);
    assert_eq!(calldata[1], sepolia_pool());
    assert_eq!(calldata[2], wallet_core::get_selector_from_name("apply_actions"));
    // The controller signed its own (outer) submission, not the virtual tx.
    assert_eq!(signature.len(), 2);
    let payloads = f.prover.payloads.lock().unwrap().clone();
    assert_eq!(hexes(&payloads[0]["transaction"]["signature"]), vec![Felt::from(5u64)]);
}

/// A refused submission prompt is 113 and keeps the plan.
#[tokio::test]
async fn refusing_the_prove_prompt_keeps_the_plan() {
    let f = prove_fixture("refuse", prove_node(), &[PROVE]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [], "submit": "account" })).await;
    assert_eq!(err_code(&r), 113);
    assert!(f.prover.payloads.lock().unwrap().is_empty());
    let d = call(&f.state, Some(&token), "companion_strk20DiscardPlan", json!({ "plan_id": plan_id })).await;
    assert_eq!(d.result, Some(json!(true)));
}

/// A prover switch since the plan prompts again, naming both kinds.
#[tokio::test]
async fn a_prover_change_since_the_plan_prompts() {
    let f = prove_fixture("kind", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    *f.prover.kind.lock().unwrap() = "remote";
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": [] })).await;
    assert!(r.result.is_some(), "{:?}", r.error);
    let prompts = prompted(&f.approver, PROVE);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("then: stub, now: remote") && prompts[0].contains("viewing key"), "{}", prompts[0]);
}

/// The pool rejecting the owner's signature fails the job with
/// OWNER_SIGNATURE_REJECTED, with the viewing key scrubbed from the text.
#[tokio::test]
async fn a_rejected_owner_signature_fails_the_job_with_a_reason() {
    let f = prove_fixture("badsig", prove_node(), &[]);
    let token = pair(&f.state, "app").await;
    let plan_id = register_plan(&f, &token, true).await;
    *f.prover.fail_with.lock().unwrap() = Some(
        "the transaction reverted while it was proved ... calldata [{sk}] revert reason: \
         0x494e56414c49445f5349474e4154555245 ('INVALID_SIGNATURE')"
            .into(),
    );
    let r = call(&f.state, Some(&token), PROVE, json!({ "plan_id": plan_id, "signature": ["0x1"] })).await;
    let job = poll_job(&f.state, &token, r.result.unwrap()["job_id"].as_str().unwrap()).await;
    assert_eq!(job["status"], json!("failed"));
    assert_eq!(job["error_reason"], json!("OWNER_SIGNATURE_REJECTED"));
    let err = job["error"].as_str().unwrap();
    assert!(err.starts_with("OWNER_SIGNATURE_REJECTED: ") && err.contains("<viewing key>"), "{err}");
    let sk = f.prover.payloads.lock().unwrap()[0]["transaction"]["calldata"][5].as_str().unwrap().to_string();
    assert!(!err.contains(&sk), "{err}");
}

/// Every proving job is visible to its own client only (companion_prove too).
#[tokio::test]
async fn companion_prove_jobs_are_visible_to_their_client_only() {
    let state = state_with_prover("scoped");
    let a = pair(&state, "app").await;
    let b = pair(&state, "agent").await;
    let r = call(&state, Some(&a), "companion_prove", json!({ "payload": { "x": 1 } })).await;
    let job_id = r.result.unwrap()["job_id"].as_str().unwrap().to_string();
    let theirs = call(&state, Some(&b), "companion_proveStatus", json!({ "job_id": job_id })).await;
    assert_eq!(err_code(&theirs), 114);
    let mine = call(&state, Some(&a), "companion_proveStatus", json!({ "job_id": job_id })).await;
    assert!(mine.result.is_some(), "{:?}", mine.error);
}
