//! Regenerate `tests/fixtures/conformance-vectors.json`.
//!
//! ```text
//! cargo run --example conformance_vectors > crates/wallet-core/tests/fixtures/conformance-vectors.json
//! ```
//!
//! Public data only: derivation paths, public keys, and addresses. No private
//! key or seed-derived secret is printed, and the mnemonic is the published
//! BIP-39 all-`abandon` test vector.
//!
//! **Regenerating is not verifying.** Any change here must be re-checked against
//! an independent implementation before it is committed — see the header of the
//! generated file.

use wallet_core::{
    account_address, address_hex, public_key, AccountContract, ChainId, Domain,
};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

fn main() {
    let chain = ChainId::Sepolia;
    let contract = AccountContract::OpenZeppelin;
    let mut entries = Vec::new();

    for (domain, name) in [(Domain::User, "user"), (Domain::Agent, "agent")] {
        for n in 0..4u32 {
            let pk = public_key(TEST_MNEMONIC, domain, n, None).unwrap();
            let addr = account_address(TEST_MNEMONIC, domain, n, None, chain, contract).unwrap();
            entries.push(serde_json::json!({
                "domain": name,
                "n": n,
                "path": domain.path(n),
                "public_key": address_hex(&pk),
                "address": address_hex(&addr),
            }));
        }
    }

    let doc = serde_json::json!({
        "_provenance": {
            "purpose": "Cross-wallet conformance: seed -> public key -> account address. \
strkd asserts against this in tests/conformance.rs; bramble can adopt the same file.",
            "generated_by": "cargo run --example conformance_vectors (wallet-core)",
            "verified_against": "starknet.js hash.calculateContractAddressFromHash(salt, class_hash, [public_key], 0) \
— exactly the call bramble makes. All vectors agreed when generated.",
            "warning": "Regenerating is not verifying. If a value here changes, re-check it against an \
independent implementation before committing, or this file just asserts strkd against itself.",
            "seed": "Published BIP-39 test vector. Never a real seed — see spec §14.",
            "issue": "https://github.com/starknet-innovation/strkd/issues/17"
        },
        "mnemonic": TEST_MNEMONIC,
        "coin_type": wallet_core::STARKNET_COIN_TYPE,
        "account_contract": "openzeppelin",
        "class_hash": address_hex(&contract.class_hash(chain).unwrap()),
        "salt": "0x0",
        "vectors": entries,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
