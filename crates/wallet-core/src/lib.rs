//! `wallet-core` — key management, derivation, signing and the encrypted vault
//! for the Starknet wallet companion.
//!
//! Security boundary: this crate is the only place that touches seed/private-key
//! material. It exposes derived **public** data (addresses, public keys,
//! signatures) and a passphrase-encrypted [`vault::EncryptedVault`]. It never
//! returns or logs private keys. See `spec/wallet-companion-spec.md`.
//!
//! All core cryptography is delegated to the (experimental) `krusty-kms`
//! crates; vault encryption uses mainstream audited crates (Argon2id +
//! AES-256-GCM).

pub mod accounts;
pub mod domain;
pub mod error;
pub mod keys;
pub mod tx;
pub mod vault;

pub use accounts::{AccountRef, Registry};
pub use domain::{Domain, AGENT_ACCOUNT_INDEX, STARKNET_COIN_TYPE, USER_ACCOUNT_INDEX};
pub use error::{CoreError, Result};
pub use keys::{deployment_data, oz_address, public_key, sign_hash, sign_typed_data, DeploymentData};
pub use tx::{
    encode_calls, get_selector_from_name, invoke_v3_hash, resolve_selector, sign_declare_v3,
    sign_deploy_account_v3, sign_invoke_v3, starknet_keccak, Call, InvokeV3Params, SignedDeclare,
    SignedDeployAccount, SignedInvoke,
};
pub use vault::EncryptedVault;

// Fee/DA types from krusty, surfaced so callers build params without depending
// on krusty directly.
pub use krusty_kms::tx_hash::{DaMode, ResourceBounds};

// Re-export the foreign types that appear in this crate's public API so callers
// don't need to depend on krusty/starknet-types-core directly.
pub use krusty_kms::StarkSignature;
pub use krusty_kms_common::ChainId;
pub use starknet_types_core::felt::Felt;

/// Generate a fresh BIP-39 mnemonic with the given word count (12 or 24).
///
/// Thin wrapper over krusty so onboarding code doesn't depend on krusty
/// directly. The returned phrase is secret material — handle accordingly.
pub fn generate_mnemonic(word_count: usize) -> Result<String> {
    krusty_kms::generate_mnemonic(word_count).map_err(|_| CoreError::Derivation)
}

/// Validate a BIP-39 mnemonic phrase (checksum + wordlist).
pub fn validate_mnemonic(phrase: &str) -> Result<()> {
    krusty_kms::validate_mnemonic(phrase).map_err(|_| CoreError::InvalidMnemonic)
}

/// Format a felt as a zero-padded 0x-prefixed 64-hex-digit string (the
/// canonical Starknet address/field-element form). Matches krusty's
/// `normalized_address_hex` and avoids leading-zero ambiguity.
pub fn address_hex(f: &Felt) -> String {
    format!("0x{:064x}", f)
}
