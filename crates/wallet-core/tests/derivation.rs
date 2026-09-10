//! Derivation + signing tests.
//!
//! These use the **public** BIP-39 all-zero-entropy test vector
//! ("abandon … about"), a standard throwaway test mnemonic with no funds. No
//! real seed or key material is involved. See `spec/portability-test-plan.md`.
//!
//! The assertions here are *structural* (determinism, domain isolation,
//! well-formedness). Cross-wallet golden-address vectors (portability T1/T2 and
//! agent isolation T4) require an independent reference and belong in a pinned
//! known-answer test added under security review — see the test plan.

use std::collections::HashSet;

use wallet_core::{
    address_hex, generate_mnemonic, oz_address, public_key, sign_hash, validate_mnemonic, ChainId,
    Domain, Felt, AGENT_ACCOUNT_INDEX, USER_ACCOUNT_INDEX,
};

/// Public, well-known BIP-39 test vector (entropy = all zeros). Test-only.
const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[test]
fn generated_mnemonics_are_valid_and_distinct() {
    let m12 = generate_mnemonic(12).expect("gen 12");
    let m24 = generate_mnemonic(24).expect("gen 24");
    assert_eq!(m12.split_whitespace().count(), 12);
    assert_eq!(m24.split_whitespace().count(), 24);
    validate_mnemonic(&m12).expect("m12 valid");
    validate_mnemonic(&m24).expect("m24 valid");
    // Two freshly generated mnemonics should differ.
    assert_ne!(m12, generate_mnemonic(12).expect("gen 12 again"));
}

#[test]
fn rejects_invalid_mnemonic() {
    assert!(validate_mnemonic("not a real mnemonic phrase at all whoops").is_err());
}

#[test]
fn derivation_is_deterministic() {
    let chain = ChainId::Sepolia;
    let a = oz_address(TEST_MNEMONIC, Domain::User, 0, None, chain).unwrap();
    let b = oz_address(TEST_MNEMONIC, Domain::User, 0, None, chain).unwrap();
    assert_eq!(a, b, "same inputs must yield same address");

    let pk_a = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    let pk_b = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    assert_eq!(pk_a, pk_b, "public key derivation must be deterministic");
}

#[test]
fn addresses_are_wellformed_and_distinct_across_indices() {
    let chain = ChainId::Sepolia;
    let mut seen = HashSet::new();
    for i in 0..5u32 {
        let addr = oz_address(TEST_MNEMONIC, Domain::User, i, None, chain).unwrap();
        assert_ne!(addr, Felt::ZERO, "address must be non-zero");
        assert!(seen.insert(addr), "addresses across indices must be distinct");

        let hex = address_hex(&addr);
        assert!(hex.starts_with("0x"));
        assert_eq!(hex.len(), 66, "0x + 64 hex digits");
    }
}

#[test]
fn user_and_agent_domains_are_isolated() {
    let chain = ChainId::Sepolia;
    // Sanity on the reserved constants.
    assert_eq!(USER_ACCOUNT_INDEX, 0);
    assert_eq!(AGENT_ACCOUNT_INDEX, 0x4147_4E54, "\"AGNT\"");
    // The branches walk different BIP-44 axes, so they can never meet.
    assert_ne!(Domain::User.path_indices(0), Domain::Agent.path_indices(0));

    let mut all = HashSet::new();
    for i in 0..5u32 {
        let user = oz_address(TEST_MNEMONIC, Domain::User, i, None, chain).unwrap();
        let agent = oz_address(TEST_MNEMONIC, Domain::Agent, i, None, chain).unwrap();
        assert_ne!(
            user, agent,
            "user and agent accounts at the same index must differ"
        );
        assert!(all.insert(user), "no collision across domains/indices");
        assert!(all.insert(agent), "no collision across domains/indices");
    }
}

#[test]
fn derivation_paths_match_expected_layout() {
    // User accounts walk the ACCOUNT index, which is what bramble's recovery
    // scan enumerates — so the same seed surfaces the same accounts in both.
    assert_eq!(Domain::User.path(0), "m/44'/9004'/0'/0/0");
    assert_eq!(Domain::User.path(3), "m/44'/9004'/3'/0/0");
    // Agent accounts share the reserved account index and walk the ADDRESS
    // index, keeping the whole branch off the axis user accounts occupy.
    assert_eq!(Domain::Agent.path(0), "m/44'/9004'/1095192148'/0/0");
    assert_eq!(Domain::Agent.path(2), "m/44'/9004'/1095192148'/0/2");
}

/// Account 0 is the one path both wallets always agreed on, even before #16.
/// If this ever changes, seed portability is broken at the root.
#[test]
fn user_account_zero_is_the_canonical_first_account() {
    assert_eq!(Domain::User.path(0), "m/44'/9004'/0'/0/0");
    assert_eq!(Domain::User.path_indices(0), (0, 0));
}

/// The agent branch must be far enough up the account axis that ordinary use
/// cannot reach it. Bramble scans 0-19 by default; 0x41 (65) was reachable.
#[test]
fn the_agent_branch_is_out_of_reach_of_account_enumeration() {
    let (agent_account, _) = Domain::Agent.path_indices(0);
    assert!(agent_account > 1_000_000, "must be far beyond any wallet's scan");
    assert_ne!(agent_account, 0x41, "0x41 became reachable once user accounts walked this axis");
}

#[test]
fn passphrase_changes_derivation() {
    let chain = ChainId::Sepolia;
    let no_pass = oz_address(TEST_MNEMONIC, Domain::User, 0, None, chain).unwrap();
    let with_pass = oz_address(TEST_MNEMONIC, Domain::User, 0, Some("hunter2"), chain).unwrap();
    assert_ne!(
        no_pass, with_pass,
        "BIP-39 passphrase must change the derived account"
    );
}

#[test]
fn signing_is_deterministic_and_bound_to_account() {
    // RFC-6979 deterministic ECDSA: signing the same hash twice is identical.
    let hash = Felt::from_hex("0x1234abcd").unwrap();
    let s1 = sign_hash(TEST_MNEMONIC, Domain::User, 0, None, &hash).unwrap();
    let s2 = sign_hash(TEST_MNEMONIC, Domain::User, 0, None, &hash).unwrap();
    assert_eq!(s1.r, s2.r);
    assert_eq!(s1.s, s2.s);
    assert_ne!(s1.r, Felt::ZERO);
    assert_ne!(s1.s, Felt::ZERO);

    // The signature carries the signer's public key; it must match derivation.
    let pk = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    assert_eq!(s1.public_key, pk);

    // A different account signing the same hash yields a different signature.
    let other = sign_hash(TEST_MNEMONIC, Domain::Agent, 0, None, &hash).unwrap();
    assert_ne!(s1.s, other.s);
}

/// The address a seed produces must be the one bramble produces from the same
/// public key. This is what "same seed, same accounts" means in practice, and
/// it is the whole point of issue #16.
///
/// The expected value was cross-checked with starknet.js:
/// `hash.calculateContractAddressFromHash("0x0", OZ_CLASS, [publicKeyX], 0)`
/// — exactly the call bramble makes — and agrees to the digit.
#[test]
fn user_account_zero_matches_brambles_address_formula() {
    let chain = ChainId::Sepolia;
    let d = wallet_core::deployment_data(
        TEST_MNEMONIC,
        Domain::User,
        0,
        None,
        chain,
        wallet_core::AccountContract::OpenZeppelin,
    )
    .unwrap();

    // Bramble salts OpenZeppelin deployments with zero, not the public key.
    assert_eq!(d.salt, Felt::ZERO, "OZ salt must be zero to match bramble");
    // Pinned OpenZeppelin AccountUpgradeable 3.0, identical on mainnet and Sepolia.
    assert_eq!(
        address_hex(&d.class_hash),
        "0x01d1777db36cdd06dd62cfde77b1b6ae06412af95d57a13dc40ac77b8a702381"
    );
    // The constructor commits to the key — which is why a zero salt is safe here.
    assert_eq!(d.constructor_calldata.len(), 1);

    assert_eq!(
        address_hex(&d.address),
        "0x0497e8446398aa1c30e6533382cedc0bca6df29f059fdbb227d9b727c04b7b4f",
        "address drifted from bramble's formula — re-check the salt and class hash",
    );
}
