//! Key derivation and account-address computation.
//!
//! All cryptography is delegated to `krusty-kms`. Private-key material lives in
//! short-lived `Zeroizing` buffers and is wiped as soon as the derived public
//! data (address / signature) has been produced.

use krusty_kms::{
    compute_typed_data_message_hash, derive_private_key_with_coin_type, sign_stark_hash,
    stark_public_key, StarkSignature,
};
use krusty_kms_common::ChainId;
use starknet_types_core::felt::Felt;

use crate::account_contract::{AccountContract, DeploymentData};
use crate::domain::Domain;
use crate::error::{CoreError, Result};

/// Derive the raw Stark private key for `(domain, index)`.
///
/// `Felt` is `Copy` and cannot be `Zeroize`d in place, so we keep the key's
/// lifetime as short as possible and never return or log it. For stronger
/// memory hygiene a future revision can thread krusty's `SecretFelt` through
/// the signing path (it zeroizes on drop) — see spec §5.2.
fn private_key(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
) -> Result<Felt> {
    derive_private_key_with_coin_type(
        mnemonic,
        index,
        domain.account_index(),
        domain.coin_type(),
        passphrase,
    )
    .map_err(|_| CoreError::Derivation)
}

/// Compute the Stark public key for `(domain, index)`.
pub fn public_key(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
) -> Result<Felt> {
    let sk = private_key(mnemonic, domain, index, passphrase)?;
    Ok(stark_public_key(&sk))
}

/// Counterfactual address for `(domain, index)` under `contract` on `chain`.
pub fn account_address(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain: ChainId,
    contract: AccountContract,
) -> Result<Felt> {
    Ok(deployment_data(mnemonic, domain, index, passphrase, chain, contract)?.address)
}

/// Counterfactual **OpenZeppelin** address for `(domain, index)` on `chain`.
///
/// A convenience for callers that specifically mean OZ (tests, examples, the
/// pre-multi-contract paths). Anything that should follow the account's own
/// contract must use [`account_address`] with the account's
/// [`AccountContract`].
pub fn oz_address(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain: ChainId,
) -> Result<Felt> {
    account_address(
        mnemonic,
        domain,
        index,
        passphrase,
        chain,
        AccountContract::OpenZeppelin,
    )
}

/// Full counterfactual deployment data for `(domain, index)` under `contract`.
///
/// The class hash, constructor calldata and salt all come from the
/// [`AccountContract`] seam, so adding a class does not mean revisiting this.
pub fn deployment_data(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain: ChainId,
    contract: AccountContract,
) -> Result<DeploymentData> {
    let pubkey = public_key(mnemonic, domain, index, passphrase)?;
    contract.deployment(&pubkey, chain)
}

/// Sign SNIP-12 typed data with the key for `(domain, index)`.
///
/// `typed_data_json` is a SNIP-12 typed-data document (types, primaryType,
/// domain, message). `account_address` is the signer account's address (it is
/// mixed into the message hash). Returns the signature over the computed hash.
pub fn sign_typed_data(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    typed_data_json: &str,
    account_address: &Felt,
) -> Result<StarkSignature> {
    let hash = typed_data_message_hash(typed_data_json, account_address)?;
    sign_hash(mnemonic, domain, index, passphrase, &hash)
}

/// Compute the SNIP-12 (revision 1) message hash that [`sign_typed_data`] signs.
///
/// This is the felt digest a signature over the typed data covers — the same
/// value starknet.js `typedData.getMessageHash(typed_data, account)` produces
/// and that a Cairo account's `is_valid_signature` checks against. It needs no
/// key material (it depends only on the document and the account address), so
/// callers can use it to verify strkd's hashing matches their expectation both
/// before and after signing. Delegated to `krusty-kms`.
pub fn typed_data_message_hash(typed_data_json: &str, account_address: &Felt) -> Result<Felt> {
    compute_typed_data_message_hash(typed_data_json, account_address).map_err(|_| CoreError::Signing)
}

/// Sign an already-computed message hash with the key for `(domain, index)`.
///
/// `hash` is a felt the caller produced (e.g. a transaction hash or a SNIP-12
/// typed-data hash). This function never sees the seed beyond derivation and
/// wipes the private key immediately after signing.
pub fn sign_hash(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    hash: &Felt,
) -> Result<StarkSignature> {
    let sk = private_key(mnemonic, domain, index, passphrase)?;
    sign_stark_hash(&sk, hash).map_err(|_| CoreError::Signing)
}
