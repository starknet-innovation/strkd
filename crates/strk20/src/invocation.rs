//! The virtual invoke a prover runs: the pool calling its own `compile_actions`.
//!
//! Shape (SDK `proof-invocation-factory.ts`):
//! - `sender_address` is the **pool**, its calldata the Cairo 1 `__execute__`
//!   wrapper around one call `pool.compile_actions(user_addr, user_sk, actions)`;
//! - nonce is the pool's nonce, resource prices are zero, tip zero, L1 DA modes;
//! - the **user's account key** signs the V3 hash. The pool's `__validate__`
//!   forwards it to the user account's `is_valid_signature`.
//!
//! `user_sk` (the viewing key) sits in the calldata in the clear. That is how the
//! protocol works, and it is why strkd proves locally: the invocation never has
//! to leave the machine except for deposits (`docs/project/strk20-plan.md` P1).

use krusty_kms_common::ChainId;
use serde_json::{json, Value};
use starknet_types_core::felt::Felt;
use wallet_core::{get_selector_from_name, invoke_v3_hash, Call, InvokeV3Params, ResourceBounds};

use crate::actions::{serialize_actions, ClientAction};

/// L2 gas budget the SDK gives a proof invocation (prices stay zero).
pub const L2_GAS_MAX_AMOUNT: u64 = 100_000_000;

/// An unsigned proof invocation.
#[derive(Debug, Clone)]
pub struct ProofInvocation {
    pub pool: Felt,
    pub chain: ChainId,
    pub nonce: Felt,
    /// `__execute__` calldata: one call to `pool.compile_actions`.
    pub calldata: Vec<Felt>,
}

impl ProofInvocation {
    pub fn new(
        pool: Felt,
        chain: ChainId,
        pool_nonce: Felt,
        user_addr: Felt,
        user_sk: Felt,
        actions: &[ClientAction],
    ) -> Self {
        let mut inner = vec![user_addr, user_sk];
        inner.extend(serialize_actions(actions));
        let call = Call {
            to: pool,
            selector: get_selector_from_name("compile_actions"),
            calldata: inner,
        };
        ProofInvocation {
            pool,
            chain,
            nonce: pool_nonce,
            calldata: wallet_core::encode_calls(&[call]),
        }
    }

    fn params(&self) -> InvokeV3Params {
        InvokeV3Params {
            nonce: self.nonce,
            l1_gas: ResourceBounds {
                max_amount: 1,
                max_price_per_unit: 0,
            },
            l2_gas: ResourceBounds {
                max_amount: L2_GAS_MAX_AMOUNT,
                max_price_per_unit: 0,
            },
            l1_data_gas: ResourceBounds {
                max_amount: 1,
                max_price_per_unit: 0,
            },
            ..InvokeV3Params::default()
        }
    }

    /// The V3 transaction hash the user's account must sign.
    pub fn hash(&self) -> Felt {
        // invoke_v3_hash re-encodes the multicall, so hand it the inner call.
        let call = self.inner_call();
        invoke_v3_hash(&self.pool, &[call], self.chain, &self.params())
    }

    fn inner_call(&self) -> Call {
        // calldata = [1, to, selector, len, ...inner]
        Call {
            to: self.calldata[1],
            selector: self.calldata[2],
            calldata: self.calldata[4..].to_vec(),
        }
    }

    /// The signed transaction in the JSON-RPC shape `starknet_proveTransaction`
    /// takes (a `BROADCASTED_INVOKE_TXN` v3).
    pub fn to_rpc_json(&self, signature: &[Felt]) -> Value {
        let hex = |f: &Felt| format!("{f:#x}");
        let bounds = |amount: u64| json!({ "max_amount": format!("{amount:#x}"), "max_price_per_unit": "0x0" });
        json!({
            "type": "INVOKE",
            "version": "0x3",
            "sender_address": hex(&self.pool),
            "calldata": self.calldata.iter().map(hex).collect::<Vec<_>>(),
            "signature": signature.iter().map(hex).collect::<Vec<_>>(),
            "nonce": hex(&self.nonce),
            "resource_bounds": {
                "l1_gas": bounds(1),
                "l2_gas": bounds(L2_GAS_MAX_AMOUNT),
                "l1_data_gas": bounds(1),
            },
            "tip": "0x0",
            "paymaster_data": [],
            "account_deployment_data": [],
            "nonce_data_availability_mode": "L1",
            "fee_data_availability_mode": "L1",
        })
    }
}
