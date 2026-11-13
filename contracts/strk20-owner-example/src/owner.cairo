//! Reference STRK20 owner contract for strkd issue #48. Not audited: a starting point, not a
//! production wallet.
//!
//! A STRK20 note owner that is a contract, not a key. The privacy pool authenticates every
//! private operation by calling back into the owner (`assert_valid_signature`, pool
//! `utils.cairo`). This contract answers through the SRC5-gated custom-validation path:
//! `is_custom_signature_valid` returns `VALIDATED` only for a plan whose SNIP-12 `Strk20Plan`
//! hash an admin approved on-chain beforehand. See `README.md` next to this file and
//! `docs/project/design-48-strk20-external-owner.md` §5.3.

use core::hash::{HashStateExTrait, HashStateTrait};
use core::poseidon::PoseidonTrait;
use openzeppelin_utils::snip12::{StarknetDomain, StructHash};
use starknet::ContractAddress;
use starknet::account::Call;

/// SRC5 interface id the pool checks before trusting custom validation:
/// `selector!("is_custom_signature_valid")` (pool `utils.cairo`,
/// `ICUSTOM_SIGNATURE_VALIDATION_ID`).
pub const ICUSTOM_SIGNATURE_VALIDATION_ID: felt252 =
    0x2eb9faa4ce06e09879e93803798b87d22877b73964334356bf9e0d8cc22ded;

/// SNIP-12 domain name and version of strkd's plan-approval format. A format change bumps the
/// version (and strkd's `plan_version`) together.
pub const STRK20_PLAN_DOMAIN_NAME: felt252 = 'strkd.Strk20Plan';
/// The felt 1, NOT the shortstring '1' (0x31). The typed-data JSON says `"version": "1"`, and
/// SNIP-12 tooling (starknet.js `getHex`, krusty-kms) encodes a decimal-digit string as the
/// number it spells, the same rule that makes `"revision": "1"` the felt 1. A Cairo `'1'` here
/// would silently produce a different hash. Checked by the shared vector (README, "Test vector").
pub const STRK20_PLAN_DOMAIN_VERSION: felt252 = 1;

/// SNIP-12 revision 1 type hash of `Strk20Plan`:
/// `selector!("\"Strk20Plan\"(\"pool\":\"ContractAddress\",\"owner\":\"ContractAddress\",\"actions_hash\":\"felt\")")`.
/// `ContractAddress` and `felt` are both basic types and encode as the felt itself.
pub const STRK20_PLAN_TYPE_HASH: felt252 = selector!(
    "\"Strk20Plan\"(\"pool\":\"ContractAddress\",\"owner\":\"ContractAddress\",\"actions_hash\":\"felt\")",
);

/// What an admin approves. `actions_hash` is `poseidon_hash_span` of the serialized
/// `Span<ClientAction>`, i.e. `compile_actions` calldata from index 2 on (length prefix included).
#[derive(Copy, Drop, Hash, Serde, Debug, PartialEq)]
pub struct Strk20Plan {
    pub pool: ContractAddress,
    pub owner: ContractAddress,
    pub actions_hash: felt252,
}

/// The pool's custom-validation interface (pool `utils.cairo`, `ICustomSignatureValidation`).
#[starknet::interface]
pub trait ICustomSignatureValidation<TState> {
    fn is_custom_signature_valid(
        self: @TState, calls: Span<Call>, additional_data: Span<felt252>, signature: Span<felt252>,
    ) -> felt252;
}

/// The pool's fallback interface (pool `utils.cairo`, `IAccount`). Implemented strictly.
#[starknet::interface]
pub trait IAccount<TState> {
    fn is_valid_signature(self: @TState, hash: felt252, signature: Array<felt252>) -> felt252;
}

#[starknet::interface]
pub trait IStrk20PlanApprovals<TState> {
    /// Approve a `Strk20Plan` SNIP-12 hash (strkd's `plan_hash`). Admin only.
    fn approve_plan(ref self: TState, plan_hash: felt252);
    /// Withdraw an approval. Admin only. Has no effect on a proof already built against a
    /// block at which the approval was live (see README, "Revocation").
    fn revoke_plan(ref self: TState, plan_hash: felt252);
    fn is_plan_approved(self: @TState, plan_hash: felt252) -> bool;
    /// The `plan_hash` this contract expects for `actions_hash` on the current chain.
    fn compute_plan_hash(self: @TState, actions_hash: felt252) -> felt252;
    fn pool(self: @TState) -> ContractAddress;
    fn admin(self: @TState) -> ContractAddress;
}

#[starknet::contract]
pub mod Strk20OwnerExample {
    use core::hash::{HashStateExTrait, HashStateTrait};
    use core::num::traits::Zero;
    use core::poseidon::{PoseidonTrait, poseidon_hash_span};
    use openzeppelin_introspection::src5::SRC5Component;
    use openzeppelin_utils::snip12::{OffchainMessageHash, SNIP12Metadata, StructHash};
    use starknet::account::Call;
    use starknet::storage::{
        Map, StorageMapReadAccess, StorageMapWriteAccess, StoragePointerReadAccess,
        StoragePointerWriteAccess,
    };
    use starknet::{ContractAddress, VALIDATED, get_caller_address, get_contract_address};
    use super::{
        IAccount, ICUSTOM_SIGNATURE_VALIDATION_ID, ICustomSignatureValidation, IStrk20PlanApprovals,
        STRK20_PLAN_DOMAIN_NAME, STRK20_PLAN_DOMAIN_VERSION, STRK20_PLAN_TYPE_HASH, Strk20Plan,
    };

    component!(path: SRC5Component, storage: src5, event: SRC5Event);

    #[abi(embed_v0)]
    impl SRC5Impl = SRC5Component::SRC5Impl<ContractState>;
    impl SRC5InternalImpl = SRC5Component::InternalImpl<ContractState>;

    pub mod Errors {
        pub const NOT_ADMIN: felt252 = 'Strk20Owner: not admin';
        pub const ZERO_ADMIN: felt252 = 'Strk20Owner: zero admin';
        pub const ZERO_POOL: felt252 = 'Strk20Owner: zero pool';
        pub const ZERO_HASH: felt252 = 'Strk20Owner: zero plan hash';
    }

    #[storage]
    struct Storage {
        #[substorage(v0)]
        src5: SRC5Component::Storage,
        pool: ContractAddress,
        admin: ContractAddress,
        approved_plans: Map<felt252, bool>,
    }

    #[event]
    #[derive(Drop, starknet::Event)]
    pub enum Event {
        #[flat]
        SRC5Event: SRC5Component::Event,
        PlanApproved: PlanApproved,
        PlanRevoked: PlanRevoked,
    }

    #[derive(Drop, Debug, PartialEq, starknet::Event)]
    pub struct PlanApproved {
        #[key]
        pub plan_hash: felt252,
        pub by: ContractAddress,
    }

    #[derive(Drop, Debug, PartialEq, starknet::Event)]
    pub struct PlanRevoked {
        #[key]
        pub plan_hash: felt252,
        pub by: ContractAddress,
    }

    /// `pool` is fixed for the contract's lifetime: a plan for any other pool never validates.
    /// `admin` is the only address that can approve or revoke. There is no admin transfer and no
    /// upgrade path; deploy a new owner to change either.
    #[constructor]
    fn constructor(ref self: ContractState, admin: ContractAddress, pool: ContractAddress) {
        assert(admin.is_non_zero(), Errors::ZERO_ADMIN);
        assert(pool.is_non_zero(), Errors::ZERO_POOL);
        self.admin.write(admin);
        self.pool.write(pool);
        // SRC5's own id is answered by the component itself.
        self.src5.register_interface(ICUSTOM_SIGNATURE_VALIDATION_ID);
    }

    impl SNIP12MetadataImpl of SNIP12Metadata {
        fn name() -> felt252 {
            STRK20_PLAN_DOMAIN_NAME
        }
        fn version() -> felt252 {
            STRK20_PLAN_DOMAIN_VERSION
        }
    }

    impl StructHashImpl of StructHash<Strk20Plan> {
        fn hash_struct(self: @Strk20Plan) -> felt252 {
            PoseidonTrait::new().update_with(STRK20_PLAN_TYPE_HASH).update_with(*self).finalize()
        }
    }

    #[abi(embed_v0)]
    impl CustomSignatureValidationImpl of ICustomSignatureValidation<ContractState> {
        /// Returns `VALIDATED` iff `calls` is exactly the pool's `compile_actions` call for this
        /// owner and the resulting `Strk20Plan` hash is approved in storage. Anything else
        /// returns 0 (never panics on malformed input, so the verdict is always explicit).
        ///
        /// `signature` and `additional_data` are deliberately ignored: authorisation is the
        /// on-chain approval, read from storage at the proof's reference block. A signature
        /// scheme layered on top would add nothing an approval does not already prove, and the
        /// pool always passes `additional_data` empty.
        fn is_custom_signature_valid(
            self: @ContractState,
            calls: Span<Call>,
            additional_data: Span<felt252>,
            signature: Span<felt252>,
        ) -> felt252 {
            if calls.len() != 1 {
                return 0;
            }
            let call = calls[0];
            let pool = self.pool.read();
            if *call.to != pool || *call.selector != selector!("compile_actions") {
                return 0;
            }
            let data = *call.calldata;
            // [owner, user_sk, ...serialized Span<ClientAction>]; the span carries at least its
            // length prefix, so a well-formed call has 3 or more felts.
            if data.len() < 3 {
                return 0;
            }
            let me = get_contract_address();
            if *data[0] != me.into() {
                return 0;
            }
            // data[1] is the owner's STRK20 viewing key (user_sk). Never read, store or emit it.
            let plan = Strk20Plan {
                pool, owner: me, actions_hash: poseidon_hash_span(data.slice(2, data.len() - 2)),
            };
            if self.approved_plans.read(plan.get_message_hash(me)) {
                VALIDATED
            } else {
                0
            }
        }
    }

    #[abi(embed_v0)]
    impl AccountImpl of IAccount<ContractState> {
        /// Always 0. The pool falls back to this entrypoint (over the tx hash, then over the
        /// SNIP-12 `CallSet` hash) whenever custom validation does not accept. Any non-zero
        /// answer here would bypass plan approval, so it must stay strict.
        fn is_valid_signature(
            self: @ContractState, hash: felt252, signature: Array<felt252>,
        ) -> felt252 {
            0
        }
    }

    #[abi(embed_v0)]
    impl Strk20PlanApprovalsImpl of IStrk20PlanApprovals<ContractState> {
        fn approve_plan(ref self: ContractState, plan_hash: felt252) {
            self.assert_admin();
            assert(plan_hash.is_non_zero(), Errors::ZERO_HASH);
            self.approved_plans.write(plan_hash, true);
            self.emit(PlanApproved { plan_hash, by: get_caller_address() });
        }

        fn revoke_plan(ref self: ContractState, plan_hash: felt252) {
            self.assert_admin();
            self.approved_plans.write(plan_hash, false);
            self.emit(PlanRevoked { plan_hash, by: get_caller_address() });
        }

        fn is_plan_approved(self: @ContractState, plan_hash: felt252) -> bool {
            self.approved_plans.read(plan_hash)
        }

        fn compute_plan_hash(self: @ContractState, actions_hash: felt252) -> felt252 {
            let me = get_contract_address();
            Strk20Plan { pool: self.pool.read(), owner: me, actions_hash }.get_message_hash(me)
        }

        fn pool(self: @ContractState) -> ContractAddress {
            self.pool.read()
        }

        fn admin(self: @ContractState) -> ContractAddress {
            self.admin.read()
        }
    }

    #[generate_trait]
    impl InternalImpl of InternalTrait {
        fn assert_admin(self: @ContractState) {
            assert(get_caller_address() == self.admin.read(), Errors::NOT_ADMIN);
        }
    }
}

/// The SNIP-12 `Strk20Plan` message hash for an explicit chain id, without a contract. Same
/// computation as the contract's (OZ `OffchainMessageHashImpl`), spelled out so off-chain
/// tooling and tests can check each step.
pub fn strk20_plan_hash(
    chain_id: felt252, pool: ContractAddress, owner: ContractAddress, actions_hash: felt252,
) -> felt252 {
    let domain = StarknetDomain {
        name: STRK20_PLAN_DOMAIN_NAME, version: STRK20_PLAN_DOMAIN_VERSION, chain_id, revision: 1,
    };
    let plan = Strk20Plan { pool, owner, actions_hash };
    let plan_struct = PoseidonTrait::new()
        .update_with(STRK20_PLAN_TYPE_HASH)
        .update_with(plan)
        .finalize();
    PoseidonTrait::new()
        .update_with('StarkNet Message')
        .update_with(domain.hash_struct())
        .update_with(owner)
        .update_with(plan_struct)
        .finalize()
}
