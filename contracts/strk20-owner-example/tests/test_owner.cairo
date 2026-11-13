use core::poseidon::poseidon_hash_span;
use openzeppelin_interfaces::introspection::{ISRC5Dispatcher, ISRC5DispatcherTrait, ISRC5_ID};
use snforge_std::{
    ContractClassTrait, DeclareResultTrait, EventSpyAssertionsTrait, declare, spy_events,
    start_cheat_caller_address, start_cheat_chain_id, stop_cheat_caller_address,
};
use starknet::account::Call;
use starknet::{ContractAddress, VALIDATED};
use strk20_owner_example::owner::Strk20OwnerExample::{Event, PlanApproved, PlanRevoked};
use strk20_owner_example::owner::{
    IAccountDispatcher, IAccountDispatcherTrait, ICUSTOM_SIGNATURE_VALIDATION_ID,
    ICustomSignatureValidationDispatcher, ICustomSignatureValidationDispatcherTrait,
    IStrk20PlanApprovalsDispatcher, IStrk20PlanApprovalsDispatcherTrait, STRK20_PLAN_TYPE_HASH,
    strk20_plan_hash,
};

// --- Shared SNIP-12 `Strk20Plan` vector ----------------------------------------------------
// Source of truth: crates/wallet-core/tests/fixtures/strk20-plan-vector.json. The same values
// are checked in Rust (crates/wallet-core/tests/strk20_plan_vector.rs, which also asserts this
// file still contains them) and in starknet.js (conformance/verify.mjs).
const VECTOR_CHAIN_ID: felt252 = 0x534e5f5345504f4c4941; // 'SN_SEPOLIA'
const VECTOR_POOL: felt252 = 0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e;
const VECTOR_OWNER: felt252 = 0x0712d1ab5b6e5d3e3c0b2a1f9e8d7c6b5a4938271605f4e3d2c1b0a998877665;
const VECTOR_ACTIONS_HASH: felt252 =
    0x3342f4106e584ae6fa004f9a518687fa0160dfd61720b8aae0edf25fe89f84a;
const VECTOR_TYPE_HASH: felt252 = 0x35ab6743c4f282adfa4275ad5c9b3f7e15e33def33bdadc084b94eb369b8b66;
const VECTOR_PLAN_HASH: felt252 = 0x62a76d34a73ac201527a84ddcf018608bf9443c3daf8597c539304ff5ce3301;
// `client_actions_felts`: opaque felts standing in for a serialized Span<ClientAction>
// (`compile_actions` calldata[2..]). The contract never decodes them.
fn vector_client_actions_felts() -> Array<felt252> {
    array![0x2, 0x7, 0x2a, 0x3, 0x1]
}

// Test-only value standing in for the owner's STRK20 viewing key in calldata[1].
const TEST_USER_SK: felt252 = 0x1234;
const ADMIN: felt252 = 'ADMIN';
const STRANGER: felt252 = 'STRANGER';
const SN_MAIN: felt252 = 0x534e5f4d41494e;

fn addr(v: felt252) -> ContractAddress {
    v.try_into().unwrap()
}

#[derive(Copy, Drop)]
struct Setup {
    owner: ContractAddress,
    approvals: IStrk20PlanApprovalsDispatcher,
    custom: ICustomSignatureValidationDispatcher,
}

/// Deploys the owner at the vector's fixed address, on the vector's chain.
fn setup() -> Setup {
    let class = declare("Strk20OwnerExample").unwrap().contract_class();
    let calldata = array![ADMIN, VECTOR_POOL];
    let (owner, _) = class.deploy_at(@calldata, addr(VECTOR_OWNER)).unwrap();
    start_cheat_chain_id(owner, VECTOR_CHAIN_ID);
    Setup {
        owner,
        approvals: IStrk20PlanApprovalsDispatcher { contract_address: owner },
        custom: ICustomSignatureValidationDispatcher { contract_address: owner },
    }
}

fn approve(s: @Setup, plan_hash: felt252) {
    start_cheat_caller_address(*s.owner, addr(ADMIN));
    (*s.approvals).approve_plan(plan_hash);
    stop_cheat_caller_address(*s.owner);
}

fn compile_actions_call(to: felt252, selector: felt252, owner: felt252, user_sk: felt252) -> Call {
    let mut calldata = array![owner, user_sk];
    for f in vector_client_actions_felts() {
        calldata.append(f);
    }
    Call { to: addr(to), selector, calldata: calldata.span() }
}

fn good_call(user_sk: felt252) -> Call {
    compile_actions_call(VECTOR_POOL, selector!("compile_actions"), VECTOR_OWNER, user_sk)
}

fn verdict(s: @Setup, calls: Array<Call>) -> felt252 {
    (*s.custom).is_custom_signature_valid(calls.span(), array![].span(), array![].span())
}

// --- The vector ----------------------------------------------------------------------------

#[test]
fn vector_type_hash() {
    assert_eq!(STRK20_PLAN_TYPE_HASH, VECTOR_TYPE_HASH);
}

#[test]
fn vector_actions_hash() {
    assert_eq!(poseidon_hash_span(vector_client_actions_felts().span()), VECTOR_ACTIONS_HASH);
}

#[test]
fn vector_plan_hash_free_function() {
    let h = strk20_plan_hash(
        VECTOR_CHAIN_ID, addr(VECTOR_POOL), addr(VECTOR_OWNER), VECTOR_ACTIONS_HASH,
    );
    assert_eq!(h, VECTOR_PLAN_HASH);
}

#[test]
fn vector_plan_hash_through_contract() {
    // OZ OffchainMessageHash with the chain id from tx info (cheated to SN_SEPOLIA).
    let s = setup();
    assert_eq!(s.approvals.compute_plan_hash(VECTOR_ACTIONS_HASH), VECTOR_PLAN_HASH);
}

#[test]
fn plan_hash_depends_on_chain() {
    let s = setup();
    start_cheat_chain_id(s.owner, SN_MAIN);
    assert_ne!(s.approvals.compute_plan_hash(VECTOR_ACTIONS_HASH), VECTOR_PLAN_HASH);
}

// --- is_custom_signature_valid -------------------------------------------------------------

#[test]
fn approved_plan_validates() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    assert_eq!(verdict(@s, array![good_call(TEST_USER_SK)]), VALIDATED);
}

#[test]
fn approval_does_not_bind_user_sk() {
    // The plan hash excludes calldata[1], so the same approval holds whatever key is there.
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    assert_eq!(verdict(@s, array![good_call(0x5678)]), VALIDATED);
}

#[test]
fn signature_and_additional_data_are_ignored() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    let v = s
        .custom
        .is_custom_signature_valid(
            array![good_call(TEST_USER_SK)].span(), array![1, 2].span(), array![3, 4, 5].span(),
        );
    assert_eq!(v, VALIDATED);
}

#[test]
fn unapproved_plan_is_rejected() {
    let s = setup();
    assert_eq!(verdict(@s, array![good_call(TEST_USER_SK)]), 0);
}

#[test]
fn revoked_plan_is_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    start_cheat_caller_address(s.owner, addr(ADMIN));
    s.approvals.revoke_plan(VECTOR_PLAN_HASH);
    stop_cheat_caller_address(s.owner);
    assert_eq!(verdict(@s, array![good_call(TEST_USER_SK)]), 0);
}

#[test]
fn approval_on_another_chain_is_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    start_cheat_chain_id(s.owner, SN_MAIN);
    assert_eq!(verdict(@s, array![good_call(TEST_USER_SK)]), 0);
}

#[test]
fn tampered_actions_are_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    let mut calldata = array![VECTOR_OWNER, TEST_USER_SK];
    for f in vector_client_actions_felts() {
        calldata.append(f);
    }
    calldata.append(0x99);
    let call = Call {
        to: addr(VECTOR_POOL), selector: selector!("compile_actions"), calldata: calldata.span(),
    };
    assert_eq!(verdict(@s, array![call]), 0);
}

#[test]
fn wrong_pool_is_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    let call = compile_actions_call(
        'OTHER_POOL', selector!("compile_actions"), VECTOR_OWNER, TEST_USER_SK,
    );
    assert_eq!(verdict(@s, array![call]), 0);
}

#[test]
fn wrong_selector_is_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    let call = compile_actions_call(VECTOR_POOL, selector!("apply_actions"), VECTOR_OWNER, 0);
    assert_eq!(verdict(@s, array![call]), 0);
}

#[test]
fn wrong_owner_in_calldata_is_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    let call = compile_actions_call(
        VECTOR_POOL, selector!("compile_actions"), 'SOMEONE_ELSE', TEST_USER_SK,
    );
    assert_eq!(verdict(@s, array![call]), 0);
}

#[test]
fn multiple_calls_are_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    assert_eq!(verdict(@s, array![good_call(TEST_USER_SK), good_call(TEST_USER_SK)]), 0);
}

#[test]
fn no_calls_are_rejected() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    assert_eq!(verdict(@s, array![]), 0);
}

#[test]
fn short_calldata_is_rejected_without_panicking() {
    let s = setup();
    let call = Call {
        to: addr(VECTOR_POOL),
        selector: selector!("compile_actions"),
        calldata: array![VECTOR_OWNER, TEST_USER_SK].span(),
    };
    assert_eq!(verdict(@s, array![call]), 0);
}

// --- Strict fallback -----------------------------------------------------------------------

#[test]
fn is_valid_signature_is_always_zero() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    let account = IAccountDispatcher { contract_address: s.owner };
    assert_eq!(account.is_valid_signature(VECTOR_PLAN_HASH, array![]), 0);
    assert_eq!(account.is_valid_signature(VECTOR_PLAN_HASH, array![1, 2]), 0);
    assert_eq!(account.is_valid_signature(0, array![]), 0);
}

// --- SRC5 ----------------------------------------------------------------------------------

#[test]
fn src5_advertises_custom_validation_and_itself() {
    let s = setup();
    let src5 = ISRC5Dispatcher { contract_address: s.owner };
    assert_eq!(
        ICUSTOM_SIGNATURE_VALIDATION_ID,
        selector!("is_custom_signature_valid"),
        "id must equal the pool's",
    );
    assert!(src5.supports_interface(ICUSTOM_SIGNATURE_VALIDATION_ID));
    assert!(src5.supports_interface(ISRC5_ID));
    // Not an account: SRC6 is not advertised.
    assert!(
        !src5.supports_interface(0x2ceccef7f994940b3962a6c67e0ba4fcd37df7d131417c604f91e03caecc1cd),
    );
}

// --- Approvals -----------------------------------------------------------------------------

#[test]
fn admin_approves_and_revokes_with_events() {
    let s = setup();
    let mut spy = spy_events();
    approve(@s, VECTOR_PLAN_HASH);
    assert!(s.approvals.is_plan_approved(VECTOR_PLAN_HASH));
    start_cheat_caller_address(s.owner, addr(ADMIN));
    s.approvals.revoke_plan(VECTOR_PLAN_HASH);
    stop_cheat_caller_address(s.owner);
    assert!(!s.approvals.is_plan_approved(VECTOR_PLAN_HASH));
    spy
        .assert_emitted(
            @array![
                (
                    s.owner,
                    Event::PlanApproved(
                        PlanApproved { plan_hash: VECTOR_PLAN_HASH, by: addr(ADMIN) },
                    ),
                ),
                (
                    s.owner,
                    Event::PlanRevoked(
                        PlanRevoked { plan_hash: VECTOR_PLAN_HASH, by: addr(ADMIN) },
                    ),
                ),
            ],
        );
}

#[test]
#[should_panic(expected: 'Strk20Owner: not admin')]
fn stranger_cannot_approve() {
    let s = setup();
    start_cheat_caller_address(s.owner, addr(STRANGER));
    s.approvals.approve_plan(VECTOR_PLAN_HASH);
}

#[test]
#[should_panic(expected: 'Strk20Owner: not admin')]
fn stranger_cannot_revoke() {
    let s = setup();
    approve(@s, VECTOR_PLAN_HASH);
    start_cheat_caller_address(s.owner, addr(STRANGER));
    s.approvals.revoke_plan(VECTOR_PLAN_HASH);
}

#[test]
#[should_panic(expected: 'Strk20Owner: zero plan hash')]
fn zero_hash_cannot_be_approved() {
    let s = setup();
    approve(@s, 0);
}

#[test]
fn views_report_constructor_args() {
    let s = setup();
    assert_eq!(s.approvals.pool(), addr(VECTOR_POOL));
    assert_eq!(s.approvals.admin(), addr(ADMIN));
}

#[test]
fn constructor_rejects_zero_admin_or_pool() {
    let class = declare("Strk20OwnerExample").unwrap().contract_class();
    assert!(class.deploy(@array![0, VECTOR_POOL]).is_err());
    assert!(class.deploy(@array![ADMIN, 0]).is_err());
}
