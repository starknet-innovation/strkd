//! Prints the OZ deployment data (public) for the all-zeros BIP-39 test vector
//! on Sepolia — used to craft a real `starknet_estimateFee` request when
//! verifying the node wire format. No real key material.

use wallet_core::{address_hex, deployment_data, AccountContract, ChainId, Domain};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

fn main() {
    let d = deployment_data(
        TEST_MNEMONIC,
        Domain::User,
        0,
        None,
        ChainId::Sepolia,
        AccountContract::OpenZeppelin,
    )
    .unwrap();
    let hexes: Vec<String> = d
        .constructor_calldata
        .iter()
        .map(|f| format!("0x{:x}", f))
        .collect();
    println!("address={}", address_hex(&d.address));
    println!("class_hash=0x{:x}", d.class_hash);
    println!("salt=0x{:x}", d.salt);
    println!("constructor_calldata={}", serde_json::to_string(&hexes).unwrap());
}
