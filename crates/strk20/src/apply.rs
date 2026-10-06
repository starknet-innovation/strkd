//! From a proof to the on-chain `pool.apply_actions(actions, screening)` call.
//!
//! The prover returns the pool's L2→L1 message, payload `[class_hash,
//! ...Span<ServerAction>]`. `apply_actions` takes the span (class hash stripped)
//! followed by a Serde-encoded `Option<ScreeningAttestation>`: `[0x1]` for none,
//! `[0x0, issued_at, sig_r, sig_s]` for some. The attestation is not part of the
//! proof, so it is appended here. The pool requires one exactly when the batch
//! contains a deposit.

use serde::Deserialize;
use starknet_types_core::felt::Felt;
use wallet_core::{get_selector_from_name, Call};

use crate::Error;

/// A depositor screening attestation (`additional_data.signature` in a prover
/// response).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ScreeningAttestation {
    pub issued_at: u64,
    pub sig_r: Felt,
    pub sig_s: Felt,
}

/// Seconds an attestation stays valid, measured against the block timestamp
/// (`DEPOSITOR_VALIDATION_MAX_AGE`).
pub const ATTESTATION_MAX_AGE_SECS: u64 = 300;

/// Build the `apply_actions` call from the pool's message payload.
pub fn apply_actions_call(
    pool: Felt,
    message_payload: &[Felt],
    screening: Option<&ScreeningAttestation>,
) -> Result<Call, Error> {
    let (_class_hash, actions) = message_payload
        .split_first()
        .ok_or(Error::EmptyProofOutput)?;
    let mut calldata = actions.to_vec();
    match screening {
        None => calldata.push(Felt::ONE),
        Some(a) => calldata.extend([Felt::ZERO, Felt::from(a.issued_at), a.sig_r, a.sig_s]),
    }
    Ok(Call {
        to: pool,
        selector: get_selector_from_name("apply_actions"),
        calldata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_class_hash_and_appends_option() {
        let payload = [Felt::from(0xc1a55u64), Felt::ONE, Felt::from(7u64)];
        let none = apply_actions_call(Felt::TWO, &payload, None).unwrap();
        assert_eq!(none.calldata, vec![Felt::ONE, Felt::from(7u64), Felt::ONE]);

        let a = ScreeningAttestation {
            issued_at: 1_700_000_000,
            sig_r: Felt::from(3u64),
            sig_s: Felt::from(4u64),
        };
        let some = apply_actions_call(Felt::TWO, &payload, Some(&a)).unwrap();
        assert_eq!(
            some.calldata,
            vec![
                Felt::ONE,
                Felt::from(7u64),
                Felt::ZERO,
                Felt::from(1_700_000_000u64),
                Felt::from(3u64),
                Felt::from(4u64)
            ]
        );
    }

    #[test]
    fn rejects_empty_payload() {
        assert!(apply_actions_call(Felt::TWO, &[], None).is_err());
    }
}
