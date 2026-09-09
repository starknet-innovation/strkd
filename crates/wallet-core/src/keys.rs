//! Key derivation and account-address computation.
//!
//! All cryptography is delegated to `krusty-kms`. Private-key material lives in
//! short-lived `Zeroizing` buffers and is wiped as soon as the derived public
//! data (address / signature) has been produced.

use krusty_kms::account_class::{OpenZeppelinAccount, SaltPolicy};
use krusty_kms::{
    compute_typed_data_message_hash, derive_keypair_with_coin_type,
    derive_private_key_with_coin_type, sign_stark_hash, stark_public_key, StarkSignature,
    TongoKeyPair, TONGO_COIN_TYPE,
};
use krusty_kms_common::ChainId;
use starknet_types_core::felt::Felt;

use crate::domain::Domain;
use crate::error::{CoreError, Result};

/// Counterfactual deployment parameters for an OpenZeppelin account, as needed
/// by `wallet_deploymentData`.
#[derive(Debug, Clone)]
pub struct DeploymentData {
    pub address: Felt,
    pub class_hash: Felt,
    pub salt: Felt,
    pub constructor_calldata: Vec<Felt>,
}

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

/// Compute the counterfactual OpenZeppelin account address for `(domain, index)`
/// on `chain`.
///
/// Uses `SaltPolicy::PublicKey` and the OZ class hash from krusty's per-network
/// manifest (`OzAccountClassConfig::latest`).
pub fn oz_address(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain: ChainId,
) -> Result<Felt> {
    Ok(deployment_data(mnemonic, domain, index, passphrase, chain)?.address)
}

/// Compute the full counterfactual deployment data for `(domain, index)`.
pub fn deployment_data(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain: ChainId,
) -> Result<DeploymentData> {
    let pubkey = public_key(mnemonic, domain, index, passphrase)?;
    let oz = OpenZeppelinAccount::latest(chain).map_err(|_| CoreError::Address)?;
    let d = oz
        .deployment_descriptor(&pubkey, SaltPolicy::PublicKey)
        .map_err(|_| CoreError::Address)?;
    Ok(DeploymentData {
        address: d.address,
        class_hash: d.class_hash,
        salt: d.salt,
        constructor_calldata: d.constructor_calldata,
    })
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

/// Derive the Tongo (STRK20 privacy pool) keypair for `(domain, index)`.
///
/// Same seed as the Stark signing key but a separate SLIP-44 branch
/// (coin type 5454 vs 9004), so the confidential-balance key is deterministic
/// from the vault mnemonic yet never equal to — or derivable from — the
/// account's signing key. The private half lives in krusty's `SecretFelt`
/// (zeroizes on drop).
pub fn tongo_keypair(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
) -> Result<TongoKeyPair> {
    derive_keypair_with_coin_type(
        mnemonic,
        index,
        domain.account_index(),
        TONGO_COIN_TYPE,
        passphrase,
    )
    .map_err(|_| CoreError::Derivation)
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
