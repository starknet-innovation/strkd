//! STRK20 pool selection (#57): the per-network pool from Settings → STRK20
//! reaches every method, approval prompts name a non-canonical pool, callers
//! can't redirect the pool, and a testnet pool's local screener key attests
//! deposits without ever leaving strkd (and is refused on mainnet).
//!
//! Dispatch-level, against a mock node and a stub prover: no network, no
//! chain. Keys: the published BIP-39 all-`abandon` vector and starknet-privacy's
//! public test screener key `0xCAFEBABE`. Never real key material.

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use wallet_core::{address_hex, get_selector_from_name, oz_address, AccountRef, ChainId, Domain, Felt, Registry, ResourceBounds};
use wallet_rpc::strk20::{SEPOLIA_POOL, STRK};
use wallet_rpc::{dispatch, ChannelApprover, Decision, FeeBounds, NodeError, Request, Response, ServerState, StarknetRpc, WalletSession};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
/// strkd's self-deployed Sepolia test pool (mainnet class, screener `0xCAFEBABE`).
const TEST_POOL: &str = "0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323";
const TEST_SCREENER: &str = "0xCAFEBABE";

fn felt(s: &str) -> Felt {
    Felt::from_hex(s).unwrap()
}

fn user_address() -> String {
    address_hex(&oz_address(TEST_MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap())
}

fn screener_public_key() -> Felt {
    wallet_core::public_key_of_key(&felt(TEST_SCREENER)).unwrap()
}

/// A pool on a mock node. Records every contract `starknet_call` reads and every
/// submitted invoke's calldata.
struct PoolNode {
    registered: bool,
    screener: Felt,
    calls: StdMutex<Vec<(Felt, Felt)>>,
    submitted: StdMutex<Vec<Vec<Felt>>>,
}

impl PoolNode {
    fn new(registered: bool, screener: Felt) -> Arc<Self> {
        Arc::new(PoolNode { registered, screener, calls: Default::default(), submitted: Default::default() })
    }

    /// Contracts read, except ERC-20 balance checks.
    fn pools_read(&self) -> Vec<Felt> {
        let balance_of = get_selector_from_name("balance_of");
        self.calls.lock().unwrap().iter().filter(|(_, sel)| *sel != balance_of).map(|(c, _)| *c).collect()
    }
}

fn bounds() -> FeeBounds {
    FeeBounds {
        l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 7 },
        l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 11 },
        l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 3 },
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[async_trait]
impl StarknetRpc for PoolNode {
    async fn raw(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        match method {
            "starknet_blockNumber" => Ok(json!(1000)),
            "starknet_getClassHashAt" => Ok(json!("0x1234")),
            "starknet_getNonce" => Ok(json!("0x5")),
            "starknet_getBlockWithTxHashes" => Ok(json!({ "timestamp": now() })),
            "starknet_call" => {
                let contract = felt(params["request"]["contract_address"].as_str().unwrap());
                let selector = felt(params["request"]["entry_point_selector"].as_str().unwrap());
                self.calls.lock().unwrap().push((contract, selector));
                let s = |n: &str| get_selector_from_name(n);
                Ok(if selector == s("get_public_key") && self.registered {
                    // Any valid curve x will do as a registered viewing key.
                    json!([format!("{:#x}", screener_public_key()), "0x0"])
                } else if selector == s("get_screener_public_key") {
                    json!([format!("{:#x}", self.screener)])
                } else if selector == s("balance_of") {
                    json!(["0xffffffffffffffffffff", "0x0"])
                } else {
                    json!(["0x0", "0x0"])
                })
            }
            m => Err(NodeError::Rpc(format!("{m}: unexpected in this test"))),
        }
    }
    async fn get_nonce(&self, _: &Felt) -> Result<Felt, NodeError> {
        Ok(Felt::from(7u64))
    }
    async fn estimate_invoke(&self, _: &Felt, _: &[Felt], _: &Felt) -> Result<FeeBounds, NodeError> {
        Ok(bounds())
    }
    async fn add_invoke(
        &self,
        _: &Felt,
        calldata: &[Felt],
        _: &[Felt],
        _: &Felt,
        _: &FeeBounds,
        _: &[Felt],
        _: Option<&str>,
    ) -> Result<Felt, NodeError> {
        self.submitted.lock().unwrap().push(calldata.to_vec());
        Ok(felt("0xabc"))
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

/// A prover that "proves" any STRK20 invocation: the pool's message carries a
/// class hash and one server action, and no screening attestation (a local
/// proof never has one).
struct PoolStubProver;

#[async_trait]
impl prover::Prover for PoolStubProver {
    async fn prove(&self, req: prover::ProveRequest) -> Result<prover::ProveResult, String> {
        let pool = req.payload["transaction"]["sender_address"].clone();
        Ok(prover::ProveResult {
            proof: json!({
                "proof": "AAAA", "proof_facts": ["0x1", "0x2"],
                "l2_to_l1_messages": [{ "from_address": pool, "to_address": "0x0", "payload": ["0xc1a55", "0x1"] }],
            }),
        })
    }
    fn kind(&self) -> &'static str {
        "native"
    }
    fn ready(&self) -> bool {
        true
    }
}

struct Harness {
    state: Arc<ServerState>,
    token: String,
    prompts: Arc<StdMutex<Vec<(String, String)>>>,
}

/// A server with `node` on both networks, the STRK20 settings `edit` applies,
/// and an approver that records each prompt and refuses `reject`'s methods.
async fn harness(tag: &str, node: Arc<PoolNode>, reject: &'static [&'static str], edit: impl FnOnce(&mut prover::Settings)) -> Harness {
    let prompts: Arc<StdMutex<Vec<(String, String)>>> = Default::default();
    let (approver, mut rx) = ChannelApprover::new(16);
    let seen = prompts.clone();
    tokio::spawn(async move {
        while let Some(pending) = rx.recv().await {
            seen.lock().unwrap().push((pending.request.method.clone(), pending.request.summary.clone()));
            let decision = if reject.contains(&pending.request.method.as_str()) { Decision::Reject } else { Decision::Approve };
            let _ = pending.respond.send(decision);
        }
    });
    let dir = std::env::temp_dir().join(format!("strkd-strk20-pool-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let settings = prover::SettingsStore::load(dir.join("settings.json"));
    let mut s = settings.get().await;
    edit(&mut s);
    settings.update(s).await.unwrap();
    let pstate = Arc::new(prover::ProverState {
        prover: Arc::new(PoolStubProver),
        jobs: Arc::new(prover::Jobs::new(0)),
        settings: Arc::new(settings),
        storage: Arc::new(prover::Storage::new(dir.join("storage"))),
    });
    let mut reg = Registry::default();
    reg.add(AccountRef {
        domain: Domain::User,
        index: 0,
        address: user_address(),
        label: "Main".into(),
        contract: Default::default(),
        owner_client_id: None,
    });
    let session = WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "test-pass", reg);
    let state = Arc::new(ServerState::new(Arc::new(Mutex::new(session)), Arc::new(approver)).with_prover(pstate));
    state.set_node(ChainId::Sepolia, Some(node.clone()));
    state.set_node(ChainId::Mainnet, Some(node));
    let token = call(&state, None, "companion_requestPairing", json!({ "name": "pool-test", "kind": "app" })).await.result.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    Harness { state, token, prompts }
}

async fn call(state: &ServerState, token: Option<&str>, method: &str, params: Value) -> Response {
    dispatch(state, token, Request { jsonrpc: Some("2.0".into()), method: method.into(), params, id: json!(1) }).await
}

impl Harness {
    async fn call(&self, method: &str, params: Value) -> Response {
        call(&self.state, Some(&self.token), method, params).await
    }
    fn prompt(&self, method: &str) -> String {
        self.prompts.lock().unwrap().iter().find(|(m, _)| m == method).map(|(_, s)| s.clone()).unwrap_or_default()
    }
}

fn code(r: &Response) -> i64 {
    r.error.as_ref().map(|e| e.code).unwrap_or(0)
}

fn use_test_pool(s: &mut prover::Settings) {
    s.testnet.strk20.pool = TEST_POOL.into();
}

fn deposit() -> Value {
    json!({ "actions": [{ "type": "deposit", "token": STRK, "amount": "0xde0b6b3a7640000" }] })
}

// Assertion messages carry method names and lengths only, never prompt text or
// addresses.

/// Every STRK20 method reads the configured pool, never the canonical one.
#[tokio::test]
async fn every_method_reads_the_configured_pool() {
    let pool = felt(TEST_POOL);
    let cases: [(&str, bool, Value, i64); 4] = [
        // Unregistered: balances stop at NOT_REGISTERED after reading the pool.
        ("wallet_strk20Balances", false, json!({ "tokens": [STRK] }), 118),
        // Register reaches its prompt (refused) after discovery on the pool.
        ("companion_strk20Register", false, json!({}), 113),
        // A transfer with no notes: insufficient private balance, read from the pool.
        ("wallet_strk20PrepareInvoke", true, json!({ "actions": [{ "type": "transfer", "token": STRK, "amount": "0x1", "recipient": "0xb0b" }] }), 119),
        // A deposit, screened locally, reaches its prompt (refused).
        ("wallet_strk20InvokeTransaction", true, deposit(), 113),
    ];
    for (method, registered, params, want) in cases {
        let node = PoolNode::new(registered, screener_public_key());
        let h = harness(&format!("route-{method}"), node.clone(), &["companion_strk20Register", "wallet_strk20InvokeTransaction"], |s| {
            use_test_pool(s);
            s.testnet.strk20.screener_key = TEST_SCREENER.into();
        })
        .await;
        let r = h.call(method, params).await;
        assert_eq!(code(&r), want, "{method}");
        let read = node.pools_read();
        assert!(!read.is_empty(), "{method}: read no pool");
        assert!(read.iter().all(|c| *c == pool), "{method}: read {read:?}");
    }
}

/// Approval prompts name a non-canonical pool as such; the canonical pool is
/// named plainly.
#[tokio::test]
async fn prompts_flag_a_non_canonical_pool() {
    let h = harness("prompt-custom", PoolNode::new(false, screener_public_key()), &["companion_strk20Register", "wallet_strk20Balances"], use_test_pool).await;
    assert_eq!(code(&h.call("wallet_strk20Balances", json!({ "tokens": [STRK] })).await), 113);
    assert_eq!(code(&h.call("companion_strk20Register", json!({})).await), 113);
    let custom = format!("NON-CANONICAL pool {:#x}", felt(TEST_POOL));
    for m in ["wallet_strk20Balances", "companion_strk20Register"] {
        assert!(h.prompt(m).contains(&custom), "{m}");
    }

    let h = harness("prompt-canonical", PoolNode::new(false, screener_public_key()), &["companion_strk20Register"], |_| {}).await;
    assert_eq!(code(&h.call("companion_strk20Register", json!({})).await), 113);
    let p = h.prompt("companion_strk20Register");
    assert!(p.contains(&format!("pool {:#x}", felt(SEPOLIA_POOL))) && !p.contains("NON-CANONICAL"), "prompt len {}", p.len());
}

/// The plan step (#48) reads the configured pool, flags it in its prompt, and
/// takes the same `pool_address` assertion.
#[tokio::test]
async fn plan_uses_and_flags_the_configured_pool() {
    let node = PoolNode::new(false, screener_public_key());
    let h = harness("plan", node.clone(), &["companion_strk20Plan"], use_test_pool).await;
    let r = h.call("companion_strk20Plan", json!({ "pool_address": SEPOLIA_POOL, "register": true })).await;
    assert_eq!(code(&r), 114);
    assert!(node.pools_read().is_empty());

    let r = h.call("companion_strk20Plan", json!({ "pool_address": TEST_POOL, "register": true })).await;
    assert_eq!(code(&r), 113);
    let read = node.pools_read();
    assert!(!read.is_empty() && read.iter().all(|c| *c == felt(TEST_POOL)), "{} reads", read.len());
    let p = h.prompt("companion_strk20Plan");
    assert!(p.contains(&format!("NON-CANONICAL pool {:#x}", felt(TEST_POOL))), "prompt len {}", p.len());
}

/// A caller can't point strkd at another contract: `pool_address` must be the
/// configured pool, and only asserts it.
#[tokio::test]
async fn pool_address_param_must_match_the_configured_pool() {
    let h = harness("pool-param", PoolNode::new(false, screener_public_key()), &["companion_strk20Register"], use_test_pool).await;
    for method in ["wallet_strk20Balances", "companion_strk20Register", "wallet_strk20PrepareInvoke", "wallet_strk20InvokeTransaction"] {
        let mut params = deposit();
        params["tokens"] = json!([STRK]);
        params["pool_address"] = json!(SEPOLIA_POOL);
        let r = h.call(method, params).await;
        assert_eq!(code(&r), 114, "{method}");
        assert!(r.error.unwrap().message.contains("Settings → STRK20"));
    }
    assert!(h.prompts.lock().unwrap().iter().all(|(m, _)| m == "companion_requestPairing"));
    // The configured pool, in any spelling, passes.
    let r = h.call("companion_strk20Register", json!({ "pool_address": "0x3016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323" })).await;
    assert_eq!(code(&r), 113);
}

/// The full local-screener deposit: proved by the (stub) local prover, attested
/// with the test screener key, submitted from the account. The attestation
/// verifies under the pool's screener key for this depositor, and the key never
/// shows up in the result, the prompt or the request log.
#[tokio::test]
async fn local_screener_attests_a_testnet_deposit() {
    let node = PoolNode::new(true, screener_public_key());
    let h = harness("screened", node.clone(), &[], |s| {
        use_test_pool(s);
        s.testnet.strk20.screener_key = TEST_SCREENER.into();
    })
    .await;
    let r = h.call("wallet_strk20InvokeTransaction", deposit()).await;
    assert!(r.error.is_none());
    assert_eq!(r.result.as_ref().unwrap()["transaction_hash"], json!("0xabc"));
    // A deposit creates only a note back to the depositor: no locator (#49).
    assert_eq!(r.result.as_ref().unwrap()["notes"], json!([]));

    let prompt = h.prompt("wallet_strk20InvokeTransaction");
    assert!(prompt.contains("proved on this machine, screened with the test screener key"), "prompt len {}", prompt.len());
    assert!(prompt.contains("NON-CANONICAL pool"), "prompt len {}", prompt.len());

    // apply_actions is the last call: its calldata ends with Some(attestation).
    let submitted = node.submitted.lock().unwrap().clone();
    assert_eq!(submitted.len(), 1);
    let tail = &submitted[0][submitted[0].len() - 4..];
    assert_eq!(tail[0], Felt::ZERO, "Option::Some");
    let issued_at = u64::try_from(tail[1]).unwrap();
    assert!(issued_at.abs_diff(now()) < 60);
    let pk = screener_public_key();
    let hash = strk20::screening::message_hash(ChainId::Sepolia.as_felt(), felt(&user_address()), issued_at, pk);
    assert!(starknet_crypto::verify(&pk, &hash, &tail[2], &tail[3]).unwrap(), "attestation must verify under the pool's key");

    // The key appears nowhere a caller or the log can see.
    let needles = ["cafebabe", "CAFEBABE", &Felt::from(0xCAFEBABEu64).to_string()];
    let mut seen = serde_json::to_string(&r).unwrap();
    seen.push_str(&prompt);
    for e in h.state.log.lock().await.recent(50) {
        seen.push_str(&format!("{} {} {:?} {:?}", e.method, e.outcome, e.params_json, e.result_json));
    }
    for n in needles {
        assert!(!seen.contains(n), "screener key leaked: {n}");
    }
}

/// A key that isn't the pool's screener fails before the prompt, not after a
/// wasted proof.
#[tokio::test]
async fn a_screener_key_for_another_pool_is_refused_up_front() {
    let node = PoolNode::new(true, felt("0x2159dc65cc026d7af0934a6562263c9b638b411887cbc350b566b5e2801bf5"));
    let h = harness("mismatch", node.clone(), &[], |s| {
        use_test_pool(s);
        s.testnet.strk20.screener_key = TEST_SCREENER.into();
    })
    .await;
    let r = h.call("wallet_strk20InvokeTransaction", deposit()).await;
    assert_eq!(code(&r), -32006);
    let msg = r.error.unwrap().message;
    assert!(msg.contains("not pool") && !msg.to_lowercase().contains("cafebabe"), "message len {}", msg.len());
    assert!(h.prompt("wallet_strk20InvokeTransaction").is_empty());
    assert!(node.submitted.lock().unwrap().is_empty());
}

/// A screener key on mainnet is refused, even if written to settings.json by hand.
#[tokio::test]
async fn a_mainnet_screener_key_is_refused() {
    let node = PoolNode::new(true, screener_public_key());
    let h = harness("mainnet", node.clone(), &[], |s| {
        s.mainnet.strk20.pool = TEST_POOL.into();
        s.mainnet.strk20.screener_key = TEST_SCREENER.into();
    })
    .await;
    let mut params = deposit();
    params["chainId"] = json!("0x534e5f4d41494e"); // SN_MAIN
    let r = h.call("wallet_strk20InvokeTransaction", params.clone()).await;
    assert_eq!(code(&r), -32006);
    assert!(r.error.unwrap().message.contains("testnet-only"));
    // PrepareInvoke takes the same path.
    let r = h.call("wallet_strk20PrepareInvoke", params).await;
    assert_eq!(code(&r), -32006);
    assert!(h.prompt("wallet_strk20InvokeTransaction").is_empty());
    assert!(node.submitted.lock().unwrap().is_empty());
}
