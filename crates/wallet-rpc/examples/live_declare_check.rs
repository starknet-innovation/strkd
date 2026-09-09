//! Live wire-format check for DECLARE v3 against a real node (strkd #9).
//!
//! Not a unit test: it needs the network. **Read-only** — it never broadcasts.
//! Run with `cargo run -p wallet-rpc --example live_declare_check`
//! (override the endpoint with `STRKD_RPC=…`).
//!
//! It establishes three things against the node itself, with no key material:
//!   1. our derived class hash equals the node's, for a class the node already
//!      holds (`starknet_getClass` → derive → compare);
//!   2. the node accepts our `BROADCASTED_DECLARE_TXN_V3` object and returns a
//!      real fee estimate for an **undeclared** class (the wire format is right);
//!   3. with validation *enabled* and a deliberately bogus signature, the node
//!      fails on the signature specifically — so the transaction body is
//!      otherwise valid, and a correct signature over the hash we now sign is
//!      the only remaining gate. That is precisely the distinction the
//!      "invalid signature" report could not make.
//!
//! Confirming the accepted case end-to-end needs a funded, deployed account
//! whose seed the wallet holds (the standing "live submit" item in
//! `docs/project/status.md`); it is deliberately not done here.
use serde_json::{json, Value};
use wallet_core::{Felt, SierraClass};

const DEFAULT_RPC: &str = "https://sepolia.nodes.starknet.org/rpc/v0_10";

/// A class already declared on Sepolia (block 14790955, tx 0x65d0f5b6…) — used
/// for (1) because the node can hand us its exact class object.
const DECLARED_CLASS: &str = "0x69d70a4855b77d155491700188d0cc86214dc1791eba05bbab1e17426fa22b8";
/// The sender of that declare: a deployed Sepolia account, so validation in (3)
/// reaches the signature check rather than failing on a missing contract.
const SENDER: &str = "0x482e1f64c050e49fe6e61a9444e9a8ad62ab73ccc60caa6fd1f8f3af3106c4b";

/// A trivial `Counter` compiled with scarb 2.18.0 — not declared anywhere, so
/// (2) and (3) get past the "already declared" check into real execution.
const COUNTER_CLASS: &str =
    include_str!("../../wallet-core/tests/fixtures/minimal_counter.contract_class.json");
/// Its CASM hash (starknet.js `computeCompiledClassHash`).
const COUNTER_CASM: &str = "0x76a63be852874049f634ae5631ce89488602e919f3656dda9684e614dd9285d";

async fn rpc(url: &str, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

fn brief(v: &Value, n: usize) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.chars().count() > n {
        s.chars().take(n).collect::<String>() + "…"
    } else {
        s
    }
}

#[tokio::main]
async fn main() {
    let url = std::env::var("STRKD_RPC").unwrap_or_else(|_| DEFAULT_RPC.to_string());
    println!("node: {url}\n");

    // (1) Derive the class hash from the node's own class object.
    let got = rpc(&url, "starknet_getClass", json!(["latest", DECLARED_CLASS])).await;
    let live = SierraClass::from_json(&got["result"]).expect("parse live class");
    let derived = format!("0x{:x}", live.class_hash());
    let node_says = format!("0x{:x}", Felt::from_hex(DECLARED_CLASS).unwrap());
    println!("[1] class hash\n    derived {derived}\n    node    {node_says}");
    assert_eq!(
        derived, node_says,
        "derived class hash must match the node's"
    );
    println!("    → match\n");

    // The undeclared class, as the canonical RPC CONTRACT_CLASS object.
    let counter = SierraClass::from_json(&serde_json::from_str(COUNTER_CLASS).unwrap())
        .expect("parse counter class");
    println!(
        "[·] undeclared test class hashes to 0x{:x}\n",
        counter.class_hash()
    );

    let nonce = rpc(&url, "starknet_getNonce", json!(["latest", SENDER])).await;
    let nonce = nonce["result"].as_str().unwrap_or("0x0").to_string();
    let bounds =
        |amount: &str, price: &str| json!({"max_amount": amount, "max_price_per_unit": price});
    let tx = |signature: Value, zero_bounds: bool| {
        let rb = if zero_bounds {
            json!({"l1_gas": bounds("0x0", "0x0"), "l2_gas": bounds("0x0", "0x0"), "l1_data_gas": bounds("0x0", "0x0")})
        } else {
            json!({
                "l1_gas": bounds("0x0", "0xe316fc8256b1"),
                "l2_gas": bounds("0x1329dd40", "0x9ca6e0222"),
                "l1_data_gas": bounds("0x120", "0xeaaacb6cb9"),
            })
        };
        json!({
            "type": "DECLARE", "version": "0x3",
            "sender_address": SENDER,
            "compiled_class_hash": COUNTER_CASM,
            "contract_class": counter.to_rpc_json(),
            "signature": signature,
            "nonce": nonce,
            "resource_bounds": rb,
            "tip": "0x0", "paymaster_data": [], "account_deployment_data": [],
            "nonce_data_availability_mode": "L1", "fee_data_availability_mode": "L1",
        })
    };

    // (2) Wire format: SKIP_VALIDATE, so no signature is needed.
    let est = rpc(
        &url,
        "starknet_estimateFee",
        json!({"request": [tx(json!([]), true)], "simulation_flags": ["SKIP_VALIDATE"], "block_id": "latest"}),
    )
    .await;
    println!("[2] estimateFee (SKIP_VALIDATE) → {}", brief(&est, 400));
    assert!(
        est.get("result").is_some(),
        "node must accept the declare tx object"
    );
    println!("    → accepted, real estimate returned\n");

    // (3) Validation enabled, signature deliberately wrong.
    let val = rpc(
        &url,
        "starknet_estimateFee",
        json!({"request": [tx(json!(["0x1", "0x2"]), false)], "simulation_flags": [], "block_id": "latest"}),
    )
    .await;
    println!(
        "[3] estimateFee (validate, bogus signature) → {}",
        brief(&val, 400)
    );
    // "Account: invalid signature", raised from __validate_declare__
    // (0x289da278…) — the reported symptom, on a body (2) just proved valid.
    let detail = serde_json::to_string(&val["error"]).unwrap_or_default();
    assert!(
        detail.contains("4163636f756e743a20696e76616c6964207369676e6174757265")
            || detail.contains("invalid signature"),
        "expected the signature check to be the failing step, got: {detail}"
    );
    println!("    → fails at __validate_declare__ on the signature alone");
}
