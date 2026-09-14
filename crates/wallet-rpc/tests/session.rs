//! Direct tests for `WalletSession` account creation (used by the desktop UI).
//! Public test vector only.

use wallet_core::{ChainId, Domain, Registry};
use wallet_rpc::WalletSession;

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[test]
fn create_user_account_derives_user_domain_and_grows_registry() {
    let mut s =
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "pass", Registry::default());

    let a0 = s.create_user_account("Main").unwrap();
    assert_eq!(a0.domain, Domain::User);
    assert_eq!(a0.index, 0);
    assert!(a0.owner_client_id.is_none());
    assert!(a0.address.starts_with("0x"));

    let a1 = s.create_user_account("Savings").unwrap();
    assert_eq!(a1.index, 1);
    assert_ne!(a0.address, a1.address);

    assert_eq!(s.registry().unwrap().accounts.len(), 2);
}

#[test]
fn create_user_account_fails_when_locked() {
    let mut s = WalletSession::new_locked(ChainId::Sepolia);
    assert!(s.create_user_account("x").is_err());
}

#[test]
fn sign_deploy_account_produces_hash_signature_and_deployment_fields() {
    use wallet_core::{Felt, InvokeV3Params, ResourceBounds};

    let mut s =
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "pass", Registry::default());
    let acct = s.create_user_account("Main").unwrap();

    let params = InvokeV3Params {
        nonce: Felt::ZERO,
        tip: 0,
        l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 1 },
        l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 1 },
        l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 1 },
        ..Default::default()
    };
    let signed = s.sign_deploy_account_for(&acct, s.chain(), &params).unwrap();

    assert_ne!(signed.transaction_hash, Felt::ZERO);
    assert_ne!(signed.r, Felt::ZERO);
    assert_ne!(signed.s, Felt::ZERO);
    // The deploy target address matches the account's address.
    assert_eq!(wallet_core::address_hex(&signed.address), acct.address);
    assert_eq!(signed.constructor_calldata.len(), 1); // OZ ctor = [public_key]
}

// --- Seed reveal (#28) ----------------------------------------------------
// Public test vector only; never a real seed.

#[test]
fn reveal_returns_the_phrase_when_the_passphrase_is_right() {
    use wallet_rpc::reveal_mnemonic;

    let s =
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "correct horse", Registry::default());
    let vault = s.reseal().unwrap();

    let revealed = reveal_mnemonic(&vault, "correct horse").unwrap();
    assert_eq!(&*revealed as &str, TEST_MNEMONIC);
}

#[test]
fn reveal_rejects_a_wrong_passphrase() {
    use wallet_rpc::reveal_mnemonic;

    let s =
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "correct horse", Registry::default());
    let vault = s.reseal().unwrap();

    // Re-authentication is the AEAD tag, not a comparison: a wrong passphrase
    // cannot decrypt, so there is no path that returns a phrase without it.
    assert!(reveal_mnemonic(&vault, "wrong").is_err());
    assert!(reveal_mnemonic(&vault, "").is_err());
    assert!(reveal_mnemonic(&vault, "correct horse ").is_err(), "not trimmed or fuzzy-matched");
}

/// Being unlocked must not be sufficient. The session already holds the
/// mnemonic, so reveal deliberately goes back to the vault — otherwise "reveal"
/// would just mean "the app is open".
#[test]
fn reveal_does_not_depend_on_session_state() {
    use wallet_rpc::reveal_mnemonic;

    let s =
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "pass", Registry::default());
    let vault = s.reseal().unwrap();
    drop(s);

    // No session in scope at all; the vault and passphrase are the whole input.
    assert_eq!(&*reveal_mnemonic(&vault, "pass").unwrap() as &str, TEST_MNEMONIC);
}
