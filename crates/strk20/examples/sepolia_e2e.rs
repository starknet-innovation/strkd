//! Live end-to-end check of the `strk20` crate on Sepolia, with no SDK involved:
//! discover → plan → sign → prove (local prover) → submit `apply_actions`.
//!
//! ```text
//! cargo run -p strk20 --example sepolia_e2e -- balances
//! cargo run -p strk20 --example sepolia_e2e -- transfer <from> <to> <amount_wei>
//! cargo run -p strk20 --example sepolia_e2e -- withdraw <from> <amount_wei>
//! ```
//!
//! Accounts are test-seed user accounts `<from>`/`<to>` (the published BIP-39
//! all-`abandon` vector — never real keys), already deployed and registered on
//! the pool. Needs a local `starknet_transaction_prover` serving
//! `starknet_proveTransaction` (`PROVER_URL`, default `http://127.0.0.1:9910`).
//! Deposits are out of scope here: they need a screening attestation.

use std::collections::BTreeMap;

use async_trait::async_trait;
use krusty_kms_common::ChainId;
use serde_json::{json, Value};
use starknet_types_core::felt::Felt;
use strk20::discovery::{self, PoolReader};
use strk20::invocation::ProofInvocation;
use strk20::planner::{self, Intent, OsRandomness, State};
use strk20::{apply, Error};
use wallet_core::{
    account_address, sign_hash, sign_invoke_v3, strk20_viewing_key, AccountContract, Domain,
    InvokeV3Params, ResourceBounds,
};

const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const RPC: &str = "https://sepolia.nodes.starknet.org/rpc/v0_10";
const POOL: &str = "0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323";
const STRK: &str = "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";
/// Proofs and the discovery that plans them read state this many blocks back.
const PROVING_LAG: u64 = 12;

fn f(hex: &str) -> Felt {
    Felt::from_hex(hex).unwrap()
}
fn hex(x: &Felt) -> String {
    format!("{x:#x}")
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let r: Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    match r.get("error") {
        Some(e) => Err(format!("{method}: {e}")),
        None => Ok(r["result"].clone()),
    }
}

struct RpcPool {
    client: reqwest::Client,
    pool: Felt,
    block: Value,
}

#[async_trait]
impl PoolReader for RpcPool {
    async fn call(&self, selector: Felt, calldata: Vec<Felt>) -> Result<Vec<Felt>, Error> {
        let req = json!({ "contract_address": hex(&self.pool), "entry_point_selector": hex(&selector),
                          "calldata": calldata.iter().map(hex).collect::<Vec<_>>() });
        let out = rpc(&self.client, RPC, "starknet_call", json!([req, self.block]))
            .await
            .map_err(Error::Read)?;
        out.as_array()
            .ok_or_else(|| Error::Read("call result is not an array".into()))?
            .iter()
            .map(|v| {
                Felt::from_hex(v.as_str().unwrap_or_default())
                    .map_err(|e| Error::Read(e.to_string()))
            })
            .collect()
    }
}

struct User {
    index: u32,
    address: Felt,
    sk: Felt,
}

fn user(index: u32, pool: Felt) -> User {
    let address = account_address(
        MNEMONIC,
        Domain::User,
        index,
        None,
        ChainId::Sepolia,
        AccountContract::OpenZeppelin,
    )
    .unwrap();
    let sk = strk20_viewing_key(
        MNEMONIC,
        Domain::User,
        index,
        None,
        &ChainId::Sepolia.as_felt(),
        &pool,
    )
    .unwrap();
    User { index, address, sk }
}

async fn balances(client: &reqwest::Client, pool: Felt) {
    let reader = RpcPool {
        client: client.clone(),
        pool,
        block: json!("latest"),
    };
    for i in 0..2 {
        let u = user(i, pool);
        let notes = discovery::discover_notes(&reader, u.address, u.sk, &[f(STRK)])
            .await
            .unwrap();
        let total: u128 = notes.iter().map(|n| n.amount).sum();
        println!(
            "user {i}: {} note(s), {} STRK shielded",
            notes.len(),
            total as f64 / 1e18
        );
    }
}

async fn run(
    client: &reqwest::Client,
    pool: Felt,
    from: u32,
    intent_for: impl FnOnce(&User) -> Intent,
    recipients: Vec<Felt>,
) {
    let u = user(from, pool);
    let head = rpc(client, RPC, "starknet_blockNumber", json!([]))
        .await
        .unwrap()
        .as_u64()
        .unwrap();
    let block = head - PROVING_LAG;
    let reader = RpcPool {
        client: client.clone(),
        pool,
        block: json!({ "block_number": block }),
    };

    // Discover what the plan needs: notes, the channels it sends on, the channel count.
    let mut channels = BTreeMap::new();
    for r in recipients.iter().chain([&u.address]) {
        channels.insert(
            *r,
            discovery::discover_channel(&reader, u.address, u.sk, *r)
                .await
                .unwrap(),
        );
    }
    let state = State {
        user: u.address,
        user_sk: u.sk,
        registered: discovery::public_key(&reader, u.address)
            .await
            .unwrap()
            .is_some(),
        outgoing_channels: discovery::outgoing_channel_count(&reader, u.address, u.sk)
            .await
            .unwrap(),
        channels,
        notes: discovery::discover_notes(&reader, u.address, u.sk, &[f(STRK)])
            .await
            .unwrap(),
    };
    let plan = planner::plan(&intent_for(&u), &state, &mut OsRandomness).unwrap();
    println!("plan at block {block}: {} action(s)", plan.actions.len());
    for a in &plan.actions {
        println!("  {a:?}");
    }

    // Sign the virtual invoke with the user's account key; prove it locally.
    let pool_nonce = Felt::from_hex(
        rpc(
            client,
            RPC,
            "starknet_getNonce",
            json!([{ "block_number": block }, hex(&pool)]),
        )
        .await
        .unwrap()
        .as_str()
        .unwrap(),
    )
    .unwrap();
    let inv = ProofInvocation::new(
        pool,
        ChainId::Sepolia,
        pool_nonce,
        u.address,
        u.sk,
        &plan.actions,
    );
    let sig = sign_hash(MNEMONIC, Domain::User, u.index, None, &inv.hash()).unwrap();
    let tx = inv.to_rpc_json(&AccountContract::OpenZeppelin.serialize_signature(&sig));
    let prover = std::env::var("PROVER_URL").unwrap_or_else(|_| "http://127.0.0.1:9910".into());
    let started = std::time::Instant::now();
    let proved = rpc(
        client,
        &prover,
        "starknet_proveTransaction",
        json!([{ "block_number": block }, tx]),
    )
    .await
    .unwrap();
    let proof = proved["proof"].as_str().unwrap().to_string();
    let facts: Vec<Felt> = proved["proof_facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| f(v.as_str().unwrap()))
        .collect();
    let payload: Vec<Felt> = proved["l2_to_l1_messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| f(m["from_address"].as_str().unwrap()) == pool)
        .expect("a message from the pool")["payload"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| f(v.as_str().unwrap()))
        .collect();
    println!(
        "proved in {:.1}s, proof {} B base64",
        started.elapsed().as_secs_f64(),
        proof.len()
    );

    // Submit apply_actions from the user's own account (no screening: no deposit).
    let call = apply::apply_actions_call(pool, &payload, None).unwrap();
    let nonce = f(rpc(
        client,
        RPC,
        "starknet_getNonce",
        json!(["latest", hex(&u.address)]),
    )
    .await
    .unwrap()
    .as_str()
    .unwrap());
    let calldata = wallet_core::encode_calls(std::slice::from_ref(&call));
    let tx_json = |signature: &[Felt], bounds: &[ResourceBounds; 3]| {
        let b = |r: &ResourceBounds| json!({ "max_amount": format!("{:#x}", r.max_amount), "max_price_per_unit": format!("{:#x}", r.max_price_per_unit) });
        json!({
            "type": "INVOKE", "version": "0x3", "sender_address": hex(&u.address),
            "calldata": calldata.iter().map(hex).collect::<Vec<_>>(),
            "signature": signature.iter().map(hex).collect::<Vec<_>>(),
            "nonce": hex(&nonce),
            "resource_bounds": { "l1_gas": b(&bounds[0]), "l2_gas": b(&bounds[1]), "l1_data_gas": b(&bounds[2]) },
            "tip": "0x0", "paymaster_data": [], "account_deployment_data": [],
            "nonce_data_availability_mode": "L1", "fee_data_availability_mode": "L1",
            "proof_facts": facts.iter().map(hex).collect::<Vec<_>>(), "proof": proof,
        })
    };
    // Estimate with the proof attached (verification gas dominates), then add 50%.
    let zero = ResourceBounds {
        max_amount: 0,
        max_price_per_unit: 0,
    };
    let est = rpc(
        client,
        RPC,
        "starknet_estimateFee",
        json!([
            [tx_json(&[], &[zero, zero, zero])],
            ["SKIP_VALIDATE"],
            "latest"
        ]),
    )
    .await
    .unwrap();
    let est = &est[0];
    let num = |k: &str| -> u128 {
        u128::from_str_radix(est[k].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    };
    let margin = |x: u128| x * 3 / 2;
    let bounds = [
        ResourceBounds {
            max_amount: margin(num("l1_gas_consumed")) as u64,
            max_price_per_unit: margin(num("l1_gas_price")),
        },
        ResourceBounds {
            max_amount: margin(num("l2_gas_consumed")) as u64,
            max_price_per_unit: margin(num("l2_gas_price")),
        },
        ResourceBounds {
            max_amount: margin(num("l1_data_gas_consumed")) as u64,
            max_price_per_unit: margin(num("l1_data_gas_price")),
        },
    ];
    println!(
        "estimated {} L2 gas, fee {:.3} STRK",
        num("l2_gas_consumed"),
        num("overall_fee") as f64 / 1e18
    );
    let params = InvokeV3Params {
        nonce,
        l1_gas: bounds[0],
        l2_gas: bounds[1],
        l1_data_gas: bounds[2],
        proof_facts: facts.clone(),
        ..InvokeV3Params::default()
    };
    let signed = sign_invoke_v3(
        MNEMONIC,
        Domain::User,
        u.index,
        None,
        &u.address,
        &[call],
        ChainId::Sepolia,
        &params,
        AccountContract::OpenZeppelin,
    )
    .unwrap();
    let sent = rpc(
        client,
        RPC,
        "starknet_addInvokeTransaction",
        json!([tx_json(&signed.signature, &bounds)]),
    )
    .await
    .unwrap();
    let hash = sent["transaction_hash"].as_str().unwrap().to_string();
    println!("submitted {hash}");
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if let Ok(r) = rpc(client, RPC, "starknet_getTransactionReceipt", json!([hash])).await {
            println!(
                "execution {} {}",
                r["execution_status"],
                r.get("revert_reason")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            );
            return;
        }
    }
    println!("no receipt after 5 minutes");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let client = reqwest::Client::new();
    let pool = f(POOL);
    let token = f(STRK);
    match args.first().map(String::as_str) {
        Some("balances") => balances(&client, pool).await,
        Some("transfer") => {
            let (from, to, amount): (u32, u32, u128) = (args[1].parse().unwrap(), args[2].parse().unwrap(), args[3].parse().unwrap());
            let to_addr = user(to, pool).address;
            run(&client, pool, from, |_| Intent { transfers: vec![(to_addr, token, amount)], ..Intent::default() }, vec![to_addr]).await
        }
        Some("withdraw") => {
            let (from, amount): (u32, u128) = (args[1].parse().unwrap(), args[2].parse().unwrap());
            run(&client, pool, from, |u| Intent { withdrawals: vec![(u.address, token, amount)], ..Intent::default() }, vec![]).await
        }
        _ => eprintln!("usage: sepolia_e2e balances | transfer <from> <to> <amount_wei> | withdraw <from> <amount_wei>"),
    }
}
