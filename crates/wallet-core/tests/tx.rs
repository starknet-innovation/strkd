//! Tests for selector derivation, multicall encoding, and invoke V3 signing.
//! Public test vector only; no real key material.

use wallet_core::{
    encode_calls, get_selector_from_name, invoke_v3_hash, resolve_selector, sign_invoke_v3,
    Call, ChainId, Domain, Felt, InvokeV3Params, ResourceBounds,
};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// The ERC20 `transfer` selector is a well-known Starknet constant. This is a
/// golden known-answer test: if our keccak/masking is wrong, it fails.
#[test]
fn transfer_selector_matches_known_value() {
    let sel = get_selector_from_name("transfer");
    let expected =
        Felt::from_hex("0x0083afd3f4caedc6eebf44246fe54e38c95e3179a5ec9ea81740eca5b482d12e")
            .unwrap();
    assert_eq!(sel, expected);
}

#[test]
fn resolve_selector_accepts_name_or_hex() {
    let by_name = resolve_selector("transfer").unwrap();
    let by_hex =
        resolve_selector("0x0083afd3f4caedc6eebf44246fe54e38c95e3179a5ec9ea81740eca5b482d12e")
            .unwrap();
    assert_eq!(by_name, by_hex);
}

#[test]
fn encode_calls_uses_cairo1_layout() {
    let to = Felt::from_hex("0xabc").unwrap();
    let selector = get_selector_from_name("transfer");
    let recipient = Felt::from_hex("0xdead").unwrap();
    let amount_low = Felt::from(1000u64);
    let amount_high = Felt::ZERO;

    let calls = vec![Call {
        to,
        selector,
        calldata: vec![recipient, amount_low, amount_high],
    }];

    let encoded = encode_calls(&calls);
    // [ n_calls, to, selector, calldata_len, ...calldata ]
    assert_eq!(encoded[0], Felt::from(1u64)); // one call
    assert_eq!(encoded[1], to);
    assert_eq!(encoded[2], selector);
    assert_eq!(encoded[3], Felt::from(3u64)); // calldata_len
    assert_eq!(encoded[4], recipient);
    assert_eq!(encoded[5], amount_low);
    assert_eq!(encoded[6], amount_high);
    assert_eq!(encoded.len(), 7);
}

#[test]
fn encode_calls_handles_multiple_and_empty_calldata() {
    let calls = vec![
        Call {
            to: Felt::from(1u64),
            selector: Felt::from(2u64),
            calldata: vec![],
        },
        Call {
            to: Felt::from(3u64),
            selector: Felt::from(4u64),
            calldata: vec![Felt::from(5u64)],
        },
    ];
    let e = encode_calls(&calls);
    // n=2 | (1,2,0) | (3,4,1,5)
    assert_eq!(
        e,
        vec![
            Felt::from(2u64),
            Felt::from(1u64),
            Felt::from(2u64),
            Felt::from(0u64),
            Felt::from(3u64),
            Felt::from(4u64),
            Felt::from(1u64),
            Felt::from(5u64),
        ]
    );
}

fn sample_params() -> InvokeV3Params {
    InvokeV3Params {
        nonce: Felt::from(7u64),
        tip: 0,
        l1_gas: ResourceBounds {
            max_amount: 1000,
            max_price_per_unit: 1,
        },
        l2_gas: ResourceBounds {
            max_amount: 1_000_000,
            max_price_per_unit: 1,
        },
        l1_data_gas: ResourceBounds {
            max_amount: 1000,
            max_price_per_unit: 1,
        },
        ..Default::default()
    }
}

fn sample_calls() -> Vec<Call> {
    vec![Call {
        to: Felt::from_hex("0x49d36").unwrap(),
        selector: get_selector_from_name("transfer"),
        calldata: vec![Felt::from_hex("0xdead").unwrap(), Felt::from(10000u64), Felt::ZERO],
    }]
}

#[test]
fn invoke_v3_hash_is_deterministic_and_nonzero() {
    let sender = Felt::from_hex("0x1234").unwrap();
    let calls = sample_calls();
    let params = sample_params();
    let h1 = invoke_v3_hash(&sender, &calls, ChainId::Sepolia, &params);
    let h2 = invoke_v3_hash(&sender, &calls, ChainId::Sepolia, &params);
    assert_eq!(h1, h2);
    assert_ne!(h1, Felt::ZERO);

    // Chain id is part of the preimage → different chains, different hash.
    let h_main = invoke_v3_hash(&sender, &calls, ChainId::Mainnet, &params);
    assert_ne!(h1, h_main);
}

#[test]
fn sign_invoke_v3_binds_signature_to_hash_and_account() {
    let sender =
        wallet_core::oz_address(TEST_MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap();
    let calls = sample_calls();
    let params = sample_params();

    let signed = sign_invoke_v3(
        TEST_MNEMONIC,
        Domain::User,
        0,
        None,
        &sender,
        &calls,
        ChainId::Sepolia,
        &params,
    )
    .unwrap();

    // Hash matches the standalone computation.
    assert_eq!(
        signed.transaction_hash,
        invoke_v3_hash(&sender, &calls, ChainId::Sepolia, &params)
    );
    // Signature is well-formed and the encoded calldata is the multicall array.
    assert_ne!(signed.r, Felt::ZERO);
    assert_ne!(signed.s, Felt::ZERO);
    assert_eq!(signed.calldata, encode_calls(&calls));

    // Deterministic (RFC-6979).
    let again = sign_invoke_v3(
        TEST_MNEMONIC, Domain::User, 0, None, &sender, &calls, ChainId::Sepolia, &params,
    )
    .unwrap();
    assert_eq!(signed.r, again.r);
    assert_eq!(signed.s, again.s);

    // A different account produces a different signature for the same tx.
    let other = sign_invoke_v3(
        TEST_MNEMONIC, Domain::User, 1, None, &sender, &calls, ChainId::Sepolia, &params,
    )
    .unwrap();
    assert_ne!(signed.s, other.s);
}
