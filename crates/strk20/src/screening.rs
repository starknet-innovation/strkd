//! Depositor screening attestations, mirroring `packages/privacy/src/snip12.cairo`
//! (starknet-privacy, deployed class `0x6d163f2b…cf83`).
//!
//! The pool accepts a deposit only with a `ScreeningAttestation { issued_at,
//! signature }` signed by its `screener_public_key` over the SNIP-12 (revision 1)
//! message
//!
//! ```text
//! poseidon('StarkNet Message', domain_hash, screener_public_key, struct_hash)
//! domain_hash = poseidon(STARKNET_DOMAIN_TYPE_HASH, 'Screening', 2, chain_id, 1)
//! struct_hash = poseidon(DEPOSITOR_VALIDATION_TYPE_HASH, depositor, issued_at)
//! ```
//!
//! On the canonical pools only the operator's screener holds that key. A pool
//! someone deploys for testing names its own screener key in the constructor,
//! and then whoever holds it can attest deposits locally. strkd does that only
//! on testnets (`wallet-rpc`'s `strk20::screener`). The attestation is not bound
//! to the proof, so it is signed after proving.

use starknet_types_core::felt::Felt;
use starknet_types_core::hash::{Poseidon, StarkHash};

use crate::apply::ScreeningAttestation;
use crate::hashes::short_string;

/// `starknet_keccak` of SNIP-12's revision-1 `StarknetDomain` type.
pub const STARKNET_DOMAIN_TYPE_HASH: &str = "0x1ff2f602e42168014d405a94f75e8a93d640751d71d16311266e140d8b0a210";
/// `starknet_keccak("\"DepositorValidation\"(\"depositor\":\"ContractAddress\",\"issued_at\":\"u128\")")`.
pub const DEPOSITOR_VALIDATION_TYPE_HASH: &str = "0x32d43b7372c9ea8a35daf12b02c5f6f74837910ecbaf2a3ecfe71fec901913d";
/// Domain name `'Screening'`; the version is the numeric felt 2, not `'2'`.
const DOMAIN_NAME: &str = "Screening";
const DOMAIN_VERSION: u64 = 2;
const DOMAIN_REVISION: u64 = 1;

fn constant(s: &str) -> Felt {
    Felt::from_hex(s).expect("constant felt")
}

/// The message hash the pool verifies for `depositor` at `issued_at` on
/// `chain_id`, under the screener key `signer_public_key`.
pub fn message_hash(chain_id: Felt, depositor: Felt, issued_at: u64, signer_public_key: Felt) -> Felt {
    let domain = Poseidon::hash_array(&[
        constant(STARKNET_DOMAIN_TYPE_HASH),
        short_string(DOMAIN_NAME),
        Felt::from(DOMAIN_VERSION),
        chain_id,
        Felt::from(DOMAIN_REVISION),
    ]);
    let message = Poseidon::hash_array(&[constant(DEPOSITOR_VALIDATION_TYPE_HASH), depositor, Felt::from(issued_at)]);
    Poseidon::hash_array(&[short_string("StarkNet Message"), domain, signer_public_key, message])
}

/// Sign an attestation for `depositor` with a screener private key. The key is
/// the caller's; it is used for this one signature and never stored here.
pub fn attest(
    screener_key: &Felt,
    chain_id: Felt,
    depositor: Felt,
    issued_at: u64,
) -> Result<ScreeningAttestation, wallet_core::CoreError> {
    let public_key = wallet_core::public_key_of_key(screener_key)?;
    let hash = message_hash(chain_id, depositor, issued_at, public_key);
    let sig = wallet_core::sign_hash_with_key(screener_key, &hash)?;
    Ok(ScreeningAttestation { issued_at, sig_r: sig.r, sig_s: sig.s })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_hashes_are_the_snip12_encodings() {
        let domain = wallet_core::starknet_keccak(
            b"\"StarknetDomain\"(\"name\":\"shortstring\",\"version\":\"shortstring\",\"chainId\":\"shortstring\",\"revision\":\"shortstring\")",
        );
        assert_eq!(domain, constant(STARKNET_DOMAIN_TYPE_HASH));
        let validation =
            wallet_core::starknet_keccak(b"\"DepositorValidation\"(\"depositor\":\"ContractAddress\",\"issued_at\":\"u128\")");
        assert_eq!(validation, constant(DEPOSITOR_VALIDATION_TYPE_HASH));
    }
}
