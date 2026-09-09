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
    address_hex, generate_mnemonic, oz_address, public_key, sign_hash, tongo_keypair,
    validate_mnemonic, ChainId, Domain, Felt, AGENT_ACCOUNT_INDEX, USER_ACCOUNT_INDEX,
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
    assert_eq!(AGENT_ACCOUNT_INDEX, 0x41);
    assert_ne!(Domain::User.account_index(), Domain::Agent.account_index());

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
    // User branch must match Argent's portable base: m/44'/9004'/0'/0/i
    assert_eq!(Domain::User.path(3), "m/44'/9004'/0'/0/3");
    // Agent branch on the reserved account index.
    assert_eq!(Domain::Agent.path(2), "m/44'/9004'/65'/0/2");
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

#[test]
fn tongo_keypair_is_deterministic_and_isolated_from_stark_branch() {
    // Deterministic: same (mnemonic, domain, index) → same Tongo public key.
    let a = tongo_keypair(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    let b = tongo_keypair(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    let ax = a.public_key.to_affine().unwrap();
    let bx = b.public_key.to_affine().unwrap();
    assert_eq!(ax.x(), bx.x());
    assert_ne!(ax.x(), Felt::ZERO);

    // Distinct per index and per domain (agent branch is isolated).
    let idx1 = tongo_keypair(TEST_MNEMONIC, Domain::User, 1, None).unwrap();
    let agent = tongo_keypair(TEST_MNEMONIC, Domain::Agent, 0, None).unwrap();
    assert_ne!(ax.x(), idx1.public_key.to_affine().unwrap().x());
    assert_ne!(ax.x(), agent.public_key.to_affine().unwrap().x());

    // The Tongo branch (coin type 5454) must not collide with the Stark
    // signing key's public key (coin type 9004) for the same account.
    let stark_pk = public_key(TEST_MNEMONIC, Domain::User, 0, None).unwrap();
    assert_ne!(ax.x(), stark_pk);
}
