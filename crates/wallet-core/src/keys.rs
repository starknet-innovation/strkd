//! Key derivation and account-address computation.
//!
//! All cryptography is delegated to `krusty-kms`. Private-key material lives in
//! short-lived `Zeroizing` buffers and is wiped as soon as the derived public
//! data (address / signature) has been produced.

use krusty_kms::strk20::{derive_scoped_strk20_viewing_key, fold_strk20_viewing_key};
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
    // Which BIP-44 axis carries `index` depends on the branch — see
    // `domain::Domain::path_indices`. Bound it first: an out-of-range user
    // index would derive a key on the agent branch.
    domain.check_index(index)?;
    let (account_index, address_index) = domain.path_indices(index);
    derive_private_key_with_coin_type(
        mnemonic,
        address_index,
        account_index,
        domain.coin_type(),
        passphrase,
    )
    .map_err(|_| CoreError::Derivation)
}

/// Derive the STRK20 viewing key (`user_sk`) for `(domain, index)`, scoped to one
/// pool on one chain.
///
/// This is bramble's convention, so the same seed sees the same private notes in
/// both wallets: sign `starknet_keccak("<chain_id>:<pool>")` with the account key
/// (RFC 6979) and fold `Poseidon(r, s)` into `[1, n/2)`. krusty's
/// `derive_scoped_strk20_viewing_key` does the work and carries the known-answer
/// tests.
///
/// The result is a secret: it decrypts *and* spends the account's notes. It is
/// returned only because the pool's `compile_actions` takes it as calldata
/// (`docs/project/strk20-plan.md` §1); callers must not log or persist it.
pub fn strk20_viewing_key(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain_id: &Felt,
    pool: &Felt,
) -> Result<Felt> {
    let sk = private_key(mnemonic, domain, index, passphrase)?;
    derive_scoped_strk20_viewing_key(&sk, &format!("{chain_id:#x}"), &format!("{pool:#x}"))
        .map_err(|_| CoreError::Derivation)
}

/// Domain tag of [`strk20_owner_viewing_key`]. Bumping it changes every owner's
/// key, so it is part of the format.
pub const STRK20_OWNER_KEY_TAG: &str = "strkd.strk20.owner.v1";

/// The hash a controller key signs to derive an owner's STRK20 viewing key:
/// `starknet_keccak("strkd.strk20.owner.v1:<chain_id>:<pool>:<owner>")`, each
/// value as shortest lowercase `0x` hex. Zero values are refused.
pub fn strk20_owner_viewing_key_message(chain_id: &Felt, pool: &Felt, owner: &Felt) -> Result<Felt> {
    if [chain_id, pool, owner].iter().any(|f| **f == Felt::ZERO) {
        return Err(CoreError::Derivation);
    }
    Ok(crate::tx::starknet_keccak(
        format!("{STRK20_OWNER_KEY_TAG}:{chain_id:#x}:{pool:#x}:{owner:#x}").as_bytes(),
    ))
}

/// Derive the STRK20 viewing key (`user_sk`) strkd holds for an external owner
/// contract at `owner`, from the key of the controller account `(domain, index)`
/// (`docs/project/design-48-strk20-external-owner.md` §3.2).
///
/// Same construction as [`strk20_viewing_key`] — sign a scope hash with the
/// account key (RFC 6979) and fold `Poseidon(r, s)` into `[1, n/2)` — but over
/// [`strk20_owner_viewing_key_message`], whose domain tag keeps it apart from the
/// controller's own STRK20 key. Not bramble-compatible, by design (D1).
///
/// The result is a secret: it decrypts *and* spends the owner's notes (subject
/// to the owner contract's own signature check). Callers must not log, persist
/// or return it.
pub fn strk20_owner_viewing_key(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain_id: &Felt,
    pool: &Felt,
    owner: &Felt,
) -> Result<Felt> {
    let message = strk20_owner_viewing_key_message(chain_id, pool, owner)?;
    let sk = private_key(mnemonic, domain, index, passphrase)?;
    let sig = sign_stark_hash(&sk, &message).map_err(|_| CoreError::Derivation)?;
    fold_strk20_viewing_key(&sig.public_key, &message, &sig.r, &sig.s).map_err(|_| CoreError::Derivation)
}
/// Compute the Stark public key for `(domain, index)`.
pub fn public_key(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
) -> Result<Felt> {
    let sk = private_key(mnemonic, domain, index, passphrase)?;
    stark_public_key(&sk).map_err(|_| CoreError::Derivation)
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

/// Sign `hash` with a caller-held Stark private key (RFC 6979), for keys that
/// are not derived from the seed: today only a STRK20 *test* screener key for a
/// self-deployed testnet pool (`strk20::screening`). The caller owns the key's
/// storage and must not log or return it.
pub fn sign_hash_with_key(private_key: &Felt, hash: &Felt) -> Result<StarkSignature> {
    sign_stark_hash(private_key, hash).map_err(|_| CoreError::Signing)
}

/// The Stark public key of a caller-held private key. Errors (rather than
/// panicking) on zero or an out-of-range key.
pub fn public_key_of_key(private_key: &Felt) -> Result<Felt> {
    stark_public_key(private_key).map_err(|_| CoreError::Derivation)
}
