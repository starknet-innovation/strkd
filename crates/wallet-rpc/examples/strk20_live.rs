//! Live check of strkd's STRK20 JSON-RPC methods on Sepolia, through the real
//! `dispatch` path (pairing, approval, session, prover, node).
//!
//! ```text
//! cargo run -p wallet-rpc --example strk20_live -- balances <account>
//! cargo run -p wallet-rpc --example strk20_live -- register <account>
//! cargo run -p wallet-rpc --example strk20_live -- prepare-transfer <from> <to> <amount_wei>
//! cargo run -p wallet-rpc --example strk20_live -- invoke <from> <json-actions>
//! cargo run -p wallet-rpc --example strk20_live -- prepare-submit <from> <to> <amount_wei> <submitter>
//! ```
//!
//! `<account>`, `<from>`, `<to>` are test-seed user account indices (the
//! published BIP-39 all-`abandon` vector; never real keys) already deployed and
//! registered on the pool. The pool defaults to strkd's Sepolia test pool
//! (`STRK20_POOL` overrides). Proving uses the bundled native prover:
//! `STRKD_SNIP36_BIN` / `STRKD_SNIP36_WORK_DIR` must point at it (the desktop
//! app's `resources/prover`). Approvals are automatic.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;
use wallet_core::{address_hex, oz_address, AccountRef, ChainId, Domain, Registry};
use wallet_rpc::{dispatch, AutoApprover, Decision, HttpStarknetRpc, Request, ServerState, WalletSession};

const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const RPC: &str = "https://sepolia.nodes.starknet.org/rpc/v0_10";
const TEST_POOL: &str = "0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323";
const STRK: &str = "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";

fn address(i: u32) -> String {
    address_hex(&oz_address(MNEMONIC, Domain::User, i, None, ChainId::Sepolia).unwrap())
}

async fn rpc(state: &ServerState, token: Option<&str>, method: &str, params: Value) -> Value {
    let req: Request = serde_json::from_value(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).unwrap();
    let started = std::time::Instant::now();
    let resp = dispatch(state, token, req).await;
    let v = serde_json::to_value(&resp).unwrap();
    eprintln!("{method} ({:.1}s)", started.elapsed().as_secs_f64());
    v
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut reg = Registry::default();
    for i in 0..3 {
        reg.add(AccountRef {
            domain: Domain::User,
            index: i,
            address: address(i),
            label: format!("test {i}"),
            contract: Default::default(),
            owner_client_id: None,
        });
    }
    let session = WalletSession::new_unlocked(ChainId::Sepolia, MNEMONIC, "live-check", reg);
    let data_dir = std::env::temp_dir().join(format!("strkd-strk20-live-{}", std::process::id()));
    let prover = prover::build_prover_state(data_dir.clone(), &prover::ProverConfig { prover_backend: "native".into() });
    let mut settings = prover.settings.get().await;
    settings.testnet.rpc_url = RPC.into();
    settings.testnet.strk20.pool = std::env::var("STRK20_POOL").unwrap_or_else(|_| TEST_POOL.into());
    settings.testnet.strk20.avnu_api_key = std::env::var("AVNU_API_KEY").unwrap_or_default();
    settings.testnet.strk20.deposit_prover_url = std::env::var("DEPOSIT_PROVER_URL").unwrap_or_default();
    prover.settings.update(settings).await.unwrap();

    let mut state = ServerState::new(Arc::new(Mutex::new(session)), Arc::new(AutoApprover(Decision::Approve)))
        .with_node(ChainId::Sepolia, Arc::new(HttpStarknetRpc::new(RPC)));
    state.prover = Some(Arc::new(prover));

    let paired = rpc(&state, None, "companion_requestPairing", json!({ "name": "strk20-live", "kind": "app" })).await;
    let token = paired["result"]["token"].as_str().expect("paired").to_string();
    let t = Some(token.as_str());

    let out = match args.first().map(String::as_str) {
        Some("balances") => {
            let i: u32 = args[1].parse().unwrap();
            rpc(&state, t, "wallet_strk20Balances", json!({ "tokens": [STRK], "account_address": address(i) })).await
        }
        Some("prepare-transfer") => {
            let (from, to, amount): (u32, u32, u128) = (args[1].parse().unwrap(), args[2].parse().unwrap(), args[3].parse().unwrap());
            let mut v = rpc(
                &state,
                t,
                "wallet_strk20PrepareInvoke",
                json!({ "account_address": address(from),
                        "actions": [{ "type": "transfer", "token": STRK, "amount": format!("{amount:#x}"), "recipient": address(to) }] }),
            )
            .await;
            if let Some(d) = v.pointer_mut("/result/proof/data") {
                let n = d.as_str().map(str::len).unwrap_or(0);
                *d = json!(format!("<{n} base64 chars>"));
            }
            v
        }
        Some("register") => {
            let i: u32 = args[1].parse().unwrap();
            rpc(&state, t, "companion_strk20Register", json!({ "account_address": address(i) })).await
        }
        Some("prepare-submit") => {
            // The no-paymaster route: prepare as <from>, submit from <submitter>.
            let (from, to, amount, by): (u32, u32, u128, u32) =
                (args[1].parse().unwrap(), args[2].parse().unwrap(), args[3].parse().unwrap(), args[4].parse().unwrap());
            let prepared = rpc(
                &state,
                t,
                "wallet_strk20PrepareInvoke",
                json!({ "account_address": address(from),
                        "actions": [{ "type": "transfer", "token": STRK, "amount": format!("{amount:#x}"), "recipient": address(to) }] }),
            )
            .await;
            let r = &prepared["result"];
            if r.is_null() {
                prepared
            } else {
                rpc(
                    &state,
                    t,
                    "wallet_addInvokeTransaction",
                    json!({ "account_address": address(by), "submit": true,
                            "calls": [{ "contract_address": r["call"]["contract_address"], "entry_point": r["call"]["entry_point"], "calldata": r["call"]["calldata"] }],
                            "proof_facts": r["proof"]["proof_facts"], "proof": r["proof"]["data"] }),
                )
                .await
            }
        }
        Some("invoke") => {
            let from: u32 = args[1].parse().unwrap();
            let actions: Value = serde_json::from_str(&args[2]).unwrap();
            rpc(&state, t, "wallet_strk20InvokeTransaction", json!({ "account_address": address(from), "actions": actions })).await
        }
        _ => {
            eprintln!("usage: strk20_live balances <i> | prepare-transfer <from> <to> <wei> | invoke <from> <json-actions>");
            return;
        }
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    let _ = std::fs::remove_dir_all(data_dir);
}
