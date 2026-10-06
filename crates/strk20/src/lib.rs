//! `strk20` — a client for the Starknet privacy pool (STRK20), ported from
//! StarkWare's TypeScript SDK (`starkware-libs/starknet-privacy`, `sdk/`).
//!
//! What a wallet does, and where it lives here:
//! - derive the viewing key `user_sk`: `wallet_core::strk20_viewing_key`;
//! - find its channels and notes by reading the pool and decrypting:
//!   [`discovery`] over a [`discovery::PoolReader`], with [`crypto`] and [`hashes`];
//! - plan a batch of [`actions::ClientAction`]s (indices, change notes): [`planner`];
//! - wrap them in the virtual invoke a prover runs: [`invocation`];
//! - turn the proof's output into the on-chain `apply_actions` call: [`apply`].
//!
//! The pool's own `compile_actions` performs every encryption inside the proof,
//! so this crate only decrypts. It holds no keys of its own: the caller passes
//! `user_sk` in, and signing stays in `wallet-core`. See
//! `docs/project/strk20-plan.md` for the protocol summary and decisions.

pub mod actions;
pub mod apply;
pub mod crypto;
pub mod discovery;
pub mod hashes;
pub mod invocation;
pub mod planner;

use starknet_types_core::felt::Felt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0:#x} is not the x-coordinate of a point on the Stark curve")]
    NotOnCurve(Felt),
    #[error("the proof carries no message from the pool")]
    EmptyProofOutput,
    #[error("{0:#x} has not registered a viewing key with the pool")]
    NotRegistered(Felt),
    #[error("insufficient private balance of {token:#x}: {shortfall} short (have {available})")]
    InsufficientBalance {
        token: Felt,
        shortfall: u128,
        available: u128,
    },
    #[error("amount must be positive")]
    ZeroAmount,
    #[error("pool read failed: {0}")]
    Read(String),
    #[error("randomness unavailable: {0}")]
    Random(String),
}
