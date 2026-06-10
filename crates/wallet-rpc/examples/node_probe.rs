//! Live check of the `HttpStarknetRpc` client against a real Sepolia v0.10 node.
//! Read-only (no signing/broadcast, no key material): nonce, deploy-status, and
//! a DEPLOY_ACCOUNT fee estimate for the public test-vector account.
//!
//! Run: `cargo run -p wallet-rpc --example node_probe`
//! Uses the `http://` URL on purpose to exercise http→https redirect handling.

use wallet_core::{deployment_data, ChainId, Domain, Felt};
use wallet_rpc::{HttpStarknetRpc, StarknetRpc};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[tokio::main]
async fn main() {
    let node = HttpStarknetRpc::new("http://sepolia.nodes.starknet.org/rpc/v0_10");
    let strk =
        Felt::from_hex("0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d")
            .unwrap();

    println!("is_deployed(STRK token) = {:?}", node.is_deployed(&strk).await);
    println!("get_nonce(STRK token)   = {:?}", node.get_nonce(&strk).await);

    let d = deployment_data(TEST_MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap();
    println!("is_deployed(test acct)  = {:?}", node.is_deployed(&d.address).await);
    let est = node
        .estimate_deploy_account(&d.address, &d.class_hash, &d.constructor_calldata, &d.salt)
        .await;
    println!("estimate_deploy_account = {:?}", est);
}
