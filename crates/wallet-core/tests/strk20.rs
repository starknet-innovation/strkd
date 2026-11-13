//! STRK20 viewing-key derivation, checked against keys a live pool accepted.
//!
//! Each expected value is the viewing **public** key the Sepolia pool stored for a
//! test account (`get_public_key`), registered by a proof built from this
//! derivation (phase 0, `docs/project/strk20-plan.md` §2b). Matching them means
//! strkd derives the same `user_sk` the pool saw, and therefore the same one
//! bramble derives for this seed.
//!
//! The mnemonic is the published BIP-39 all-`abandon` test vector; never a real
//! seed.

use krusty_kms::stark_public_key;
use krusty_kms_common::ChainId;
use starknet_types_core::felt::Felt;
use wallet_core::{strk20_owner_viewing_key, strk20_owner_viewing_key_message, strk20_viewing_key, Domain};

const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

fn felt(hex: &str) -> Felt {
    Felt::from_hex(hex).unwrap()
}

/// The Sepolia pool running the mainnet class (deployed by a third party).
const CANONICAL_SEPOLIA_POOL: &str =
    "0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e";
/// strkd's own Sepolia test pool (same class, test screener key).
const TEST_POOL: &str = "0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323";

fn registered_key(index: u32, pool: &str) -> Felt {
    let vk = strk20_viewing_key(
        MNEMONIC,
        Domain::User,
        index,
        None,
        &ChainId::Sepolia.as_felt(),
        &felt(pool),
    )
    .unwrap();
    stark_public_key(&vk).unwrap()
}

#[test]
fn viewing_key_matches_live_registrations() {
    assert_eq!(
        registered_key(0, CANONICAL_SEPOLIA_POOL),
        felt("0x17b6d465d88101899739ed61541e9aeaa3e9109b72d3a586edbe4e0655bc4a2"),
    );
    assert_eq!(
        registered_key(0, TEST_POOL),
        felt("0x28f95cba58596b35cae6856d4670a8577a7390f5df371700429c3c8a0daaf37"),
    );
    assert_eq!(
        registered_key(1, TEST_POOL),
        felt("0x61e00fd6b5c92edccec6e9dce621f0ebb2d74795d148ed27bbf432507a20888"),
    );
}

#[test]
fn viewing_key_is_scoped_to_pool_and_chain() {
    let pool = felt(TEST_POOL);
    let vk = |chain: ChainId, pool: &Felt| {
        strk20_viewing_key(MNEMONIC, Domain::User, 0, None, &chain.as_felt(), pool).unwrap()
    };
    assert_ne!(vk(ChainId::Sepolia, &pool), vk(ChainId::Mainnet, &pool));
    assert_ne!(vk(ChainId::Sepolia, &pool), vk(ChainId::Sepolia, &felt(CANONICAL_SEPOLIA_POOL)));
}

// ── owner viewing keys (#48, design note §3.2) ──────────────────────────────

fn owner_key(index: u32, chain: ChainId, pool: &str, owner: &str) -> Felt {
    strk20_owner_viewing_key(MNEMONIC, Domain::User, index, None, &chain.as_felt(), &felt(pool), &felt(owner)).unwrap()
}

/// Known-answer vectors for the owner key, from the test seed. The message
/// hashes were reproduced independently (keccak-256 of the tag string, top six
/// bits cleared, pycryptodome); the keys are pinned as their public keys so a
/// change to the construction shows up here. No pool has registered these.
#[test]
fn owner_viewing_key_known_answers() {
    let chain = ChainId::Sepolia.as_felt();
    let pool = felt(CANONICAL_SEPOLIA_POOL);
    assert_eq!(
        strk20_owner_viewing_key_message(&chain, &pool, &felt("0x1234")).unwrap(),
        felt("0x26e069e1518072ee37ede4183038fbfa50328f7b1d549d0764f5f765ddfa8d4"),
    );
    assert_eq!(
        strk20_owner_viewing_key_message(&chain, &pool, &felt("0x5678")).unwrap(),
        felt("0x27ce1059691de6da945511c2cbf4dcc5fdd6528065ce13ff5e022802cd22b0d"),
    );
    assert_eq!(
        stark_public_key(&owner_key(0, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, "0x1234")).unwrap(),
        felt("0x5b9c157d480682fd660acbbeff3ecd4f6dad0e48b9591a8e2343a56e47488b"),
    );
    assert_eq!(
        stark_public_key(&owner_key(0, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, "0x5678")).unwrap(),
        felt("0x5bc86f33f5f5de25c5164a6809495f9177bc465655ef548326b481cc9cc1e92"),
    );
}

/// The owner key is the same sign-and-fold as the account key, over a
/// domain-tagged message: never the controller's own STRK20 key, and different
/// per owner, controller, pool and chain.
#[test]
fn owner_viewing_key_is_separate_from_the_controllers_key() {
    let own = strk20_viewing_key(
        MNEMONIC,
        Domain::User,
        0,
        None,
        &ChainId::Sepolia.as_felt(),
        &felt(CANONICAL_SEPOLIA_POOL),
    )
    .unwrap();
    let a = owner_key(0, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, "0x1234");
    assert_ne!(a, own);
    // Even with the controller's own address as the "owner".
    let controller = wallet_core::oz_address(MNEMONIC, Domain::User, 0, None, ChainId::Sepolia).unwrap();
    assert_ne!(owner_key(0, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, &format!("{controller:#x}")), own);
    assert_ne!(a, owner_key(0, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, "0x5678"));
    assert_ne!(a, owner_key(1, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, "0x1234"));
    assert_ne!(a, owner_key(0, ChainId::Sepolia, TEST_POOL, "0x1234"));
    assert_ne!(a, owner_key(0, ChainId::Mainnet, CANONICAL_SEPOLIA_POOL, "0x1234"));
    assert_eq!(a, owner_key(0, ChainId::Sepolia, CANONICAL_SEPOLIA_POOL, "0x1234"), "deterministic");
}

#[test]
fn owner_viewing_key_refuses_zero_scope() {
    let chain = ChainId::Sepolia.as_felt();
    let pool = felt(TEST_POOL);
    assert!(strk20_owner_viewing_key(MNEMONIC, Domain::User, 0, None, &chain, &pool, &Felt::ZERO).is_err());
    assert!(strk20_owner_viewing_key(MNEMONIC, Domain::User, 0, None, &chain, &Felt::ZERO, &felt("0x1")).is_err());
}
