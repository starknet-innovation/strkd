//! End-to-end HTTP smoke test: a real loopback server, driven over the wire
//! with reqwest, exercising the pairing → request flow and the `port.lock`
//! discovery file.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use wallet_core::ChainId;
use wallet_rpc::{bind_loopback, AutoApprover, Decision, ServerState, WalletSession};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// A well-formed native request: carries the required `X-Companion-Client`
/// header that transport hardening (spec §5.4) demands.
async fn rpc(client: &reqwest::Client, url: &str, token: Option<&str>, body: Value) -> Value {
    let mut req = client
        .post(url)
        .header("X-Companion-Client", "smoke-client")
        .json(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.unwrap().json().await.unwrap()
}

#[tokio::test]
async fn loopback_server_serves_requests_and_writes_port_lock() {
    let session = WalletSession::new_unlocked(
        ChainId::Sepolia,
        TEST_MNEMONIC,
        "test-pass",
        wallet_core::Registry::default(),
    );
    let state = Arc::new(ServerState::new(
        Arc::new(Mutex::new(session)),
        Arc::new(AutoApprover(Decision::Approve)),
    ));

    let dir = std::env::temp_dir().join(format!("strkd-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let port_lock = dir.join("port.lock");

    let (addr, _handle) = bind_loopback(state, Some(&port_lock)).await.unwrap();

    // The server binds loopback only.
    assert!(addr.ip().is_loopback());

    // The discovery file exists and reports the bound port.
    let lock: Value = serde_json::from_slice(&std::fs::read(&port_lock).unwrap()).unwrap();
    assert_eq!(lock["port"].as_u64().unwrap() as u16, addr.port());
    assert!(lock["nonce"].as_str().unwrap().len() >= 16);

    let url = format!("http://{addr}/");
    let client = reqwest::Client::new();

    // Public method works without a token.
    let status = rpc(&client, &url, None, json!({
        "jsonrpc": "2.0", "id": 1, "method": "companion_getStatus", "params": {}
    }))
    .await;
    assert_eq!(status["result"]["network"], json!("SN_SEPOLIA"));

    // Unauthenticated protected call is rejected over the wire.
    let unauth = rpc(&client, &url, None, json!({
        "jsonrpc": "2.0", "id": 2, "method": "wallet_requestChainId", "params": {}
    }))
    .await;
    assert_eq!(unauth["error"]["code"], json!(118));

    // Pair, then make an authenticated call with the bearer token.
    let paired = rpc(&client, &url, None, json!({
        "jsonrpc": "2.0", "id": 3, "method": "companion_requestPairing",
        "params": {"name": "smoke", "kind": "app"}
    }))
    .await;
    let token = paired["result"]["token"].as_str().unwrap().to_string();

    let chain = rpc(&client, &url, Some(&token), json!({
        "jsonrpc": "2.0", "id": 4, "method": "wallet_requestChainId", "params": {}
    }))
    .await;
    assert!(chain["result"].as_str().unwrap().starts_with("0x"));

    let _ = std::fs::remove_dir_all(&dir);
}

async fn spawn_server() -> std::net::SocketAddr {
    let session = WalletSession::new_unlocked(
        ChainId::Sepolia,
        TEST_MNEMONIC,
        "test-pass",
        wallet_core::Registry::default(),
    );
    let state = Arc::new(ServerState::new(
        Arc::new(Mutex::new(session)),
        Arc::new(AutoApprover(Decision::Approve)),
    ));
    let (addr, _handle) = bind_loopback(state, None).await.unwrap();
    addr
}

#[tokio::test]
async fn transport_guard_rejects_request_without_custom_header() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/");
    let client = reqwest::Client::new();
    // No X-Companion-Client header → 403.
    let resp = client
        .post(&url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"companion_getStatus","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], json!(-32003));
}

#[tokio::test]
async fn usage_endpoint_is_open_and_self_describing() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/");
    let client = reqwest::Client::new();

    // GET / needs no auth and no X-Companion-Client header (open discovery).
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let doc: Value = resp.json().await.unwrap();

    assert!(doc["service"].as_str().unwrap().contains("strkd"));
    assert!(doc["methods"].is_array());
    assert!(doc["quickstart"].is_array());
    // Core sections the agent contract must carry.
    for key in [
        "transport", "concepts", "approval_model", "submit_model", "errors", "notes",
        "alpha_notice",
    ] {
        assert!(!doc[key].is_null(), "usage doc missing '{key}'");
    }
    // The alpha disclaimer carries the "don't work around it, report it" CTA and a
    // copy-pasteable feedback template the operator relays to the maintainer.
    let report = doc["alpha_notice"]["report_format"].as_str().unwrap();
    assert!(report.contains("=== STRKD-FEEDBACK ==="), "feedback template missing its sentinel");
    assert!(doc["alpha_notice"]["do_not_work_around"].as_str().unwrap().contains("workaround"));
    // The full method catalogue is present (every implemented method is listed,
    // so the doc can't silently drift from the dispatcher).
    let methods: Vec<String> = doc["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["method"].as_str().unwrap().to_string())
        .collect();
    for expected in [
        "companion_requestPairing",
        "companion_createAgentAccount",
        "companion_requestFunding",
        "companion_deployAccount",
        "companion_estimateFee",
        "companion_reportIssue",
        "companion_requestGrant",
        "wallet_signTypedData",
        "wallet_addInvokeTransaction",
        "wallet_addDeclareTransaction",
        "wallet_switchStarknetChain",
        "wallet_watchAsset",
        "companion_getStatus",
        "wallet_strk20Balances",
        "wallet_strk20PrepareInvoke",
        "wallet_strk20InvokeTransaction",
    ] {
        assert!(methods.iter().any(|m| m == expected), "usage doc missing method {expected}");
    }
    // Permission/grant model is documented (the key agent-facing behaviour).
    assert!(doc["concepts"]["permissions"].as_str().unwrap().contains("grant"));

    // /usage is an alias.
    let resp2 = client.get(format!("http://{addr}/usage")).send().await.unwrap();
    assert_eq!(resp2.status().as_u16(), 200);
}

#[tokio::test]
async fn transport_guard_rejects_request_with_origin() {
    let addr = spawn_server().await;
    let url = format!("http://{addr}/");
    let client = reqwest::Client::new();
    // Browser-shaped request: Origin present (even with the custom header) → 403.
    let resp = client
        .post(&url)
        .header("X-Companion-Client", "x")
        .header("Origin", "http://evil.example")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"companion_getStatus","params":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
}
