//! Vault tests: round-trip, wrong passphrase, tamper detection, persistence,
//! and a full seed+registry seal/open cycle.
//!
//! Uses the public BIP-39 test vector only; no real secrets.

use wallet_core::accounts::{AccountRef, Registry};
use wallet_core::{CoreError, Domain, EncryptedVault};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[test]
fn seal_open_roundtrip() {
    let secret = b"the seed bytes (or serialized vault contents)";
    let vault = EncryptedVault::seal("correct horse battery staple", secret).unwrap();
    let opened = vault.open("correct horse battery staple").unwrap();
    assert_eq!(opened.as_slice(), secret);
}

#[test]
fn wrong_passphrase_fails() {
    let vault = EncryptedVault::seal("right-pass", b"secret").unwrap();
    let err = vault.open("wrong-pass");
    assert!(err.is_err(), "wrong passphrase must not decrypt");
}

#[test]
fn ciphertext_is_not_plaintext() {
    let secret = b"abandon abandon ... about";
    let vault = EncryptedVault::seal("pw", secret).unwrap();
    // The sealed blob must not contain the plaintext.
    assert!(!vault
        .ciphertext
        .windows(secret.len())
        .any(|w| w == secret));
}

#[test]
fn tamper_is_detected() {
    let mut vault = EncryptedVault::seal("pw", b"secret payload").unwrap();
    // Flip a byte in the ciphertext; AES-GCM tag must reject it.
    vault.ciphertext[0] ^= 0xff;
    assert!(vault.open("pw").is_err(), "tampering must fail the AEAD tag");
}

#[test]
fn distinct_salt_and_nonce_each_seal() {
    let a = EncryptedVault::seal("pw", b"same plaintext").unwrap();
    let b = EncryptedVault::seal("pw", b"same plaintext").unwrap();
    // Random salt + nonce ⇒ different KDF output and different ciphertext.
    assert_ne!(a.kdf.salt, b.kdf.salt);
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(a.ciphertext, b.ciphertext);
}

#[test]
fn json_persistence_roundtrip() {
    let vault = EncryptedVault::seal("pw", b"persist me").unwrap();
    let json = vault.to_json().unwrap();
    let parsed = EncryptedVault::from_json(&json).unwrap();
    assert_eq!(vault, parsed);
    assert_eq!(parsed.open("pw").unwrap().as_slice(), b"persist me");
}

/// End-to-end: serialize a seed + account registry, seal it, reopen it, and
/// confirm the registry survives — the real shape of what the app stores.
#[test]
fn seed_and_registry_seal_cycle() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
    struct VaultContents {
        mnemonic: String,
        registry: Registry,
    }

    let mut registry = Registry::default();
    registry.add(AccountRef {
        domain: Domain::User,
        index: registry.next_index(Domain::User),
        address: "0x0123".into(),
        label: "Main".into(),
        contract: Default::default(),
        owner_client_id: None,
    });
    registry.add(AccountRef {
        domain: Domain::Agent,
        index: registry.next_index(Domain::Agent),
        address: "0x0abc".into(),
        label: "agent-bot".into(),
        contract: Default::default(),
        owner_client_id: Some("client-42".into()),
    });

    let contents = VaultContents {
        mnemonic: TEST_MNEMONIC.to_string(),
        registry,
    };
    let plaintext = serde_json::to_vec(&contents).unwrap();

    let vault = EncryptedVault::seal("vault-pass", &plaintext).unwrap();
    let opened = vault.open("vault-pass").unwrap();
    let restored: VaultContents = serde_json::from_slice(&opened).unwrap();

    assert_eq!(restored, contents);

    // Scoping: the agent client sees only its own account; a user/app caller
    // (None) sees only user accounts.
    let agent_view: Vec<_> = restored.registry.scoped_for(Some("client-42")).collect();
    assert_eq!(agent_view.len(), 1);
    assert_eq!(agent_view[0].domain, Domain::Agent);

    let user_view: Vec<_> = restored.registry.scoped_for(None).collect();
    assert_eq!(user_view.len(), 1);
    assert_eq!(user_view[0].domain, Domain::User);

    // A different agent client sees nothing.
    assert_eq!(restored.registry.scoped_for(Some("other")).count(), 0);
}

/// A vault from an older format must be reported as *unsupported*, not as a bad
/// passphrase — even when the passphrase is correct.
///
/// This is the distinction the desktop unlock screen collapsed: it mapped every
/// failure to "incorrect passphrase or corrupt vault", so after the #16 version
/// bump a perfectly intact v1 vault told the user it might be corrupt. That is
/// both false and dangerous, since the obvious response is to delete it.
#[test]
fn an_older_vault_version_is_unsupported_not_a_bad_passphrase() {
    let sealed = EncryptedVault::seal("passphrase", b"payload").unwrap();
    assert!(sealed.is_supported_version());
    assert_eq!(sealed.version, EncryptedVault::supported_version());

    // Same vault, older format marker.
    let mut older = sealed.clone();
    older.version = sealed.version - 1;

    assert!(!older.is_supported_version());
    assert!(
        matches!(
            older.open("passphrase"),
            Err(CoreError::UnsupportedVaultVersion(v)) if v == sealed.version - 1
        ),
        "the right passphrase on an old vault must still say 'unsupported version'",
    );
}

/// And the inverse: a wrong passphrase on a current vault is still a passphrase
/// error, so the two cases stay distinguishable in both directions.
#[test]
fn a_wrong_passphrase_on_a_current_vault_is_still_a_passphrase_error() {
    let sealed = EncryptedVault::seal("passphrase", b"payload").unwrap();
    assert!(matches!(
        sealed.open("wrong"),
        Err(CoreError::BadPassphraseOrCorrupt)
    ));
}
