//! Invoke V3 transaction construction: entry-point selectors, multicall
//! calldata encoding, V3 hash, and signing.
//!
//! krusty provides the V3 hash over an already-flattened calldata array
//! (`compute_invoke_v3_hash`) but no selector helper and no multicall encoder,
//! so those two well-specified primitives live here.
//!
//! Calldata uses the **Cairo 1 / SNIP-6** `__execute__` layout used by modern
//! OpenZeppelin/Argent/Braavos accounts:
//! ```text
//! [ n_calls, (to, selector, calldata_len, calldata...)* ]
//! ```

use krusty_kms::tx_hash::{
    compute_declare_v3_hash, compute_deploy_account_v3_hash, compute_invoke_v3_hash_with_proof_facts,
    DaMode, ResourceBounds,
};
use krusty_kms_common::ChainId;
use sha3::{Digest, Keccak256};
use starknet_types_core::felt::Felt;

use crate::domain::Domain;
use crate::error::{CoreError, Result};
use crate::keys::{deployment_data, sign_hash};

/// A single contract call in a multicall.
#[derive(Debug, Clone)]
pub struct Call {
    pub to: Felt,
    pub selector: Felt,
    pub calldata: Vec<Felt>,
}

/// Fee / nonce parameters for a V3 invoke. The exotic fields default to empty
/// (`Default`): no paymaster, no account-deployment data, L1 DA modes.
#[derive(Debug, Clone)]
pub struct InvokeV3Params {
    pub nonce: Felt,
    pub tip: u64,
    pub l1_gas: ResourceBounds,
    pub l2_gas: ResourceBounds,
    pub l1_data_gas: ResourceBounds,
    pub paymaster_data: Vec<Felt>,
    pub account_deployment_data: Vec<Felt>,
    /// SNIP-36 proof facts. When non-empty, `Poseidon(proof_facts)` is appended
    /// to the V3 hash preimage (a proof-carrying invoke), so the signature
    /// covers them. Empty = a standard invoke (identical hash to before).
    pub proof_facts: Vec<Felt>,
}

impl Default for InvokeV3Params {
    fn default() -> Self {
        InvokeV3Params {
            nonce: Felt::ZERO,
            tip: 0,
            l1_gas: ResourceBounds::zero(),
            l2_gas: ResourceBounds::zero(),
            l1_data_gas: ResourceBounds::zero(),
            paymaster_data: Vec::new(),
            account_deployment_data: Vec::new(),
            proof_facts: Vec::new(),
        }
    }
}

/// Starknet's domain-separated keccak: keccak256 with the top 6 bits cleared so
/// the result fits in a 250-bit field element. Matches the definition krusty
/// uses internally for typed-data type hashes.
pub fn starknet_keccak(data: &[u8]) -> Felt {
    let mut bytes: [u8; 32] = Keccak256::digest(data).into();
    bytes[0] &= 0x03; // clear top 6 bits → 250-bit value
    Felt::from_bytes_be(&bytes)
}

/// Compute an entry-point selector from a function name
/// (`get_selector_from_name`): `starknet_keccak(name)`.
pub fn get_selector_from_name(name: &str) -> Felt {
    starknet_keccak(name.as_bytes())
}

/// Resolve a request's `entry_point_selector` field, which may be either an
/// already-computed selector (`0x…` hex) or a function name (`"transfer"`).
pub fn resolve_selector(s: &str) -> Result<Felt> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Felt::from_hex(&format!("0x{hex}")).map_err(|_| CoreError::Address)
    } else {
        Ok(get_selector_from_name(s))
    }
}

/// Encode a multicall into the Cairo 1 `__execute__` calldata array.
pub fn encode_calls(calls: &[Call]) -> Vec<Felt> {
    let mut out = Vec::new();
    out.push(Felt::from(calls.len() as u64));
    for call in calls {
        out.push(call.to);
        out.push(call.selector);
        out.push(Felt::from(call.calldata.len() as u64));
        out.extend_from_slice(&call.calldata);
    }
    out
}

/// Compute the V3 invoke transaction hash for `sender` executing `calls`.
pub fn invoke_v3_hash(
    sender: &Felt,
    calls: &[Call],
    chain: ChainId,
    params: &InvokeV3Params,
) -> Felt {
    let calldata = encode_calls(calls);
    // Empty proof_facts ⇒ identical to a standard invoke hash; non-empty ⇒
    // SNIP-36 proof-carrying hash (Poseidon(proof_facts) appended).
    compute_invoke_v3_hash_with_proof_facts(
        sender,
        &calldata,
        &chain.as_felt(),
        &params.nonce,
        &params.account_deployment_data,
        params.tip,
        &params.l1_gas,
        &params.l2_gas,
        &params.l1_data_gas,
        &params.paymaster_data,
        DaMode::L1,
        DaMode::L1,
        &params.proof_facts,
    )
}

/// The product of signing an invoke: the tx hash, the encoded calldata (so the
/// caller can broadcast), and the signature `(r, s)`.
#[derive(Debug, Clone)]
pub struct SignedInvoke {
    pub transaction_hash: Felt,
    pub calldata: Vec<Felt>,
    pub r: Felt,
    pub s: Felt,
}

/// The product of signing a DEPLOY_ACCOUNT v3: the tx hash, the deployment
/// fields the node needs to broadcast it, and the signature.
#[derive(Debug, Clone)]
pub struct SignedDeployAccount {
    pub transaction_hash: Felt,
    pub address: Felt,
    pub class_hash: Felt,
    pub salt: Felt,
    pub constructor_calldata: Vec<Felt>,
    pub r: Felt,
    pub s: Felt,
}

/// Build, hash, and sign a DEPLOY_ACCOUNT v3 for `(domain, index)`'s OZ account.
///
/// `params.nonce` must be 0 (a deploy_account is the account's first tx). The
/// account must already hold funds to pay its own deploy fee. Sign-only — the
/// caller broadcasts via `starknet_addDeployAccountTransaction`.
pub fn sign_deploy_account_v3(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    chain: ChainId,
    params: &InvokeV3Params,
) -> Result<SignedDeployAccount> {
    let d = deployment_data(mnemonic, domain, index, passphrase, chain)?;
    let hash = compute_deploy_account_v3_hash(
        &d.address,
        &d.class_hash,
        &d.constructor_calldata,
        &d.salt,
        &chain.as_felt(),
        &params.nonce,
        params.tip,
        &params.l1_gas,
        &params.l2_gas,
        &params.l1_data_gas,
        &params.paymaster_data,
        DaMode::L1,
        DaMode::L1,
    );
    let sig = sign_hash(mnemonic, domain, index, passphrase, &hash)?;
    Ok(SignedDeployAccount {
        transaction_hash: hash,
        address: d.address,
        class_hash: d.class_hash,
        salt: d.salt,
        constructor_calldata: d.constructor_calldata,
        r: sig.r,
        s: sig.s,
    })
}

/// The product of signing a DECLARE v3: the tx hash + signature.
#[derive(Debug, Clone)]
pub struct SignedDeclare {
    pub transaction_hash: Felt,
    pub r: Felt,
    pub s: Felt,
}

/// Hash of a DECLARE v3 (SNIP-8 Poseidon layout), independent of signature.
///
/// `class_hash` **must** be the hash a node derives from the broadcast
/// `contract_class` (see [`crate::class_hash::SierraClass::class_hash`]); the
/// node recomputes it and validates the signature against the resulting tx
/// hash, so a mismatching caller-supplied value yields "invalid signature".
pub fn declare_v3_hash(
    sender: &Felt,
    class_hash: &Felt,
    compiled_class_hash: &Felt,
    chain: ChainId,
    params: &InvokeV3Params,
) -> Felt {
    compute_declare_v3_hash(
        sender,
        class_hash,
        compiled_class_hash,
        &chain.as_felt(),
        &params.nonce,
        params.tip,
        &params.l1_gas,
        &params.l2_gas,
        &params.l1_data_gas,
        &params.paymaster_data,
        DaMode::L1,
        DaMode::L1,
        &params.account_deployment_data,
    )
}

/// Build, hash, and sign a DECLARE v3. The signature only needs the Sierra
/// `class_hash` and the CASM `compiled_class_hash`; broadcasting additionally
/// needs the full contract class (caller-supplied). Sign-only.
#[allow(clippy::too_many_arguments)]
pub fn sign_declare_v3(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    sender: &Felt,
    class_hash: &Felt,
    compiled_class_hash: &Felt,
    chain: ChainId,
    params: &InvokeV3Params,
) -> Result<SignedDeclare> {
    let hash = declare_v3_hash(sender, class_hash, compiled_class_hash, chain, params);
    let sig = sign_hash(mnemonic, domain, index, passphrase, &hash)?;
    Ok(SignedDeclare {
        transaction_hash: hash,
        r: sig.r,
        s: sig.s,
    })
}

/// Build, hash, and sign a V3 invoke with the key for `(domain, index)`.
///
/// Sign-only: this never broadcasts. The hash is independent of the signature,
/// so the returned `transaction_hash` is the hash the tx will have once the
/// caller broadcasts it with the given params.
#[allow(clippy::too_many_arguments)]
pub fn sign_invoke_v3(
    mnemonic: &str,
    domain: Domain,
    index: u32,
    passphrase: Option<&str>,
    sender: &Felt,
    calls: &[Call],
    chain: ChainId,
    params: &InvokeV3Params,
) -> Result<SignedInvoke> {
    let calldata = encode_calls(calls);
    let hash = invoke_v3_hash(sender, calls, chain, params);
    let sig = sign_hash(mnemonic, domain, index, passphrase, &hash)?;
    Ok(SignedInvoke {
        transaction_hash: hash,
        calldata,
        r: sig.r,
        s: sig.s,
    })
}
