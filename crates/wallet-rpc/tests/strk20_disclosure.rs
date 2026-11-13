//! STRK20 note disclosure (#49): `notes` locators on the batch methods, and
//! `companion_strk20ChannelKey`.
//!
//! Dispatch-level, against an in-memory pool whose storage is built with the
//! pool's own hashes and encryptions (`strk20::hashes`, `strk20::crypto`), so
//! discovery, planning and the channel key run the real code paths. No network.
//! Keys: the published BIP-39 all-`abandon` vector and small fixed test
//! scalars. Never real key material.
//!
//! Assertion messages never print addresses, keys, prompt text or results
//! (CodeQL flags them): only indices and lengths.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use strk20::{crypto, hashes};
use tokio::sync::Mutex;
use wallet_core::{
    address_hex, get_selector_from_name, oz_address, AccountRef, ChainId, Domain, Felt, Registry,
    ResourceBounds,
};
use wallet_rpc::strk20::{SEPOLIA_POOL, STRK};
use wallet_rpc::{
    dispatch, Decision, FeeBounds, NodeError, Request, Response, ServerState, StarknetRpc,
    WalletSession,
};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const OWNER: &str = "0x0000000000000000000000000000000000000000000000000000000000c0ffee";
const METHOD: &str = "companion_strk20ChannelKey";
const PLAN: &str = "companion_strk20Plan";

fn felt(s: &str) -> Felt {
    Felt::from_hex(s).unwrap()
}
fn felt_of(v: &Value) -> Felt {
    felt(v.as_str().expect("a hex string"))
}
fn pool() -> Felt {
    felt(SEPOLIA_POOL)
}
fn strk() -> Felt {
    felt(STRK)
}
fn user() -> Felt {
    oz_address(TEST_MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap()
}
fn user_sk() -> Felt {
    wallet_core::strk20_viewing_key(
        TEST_MNEMONIC,
        Domain::User,
        0,
        None,
        &ChainId::Sepolia.as_felt(),
        &pool(),
    )
    .unwrap()
}
fn owner() -> Felt {
    felt(OWNER)
}
fn owner_sk() -> Felt {
    wallet_core::strk20_owner_viewing_key(
        TEST_MNEMONIC,
        Domain::User,
        0,
        None,
        &ChainId::Sepolia.as_felt(),
        &pool(),
        &owner(),
    )
    .unwrap()
}
/// A registered recipient with a channel from the user.
fn bob() -> Felt {
    Felt::from(0xb0b_u64)
}
fn bob_pk() -> Felt {
    crypto::public_key(Felt::from(0x5eed2_u64))
}
/// A registered recipient nobody has sent to.
fn carol() -> Felt {
    Felt::from(0xca201_u64)
}

/// Every lowercase hex spelling of `f` a JSON string could carry.
fn spellings(f: Felt) -> [String; 2] {
    [format!("{f:x}"), format!("{f:064x}")]
}

// ── an in-memory pool ────────────────────────────────────────────────────────

/// `(selector, calldata)` → the view's output felts.
type Views = HashMap<(Felt, Vec<Felt>), Vec<Felt>>;

/// A node whose `starknet_call` answers from a table keyed by
/// `(selector, calldata)`; anything not in it reads as zeros, like an empty
/// storage slot. The contract address is not checked.
struct FakePool {
    views: StdMutex<Views>,
    outgoing: StdMutex<HashMap<Felt, u32>>,
    incoming: StdMutex<HashMap<Felt, u64>>,
}

impl FakePool {
    fn new() -> Arc<Self> {
        let pool = FakePool {
            views: Default::default(),
            outgoing: Default::default(),
            incoming: Default::default(),
        };
        pool.set(
            "get_proof_validity_blocks",
            vec![],
            vec![Felt::from(450u64)],
        );
        Arc::new(pool)
    }

    fn set(&self, name: &str, calldata: Vec<Felt>, out: Vec<Felt>) {
        self.views
            .lock()
            .unwrap()
            .insert((get_selector_from_name(name), calldata), out);
    }

    fn get(&self, name: &str, calldata: Vec<Felt>) -> Option<Vec<Felt>> {
        self.views
            .lock()
            .unwrap()
            .get(&(get_selector_from_name(name), calldata))
            .cloned()
    }

    fn register(&self, who: Felt, pk: Felt) {
        self.set("get_public_key", vec![who], vec![pk]);
    }

    /// Make the contract at [`OWNER`] enrollable: it advertises SRC5 custom
    /// validation (the address is not checked).
    fn advertise_custom_validation(&self) {
        self.set(
            "supports_interface",
            vec![felt(
                wallet_rpc::strk20::owners::CUSTOM_SIGNATURE_INTERFACE_ID,
            )],
            vec![Felt::ONE],
        );
    }

    /// Write what the pool's `open_channel`, `open_subchannel` and
    /// `create_enc_note` would: the channel marker, the sender's outgoing slot,
    /// the recipient's encrypted channel info, one subchannel per token, and
    /// each note's packed value. Returns the channel key.
    fn open_channel(
        &self,
        sender: Felt,
        sender_sk: Felt,
        recipient: Felt,
        recipient_pk: Felt,
        tokens: &[(Felt, &[u128])],
    ) -> Felt {
        let ck = hashes::channel_key(sender, sender_sk, recipient, recipient_pk);
        let marker = hashes::channel_marker(ck, sender, recipient, recipient_pk);
        self.set("channel_exists", vec![marker], vec![Felt::ONE]);

        let out_index = {
            let mut m = self.outgoing.lock().unwrap();
            let n = m.entry(sender).or_default();
            *n += 1;
            *n - 1
        };
        let id = hashes::outgoing_channel_id(sender, sender_sk, out_index);
        self.set(
            "get_outgoing_channel_info",
            vec![id],
            vec![Felt::from(0x77u64), Felt::ONE],
        );

        let in_index = {
            let mut m = self.incoming.lock().unwrap();
            let n = m.entry(recipient).or_default();
            *n += 1;
            *n - 1
        };
        self.set(
            "get_num_of_channels",
            vec![recipient],
            vec![Felt::from(in_index + 1)],
        );
        let ephemeral = Felt::from(0x5eed1 + in_index);
        let shared = crypto::shared_x(ephemeral, recipient_pk).unwrap();
        self.set(
            "get_channel_info",
            vec![recipient, Felt::from(in_index)],
            vec![
                crypto::public_key(ephemeral),
                hashes::enc_channel_key_hash(shared) + ck,
                hashes::enc_sender_addr_hash(shared) + sender,
            ],
        );

        for (k, (token, amounts)) in tokens.iter().enumerate() {
            let k = k as u32;
            let salt = Felt::from(0x51 + k as u64);
            self.set(
                "get_subchannel_info",
                vec![hashes::subchannel_id(ck, k)],
                vec![salt, hashes::enc_token_hash(ck, k, salt) + *token],
            );
            for (i, &amount) in amounts.iter().enumerate() {
                let i = i as u32;
                let packed = crypto::encrypt_note_value(amount, 0xabc + i as u128, ck, *token, i);
                self.set(
                    "get_note",
                    vec![hashes::note_id(ck, *token, i)],
                    vec![packed, *token],
                );
            }
        }
        ck
    }
}

fn bounds() -> FeeBounds {
    FeeBounds {
        l1_gas: ResourceBounds {
            max_amount: 100,
            max_price_per_unit: 7,
        },
        l2_gas: ResourceBounds {
            max_amount: 200_000,
            max_price_per_unit: 11,
        },
        l1_data_gas: ResourceBounds {
            max_amount: 100,
            max_price_per_unit: 3,
        },
    }
}

#[async_trait]
impl StarknetRpc for FakePool {
    async fn raw(&self, method: &str, params: Value) -> Result<Value, NodeError> {
        match method {
            "starknet_blockNumber" => Ok(json!(1000)),
            "starknet_getClassHashAt" => Ok(json!("0x1234")),
            "starknet_getNonce" => Ok(json!("0x0")),
            "starknet_call" => {
                let selector = felt(params["request"]["entry_point_selector"].as_str().unwrap());
                let calldata: Vec<Felt> = params["request"]["calldata"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(felt_of)
                    .collect();
                let out = self
                    .views
                    .lock()
                    .unwrap()
                    .get(&(selector, calldata))
                    .cloned()
                    .unwrap_or_else(|| vec![Felt::ZERO; 3]);
                Ok(json!(out
                    .iter()
                    .map(|f| format!("{f:#x}"))
                    .collect::<Vec<_>>()))
            }
            m => Err(NodeError::Rpc(format!("{m}: unexpected in this test"))),
        }
    }
    async fn get_nonce(&self, _: &Felt) -> Result<Felt, NodeError> {
        Ok(Felt::from(7u64))
    }
    async fn estimate_invoke(
        &self,
        _: &Felt,
        _: &[Felt],
        _: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(bounds())
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
        Ok(felt("0xabc"))
    }
    async fn is_deployed(&self, _: &Felt) -> Result<bool, NodeError> {
        Ok(true)
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

/// The user registered, with 100 STRK on its self-channel and a channel to bob
/// carrying two STRK notes (so bob's next note index is 2); bob and carol
/// registered. Returns the pool and the user→bob channel key.
fn funded_pool() -> (Arc<FakePool>, Felt) {
    let p = FakePool::new();
    let (u, sk) = (user(), user_sk());
    let pk = crypto::public_key(sk);
    p.register(u, pk);
    p.register(bob(), bob_pk());
    p.register(carol(), crypto::public_key(Felt::from(0x5eed3_u64)));
    p.open_channel(u, sk, u, pk, &[(strk(), &[100])]);
    let ck = p.open_channel(u, sk, bob(), bob_pk(), &[(strk(), &[5, 7])]);
    (p, ck)
}

/// A prover whose pool message carries a class hash and one server action.
struct StubProver;

#[async_trait]
impl prover::Prover for StubProver {
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

/// Records every prompt and rejects the listed methods.
struct RecordingApprover {
    reject: &'static [&'static str],
    prompts: StdMutex<Vec<wallet_rpc::ApprovalRequest>>,
}

#[async_trait]
impl wallet_rpc::Approver for RecordingApprover {
    async fn request_approval(&self, req: wallet_rpc::ApprovalRequest) -> Decision {
        let decision = if self.reject.contains(&req.method.as_str()) {
            Decision::Reject
        } else {
            Decision::Approve
        };
        self.prompts.lock().unwrap().push(req);
        decision
    }
}

impl RecordingApprover {
    fn prompts(&self, method: &str) -> Vec<String> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.method == method)
            .map(|p| p.summary.clone())
            .collect()
    }
}

struct Fixture {
    state: Arc<ServerState>,
    approver: Arc<RecordingApprover>,
}

fn fixture(tag: &str, node: Arc<FakePool>, reject: &'static [&'static str]) -> Fixture {
    let dir =
        std::env::temp_dir().join(format!("strkd-rpc-disclosure-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let pstate = Arc::new(prover::ProverState {
        prover: Arc::new(StubProver),
        jobs: Arc::new(prover::Jobs::new(0)),
        settings: Arc::new(prover::SettingsStore::load(dir.join("settings.json"))),
        storage: Arc::new(prover::Storage::new(dir.join("storage"))),
    });
    let mut reg = Registry::default();
    reg.add(AccountRef {
        domain: Domain::User,
        index: 0,
        address: address_hex(&user()),
        label: "Main".into(),
        contract: Default::default(),
        owner_client_id: None,
    });
    let session = WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "test-pass", reg);
    let approver = Arc::new(RecordingApprover {
        reject,
        prompts: Default::default(),
    });
    let state = Arc::new(
        ServerState::new(Arc::new(Mutex::new(session)), approver.clone())
            .with_node(ChainId::Sepolia, node)
            .with_prover(pstate),
    );
    Fixture { state, approver }
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

/// Pair a client; returns `(token, client_id)`.
async fn pair(state: &ServerState, kind: &str) -> (String, String) {
    let r = call(
        state,
        None,
        "companion_requestPairing",
        json!({ "name": kind, "kind": kind }),
    )
    .await;
    let r = r.result.unwrap();
    (
        r["token"].as_str().unwrap().to_string(),
        r["client_id"].as_str().unwrap().to_string(),
    )
}

async fn grant_forever(state: &ServerState, client_id: &str) {
    let until = wallet_rpc::now_unix_ms() + 90 * 24 * 60 * 60 * 1000;
    assert!(state.clients.lock().await.grant(client_id, until));
}

fn code(r: &Response) -> i64 {
    r.error.as_ref().map(|e| e.code).unwrap_or(0)
}

fn reason(r: &Response) -> Option<String> {
    r.error.as_ref()?.data.as_ref()?["reason"]
        .as_str()
        .map(str::to_string)
}

fn ok(r: Response) -> Value {
    match r.result {
        Some(v) => v,
        None => panic!("error code {}", code(&r)),
    }
}

/// Everything the request log holds for `method`.
async fn logged(state: &ServerState, method: &str) -> String {
    let mut out = String::new();
    for e in state.log.lock().await.recent(100) {
        if e.method == method {
            out.push_str(&format!(
                "{} {:?} {:?}",
                e.outcome, e.params_json, e.result_json
            ));
        }
    }
    out
}

fn transfer(amount: &str) -> Value {
    json!([{ "type": "transfer", "token": STRK, "amount": amount, "recipient": format!("{:#x}", bob()) }])
}

/// The one locator a 0x1e STRK transfer to bob produces: on the user→bob STRK
/// subchannel after the two notes already there.
fn assert_bob_locator(notes: &Value, ck: Felt) {
    let notes = notes.as_array().expect("notes array");
    assert_eq!(
        notes.len(),
        1,
        "one transfer, one locator (change excluded)"
    );
    let n = &notes[0];
    assert_eq!(felt_of(&n["recipient"]), bob());
    assert_eq!(felt_of(&n["token"]), strk());
    assert_eq!(felt_of(&n["amount"]), Felt::from(0x1e_u64));
    assert_eq!(n["index"], json!(2));
    // The pool's `compute_note_id(channel_key, token, index)`.
    assert_eq!(felt_of(&n["note_id"]), hashes::note_id(ck, strk(), 2));
    let salt = u128::try_from(felt_of(&n["salt"])).unwrap();
    assert!(salt > crypto::OPEN_NOTE_SALT && salt < 1 << 120);
    let keys: Vec<&str> = n.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), 6, "locator fields");
}

/// No spelling of the viewing key or any channel key in `text`.
fn assert_no_keys(text: &str, cks: &[Felt]) {
    for (i, k) in std::iter::once(user_sk())
        .chain(cks.iter().copied())
        .enumerate()
    {
        for s in spellings(k) {
            assert!(!text.contains(&s), "key #{i} leaked");
        }
    }
}

// ── locators ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn prepare_invoke_returns_a_locator_per_transfer_and_no_key() {
    let (node, ck) = funded_pool();
    let f = fixture("prepare", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let r = ok(call(
        &f.state,
        Some(&token),
        "wallet_strk20PrepareInvoke",
        json!({ "actions": transfer("0x1e") }),
    )
    .await);
    assert!(
        r.get("call").is_some() && r.get("proof").is_some(),
        "existing fields kept"
    );
    assert_bob_locator(&r["notes"], ck);
    let self_ck = hashes::channel_key(user(), user_sk(), user(), crypto::public_key(user_sk()));
    assert_no_keys(&r.to_string(), &[ck, self_ck]);
    assert_no_keys(
        &logged(&f.state, "wallet_strk20PrepareInvoke").await,
        &[ck, self_ck],
    );
}

/// The plan returns locators without `reveal_actions` and without any key; with
/// it, each locator matches its `create_enc_note` action (index and salt), and
/// the predicted packed value decrypts to the amount under the pool's formula.
#[tokio::test]
async fn plan_returns_locators_that_match_the_planned_actions() {
    let (node, ck) = funded_pool();
    let f = fixture("plan", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let p = ok(call(
        &f.state,
        Some(&token),
        PLAN,
        json!({ "actions": transfer("0x1e") }),
    )
    .await);
    assert_bob_locator(&p["notes"], ck);
    assert!(p.get("client_actions").is_none());
    let self_ck = hashes::channel_key(user(), user_sk(), user(), crypto::public_key(user_sk()));
    assert_no_keys(&p.to_string(), &[ck, self_ck]);
    let d = call(
        &f.state,
        Some(&token),
        "companion_strk20DiscardPlan",
        json!({ "plan_id": p["plan_id"] }),
    )
    .await;
    assert!(d.error.is_none());

    let p = ok(call(
        &f.state,
        Some(&token),
        PLAN,
        json!({ "actions": transfer("0x1e"), "reveal_actions": true }),
    )
    .await);
    assert_eq!(p["actions_revealed"], json!(true));
    let loc = &p["notes"][0];
    let created: Vec<&Value> = p["client_actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["type"] == json!("create_enc_note"))
        .collect();
    // The transfer, then the change back to the user (not a locator).
    assert_eq!(created.len(), 2);
    assert_eq!(felt_of(&created[0]["recipient_addr"]), bob());
    assert_eq!(created[0]["index"], loc["index"]);
    assert_eq!(felt_of(&created[0]["salt"]), felt_of(&loc["salt"]));
    assert_eq!(felt_of(&created[1]["recipient_addr"]), user());
    let salt = u128::try_from(felt_of(&loc["salt"])).unwrap();
    let packed = crypto::encrypt_note_value(0x1e, salt, ck, strk(), 2);
    let value = crypto::decrypt_note_value(packed, ck, strk(), 2).unwrap();
    assert_eq!((value.amount, value.salt, value.open), (0x1e, salt, false));
}

#[tokio::test]
async fn the_prove_job_result_carries_the_plan_locators() {
    let (node, ck) = funded_pool();
    let f = fixture("prove", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let p = ok(call(
        &f.state,
        Some(&token),
        PLAN,
        json!({ "actions": transfer("0x1e") }),
    )
    .await);
    let job = ok(call(
        &f.state,
        Some(&token),
        "companion_strk20Prove",
        json!({ "plan_id": p["plan_id"] }),
    )
    .await);
    let id = job["job_id"].as_str().unwrap().to_string();
    let mut done = Value::Null;
    for _ in 0..500 {
        let s = ok(call(
            &f.state,
            Some(&token),
            "companion_proveStatus",
            json!({ "job_id": id }),
        )
        .await);
        if s["status"] == json!("succeeded") || s["status"] == json!("failed") {
            done = s;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(done["status"], json!("succeeded"));
    assert_eq!(done["result"]["notes"], p["notes"]);
    assert_bob_locator(&done["result"]["notes"], ck);
}

// ── companion_strk20ChannelKey ───────────────────────────────────────────────

/// The happy path: a prompt even under a grant, wording that states the full
/// disclosure, the key and marker exactly as the pool derives them, no
/// outgoing-channel index, and no key in the request log.
#[tokio::test]
async fn channel_key_always_prompts_and_returns_the_pool_derivation() {
    let (node, ck) = funded_pool();
    let f = fixture("ck-ok", node.clone(), &[]);
    let (token, id) = pair(&f.state, "app").await;
    grant_forever(&f.state, &id).await;

    let r = ok(call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": format!("{:#x}", bob()) }),
    )
    .await);
    assert_eq!(felt_of(&r["channel_key"]), ck);
    assert_eq!(felt_of(&r["sender"]), user());
    assert_eq!(felt_of(&r["recipient"]), bob());
    assert_eq!(felt_of(&r["recipient_public_key"]), bob_pk());
    assert_eq!(felt_of(&r["pool"]), pool());
    assert_eq!(r["chainId"], json!("SN_SEPOLIA"));
    // The marker the pool's `channel_exists` checks, and it is set.
    let marker = felt_of(&r["marker"]);
    assert_eq!(marker, hashes::channel_marker(ck, user(), bob(), bob_pk()));
    assert_eq!(
        node.get("channel_exists", vec![marker]),
        Some(vec![Felt::ONE])
    );
    assert_eq!(
        r["subchannels"],
        json!([{ "token": format!("{:#x}", strk()), "note_count": 2 }])
    );
    // With the key, a verifier finds each note and decrypts its amount.
    for (i, want) in [5u128, 7].into_iter().enumerate() {
        let id = hashes::note_id(ck, strk(), i as u32);
        let packed = node.get("get_note", vec![id]).unwrap()[0];
        assert_eq!(
            crypto::decrypt_note_value(packed, ck, strk(), i as u32)
                .unwrap()
                .amount,
            want
        );
    }
    // The disclosure covers the future too: a note sent after the key was
    // handed over decrypts under the same key (scope regression, design §3.2).
    let later = hashes::note_id(ck, strk(), 2);
    // A fresh random salt, as the planner draws it (not a fixed value).
    let later_salt = {
        use strk20::planner::Randomness;
        strk20::planner::OsRandomness.salt120().unwrap().max(2)
    };
    node.set(
        "get_note",
        vec![later],
        vec![crypto::encrypt_note_value(11, later_salt, ck, strk(), 2), strk()],
    );
    let packed = node.get("get_note", vec![later]).unwrap()[0];
    assert_eq!(
        crypto::decrypt_note_value(packed, ck, strk(), 2)
            .unwrap()
            .amount,
        11
    );
    for k in ["index", "channel_index", "outgoing_index"] {
        assert!(r.get(k).is_none(), "returned field {k}");
    }

    // The grant did not cover it: one prompt, worded as a channel disclosure.
    let prompts = f.approver.prompts(METHOD);
    assert_eq!(prompts.len(), 1);
    let bob_hex = address_hex(&bob());
    let user_hex = address_hex(&user());
    let pool_hex = format!("{:#x}", pool());
    let wants = [
        "EVERY private payment",
        "has sent or will ever send",
        "in every token",
        "cannot be revoked",
        "does not let anyone spend",
        "2 note(s) in 1 token(s)",
        "SN_SEPOLIA",
        bob_hex.as_str(),
        user_hex.as_str(),
        pool_hex.as_str(),
    ];
    for (i, want) in wants.iter().enumerate() {
        assert!(prompts[0].contains(want), "prompt lacks expected item #{i}");
    }

    // The request log keeps the call, not the key.
    let log = logged(&f.state, METHOD).await;
    assert!(
        log.contains("<redacted>"),
        "channel_key placeholder missing"
    );
    assert_no_keys(&log, &[ck]);
}

#[tokio::test]
async fn refusing_the_prompt_returns_nothing() {
    let (node, ck) = funded_pool();
    let f = fixture("ck-refused", node, &[METHOD]);
    let (token, _) = pair(&f.state, "app").await;
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": format!("{:#x}", bob()) }),
    )
    .await;
    assert_eq!(code(&r), 113);
    assert!(r.result.is_none());
    assert_eq!(f.approver.prompts(METHOD).len(), 1);
    assert_no_keys(&serde_json::to_string(&r).unwrap(), &[ck]);
    assert_no_keys(&logged(&f.state, METHOD).await, &[ck]);
}

#[tokio::test]
async fn the_self_channel_is_refused_without_a_prompt() {
    let (node, _) = funded_pool();
    let f = fixture("ck-self", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": address_hex(&user()) }),
    )
    .await;
    assert_eq!(
        (code(&r), reason(&r).as_deref()),
        (114, Some("SELF_CHANNEL"))
    );
    assert!(f.approver.prompts(METHOD).is_empty());
}

#[tokio::test]
async fn a_missing_channel_or_recipient_is_refused_without_a_prompt() {
    let (node, _) = funded_pool();
    let f = fixture("ck-none", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": format!("{:#x}", carol()) }),
    )
    .await;
    assert_eq!(
        (code(&r), reason(&r).as_deref()),
        (-32006, Some("CHANNEL_NOT_FOUND"))
    );
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": "0xdead" }),
    )
    .await;
    assert_eq!(code(&r), 118);
    for bad in [
        json!({}),
        json!({ "recipient": "0x0" }),
        json!({ "recipient": "zz" }),
    ] {
        assert_eq!(code(&call(&f.state, Some(&token), METHOD, bad).await), 114);
    }
    assert!(f.approver.prompts(METHOD).is_empty());
}

/// An agent sees only its own accounts and the owners it enrolled.
#[tokio::test]
async fn an_agent_cannot_reach_subjects_outside_its_scope() {
    let (node, _) = funded_pool();
    node.advertise_custom_validation();
    let f = fixture("ck-scope", node, &[]);
    let (app, _) = pair(&f.state, "app").await;
    let (agent, id) = pair(&f.state, "agent").await;
    grant_forever(&f.state, &id).await;
    let r = call(
        &f.state,
        Some(&app),
        "companion_strk20AddOwner",
        json!({ "owner_address": OWNER }),
    )
    .await;
    assert!(r.error.is_none(), "enrol: code {}", code(&r));

    let bob_hex = format!("{:#x}", bob());
    let r = call(
        &f.state,
        Some(&agent),
        METHOD,
        json!({ "recipient": bob_hex, "account_address": address_hex(&user()) }),
    )
    .await;
    assert_eq!(code(&r), -32002);
    let r = call(
        &f.state,
        Some(&agent),
        METHOD,
        json!({ "recipient": bob_hex, "owner_address": OWNER }),
    )
    .await;
    assert_eq!(
        (code(&r), reason(&r).as_deref()),
        (-32002, Some("OWNER_NOT_ENROLLED"))
    );
    // No account of its own: nothing to disclose.
    let r = call(
        &f.state,
        Some(&agent),
        METHOD,
        json!({ "recipient": bob_hex }),
    )
    .await;
    assert_eq!(code(&r), -32002);
    assert!(f.approver.prompts(METHOD).is_empty());
}

/// An enrolled owner contract's channel, keyed by the viewing key strkd holds
/// for it.
#[tokio::test]
async fn an_owner_subject_discloses_the_owners_channel() {
    let (node, _) = funded_pool();
    let osk = owner_sk();
    node.register(owner(), crypto::public_key(osk));
    node.advertise_custom_validation();
    let ck = node.open_channel(owner(), osk, bob(), bob_pk(), &[(strk(), &[9])]);
    let f = fixture("ck-owner", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let r = call(
        &f.state,
        Some(&token),
        "companion_strk20AddOwner",
        json!({ "owner_address": OWNER }),
    )
    .await;
    assert!(r.error.is_none(), "enrol: code {}", code(&r));

    let r = ok(call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": format!("{:#x}", bob()), "owner_address": OWNER }),
    )
    .await);
    assert_eq!(felt_of(&r["channel_key"]), ck);
    assert_eq!(felt_of(&r["sender"]), owner());
    assert_eq!(
        felt_of(&r["marker"]),
        hashes::channel_marker(ck, owner(), bob(), bob_pk())
    );
    let prompts = f.approver.prompts(METHOD);
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("owner contract"),
        "prompt does not name the owner subject"
    );
    // The owner's own channel to itself is refused like an account's.
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": OWNER, "owner_address": OWNER }),
    )
    .await;
    assert_eq!(reason(&r).as_deref(), Some("SELF_CHANNEL"));
}

#[tokio::test]
async fn pool_address_only_confirms_the_configured_pool() {
    let (node, _) = funded_pool();
    let f = fixture("ck-pool", node, &[]);
    let (token, _) = pair(&f.state, "app").await;
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": format!("{:#x}", bob()), "pool_address": "0x123" }),
    )
    .await;
    assert_eq!(code(&r), 114);
    assert!(f.approver.prompts(METHOD).is_empty());
    let r = call(
        &f.state,
        Some(&token),
        METHOD,
        json!({ "recipient": format!("{:#x}", bob()), "pool_address": SEPOLIA_POOL }),
    )
    .await;
    assert!(r.error.is_none(), "code {}", code(&r));
}
