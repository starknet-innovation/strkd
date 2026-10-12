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
use wallet_core::{strk20_viewing_key, Domain};

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
