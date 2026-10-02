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
    account_address, address_hex, get_selector_from_name, invoke_v3_hash, public_key,
    typed_data_message_hash, AccountContract, Call, ChainId, Domain, Felt, InvokeV3Params,
    ResourceBounds,
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

    // SNIP-12 rev 1 digests (the #7 defect lived here): a minimal message and
    // one that exercises the common basic types and a nested struct.
    // `string` and `selector` were wrong/unsupported before the krusty#111 pin
    // (strkd#32), so they are pinned here explicitly.
    //
    // `i128` is written as a JSON number: krusty's encoder (starknet-rust-core
    // 0.19.1) rejects it as a string, and rejects non-negative values entirely.
    // That is a known divergence, recorded in the fixture, not a vector.
    let signer = account_address(TEST_MNEMONIC, Domain::User, 0, None, chain, contract).unwrap();
    let typed = [
        serde_json::json!({
            "types": {
                "StarknetDomain": [
                    {"name": "name", "type": "shortstring"},
                    {"name": "version", "type": "shortstring"},
                    {"name": "chainId", "type": "shortstring"},
                    {"name": "revision", "type": "shortstring"}
                ],
                "Message": [{"name": "contents", "type": "felt"}]
            },
            "primaryType": "Message",
            "domain": {"name": "strkd", "version": "1", "chainId": "SN_SEPOLIA", "revision": "1"},
            "message": {"contents": "0x1"}
        }),
        serde_json::json!({
            "types": {
                "StarknetDomain": [
                    {"name": "name", "type": "shortstring"},
                    {"name": "version", "type": "shortstring"},
                    {"name": "chainId", "type": "shortstring"},
                    {"name": "revision", "type": "shortstring"}
                ],
                "Approval": [
                    {"name": "proposal", "type": "Proposal"},
                    {"name": "approver", "type": "ContractAddress"},
                    {"name": "weight", "type": "u128"},
                    {"name": "final", "type": "bool"},
                    {"name": "tags", "type": "felt*"},
                    {"name": "note", "type": "shortstring"},
                    {"name": "memo", "type": "string"},
                    {"name": "entrypoint", "type": "selector"}
                ],
                "Proposal": [
                    {"name": "id", "type": "felt"},
                    {"name": "target", "type": "ContractAddress"},
                    {"name": "deadline", "type": "timestamp"},
                    {"name": "delta", "type": "i128"}
                ]
            },
            "primaryType": "Approval",
            "domain": {"name": "committee", "version": "2", "chainId": "SN_MAIN", "revision": "1"},
            "message": {
                "proposal": {
                    "id": "0x2a",
                    "target": "0x049d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7",
                    "deadline": "1700000000",
                    "delta": -5
                },
                "approver": address_hex(&signer),
                "weight": "1000000",
                "final": true,
                "tags": ["0x1", "0x2", "0x3"],
                "note": "approve",
                "memo": "A ByteArray memo, long enough to span more than one 31-byte word.",
                "entrypoint": "transfer"
            }
        }),
    ];
    let typed_vectors: Vec<_> = typed
        .iter()
        .map(|td| {
            let h = typed_data_message_hash(&td.to_string(), &signer).unwrap();
            serde_json::json!({ "account_address": address_hex(&signer), "typed_data": td, "message_hash": address_hex(&h) })
        })
        .collect();

    // Invoke V3 transaction hashes: an STRK transfer from user account 0, on
    // both chains, with non-trivial nonce, tip and resource bounds.
    let strk = Felt::from_hex("0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d").unwrap();
    let to = account_address(TEST_MNEMONIC, Domain::User, 1, None, chain, contract).unwrap();
    let calls = vec![Call {
        to: strk,
        selector: get_selector_from_name("transfer"),
        calldata: vec![to, Felt::from(1_000_000_000_000_000_000u128), Felt::ZERO],
    }];
    let params = InvokeV3Params {
        nonce: Felt::from(7u64),
        tip: 3,
        l1_gas: ResourceBounds { max_amount: 0x1000, max_price_per_unit: 0x2540be400 },
        l2_gas: ResourceBounds { max_amount: 0x5f5e100, max_price_per_unit: 0x3b9aca00 },
        l1_data_gas: ResourceBounds { max_amount: 0x200, max_price_per_unit: 0x174876e800 },
        ..Default::default()
    };
    let invoke_vectors: Vec<_> = [(ChainId::Sepolia, "SN_SEPOLIA"), (ChainId::Mainnet, "SN_MAIN")]
        .into_iter()
        .map(|(c, name)| {
            let h = invoke_v3_hash(&signer, &calls, c, &params);
            serde_json::json!({
                "chain_id": name,
                "sender_address": address_hex(&signer),
                "calldata": wallet_core::encode_calls(&calls).iter().map(address_hex).collect::<Vec<_>>(),
                "nonce": "0x7",
                "tip": "0x3",
                "resource_bounds": {
                    "l1_gas": {"max_amount": "0x1000", "max_price_per_unit": "0x2540be400"},
                    "l2_gas": {"max_amount": "0x5f5e100", "max_price_per_unit": "0x3b9aca00"},
                    "l1_data_gas": {"max_amount": "0x200", "max_price_per_unit": "0x174876e800"}
                },
                "nonce_data_availability_mode": "L1",
                "fee_data_availability_mode": "L1",
                "transaction_hash": address_hex(&h),
            })
        })
        .collect();

    let doc = serde_json::json!({
        "_provenance": {
            "purpose": "Cross-wallet conformance: seed -> public key -> account address. \
strkd asserts against this in tests/conformance.rs; bramble can adopt the same file.",
            "generated_by": "cargo run --example conformance_vectors (wallet-core)",
            "verified_against": "conformance/verify.mjs (pinned starknet.js + @scure/bip32 + @scure/bip39), run in CI: \
re-derives every public key from the mnemonic (BIP-32 path -> EIP-2645 grindKey), every address via \
hash.calculateContractAddressFromHash (exactly the call bramble makes), every SNIP-12 digest via \
typedData.getMessageHash and every invoke hash via hash.calculateInvokeTransactionHash, independently of krusty.",
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
        "typed_data_vectors": typed_vectors,
        "known_divergences": [
            "SNIP-12 rev 1 `i128`: krusty (via starknet-rust-core 0.19.1) accepts only a negative JSON number; \
it rejects strings (\"-5\", \"5\") and non-negative numbers (5), all of which starknet.js accepts. strkd refuses \
to sign such messages (fails closed); when it does sign, the digest matches."
        ],
        "invoke_v3_vectors": invoke_vectors,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
