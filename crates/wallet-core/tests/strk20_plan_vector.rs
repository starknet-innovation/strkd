//! SNIP-12 `Strk20Plan` approval-hash vector (issue #48, design note §5.3).
//!
//! `fixtures/strk20-plan-vector.json` is the single source of truth. Three
//! independent implementations must agree on it:
//! - Rust / krusty-kms: this file, through the production `typed_data_message_hash`;
//! - starknet.js: `conformance/verify.mjs`;
//! - Cairo / OpenZeppelin SNIP-12: `contracts/strk20-owner-example/tests/test_owner.cairo`.
//!
//! The Cairo test cannot read JSON, so it carries the constants itself; the last
//! test here fails if that file and the fixture drift apart.
//!
//! Public, made-up values only: no keys.

use serde_json::Value;
use starknet_types_core::hash::{Poseidon, StarkHash};
use wallet_core::{address_hex, typed_data_message_hash, Felt};

const VECTOR: &str = include_str!("fixtures/strk20-plan-vector.json");
const CAIRO_TEST: &str =
    include_str!("../../../contracts/strk20-owner-example/tests/test_owner.cairo");

fn doc() -> Value {
    serde_json::from_str(VECTOR).expect("the Strk20Plan vector must be valid JSON")
}

fn felt(v: &Value) -> Felt {
    Felt::from_hex(v.as_str().expect("hex string")).expect("valid felt")
}

/// `actions_hash = poseidon_hash_many(client_actions_felts)`, i.e. Cairo's
/// `poseidon_hash_span(compile_actions calldata[2..])`.
#[test]
fn actions_hash_is_poseidon_of_the_client_action_felts() {
    let d = doc();
    let felts: Vec<Felt> =
        d["inputs"]["client_actions_felts"].as_array().unwrap().iter().map(felt).collect();
    assert_eq!(Poseidon::hash_array(&felts), felt(&d["actions_hash"]));
    assert_eq!(felt(&d["typed_data"]["message"]["actions_hash"]), felt(&d["actions_hash"]));
}

#[test]
fn the_message_binds_the_vector_inputs() {
    let d = doc();
    let msg = &d["typed_data"]["message"];
    assert_eq!(felt(&msg["pool"]), felt(&d["inputs"]["pool"]));
    assert_eq!(felt(&msg["owner"]), felt(&d["inputs"]["owner"]));
    assert_eq!(felt(&d["account_address"]), felt(&d["inputs"]["owner"]), "signer = owner");
    assert_eq!(
        Felt::from_bytes_be_slice(d["inputs"]["chain_id"].as_str().unwrap().as_bytes()),
        felt(&d["inputs"]["chain_id_felt"]),
    );
}

#[test]
fn plan_hash_reproduces() {
    let d = doc();
    let h = typed_data_message_hash(&d["typed_data"].to_string(), &felt(&d["account_address"]))
        .unwrap();
    assert_eq!(address_hex(&h), d["plan_hash"].as_str().unwrap(), "Strk20Plan digest drifted");
}

/// `"version": "1"` is the felt 1, not the shortstring 0x31 (same rule as
/// `"revision": "1"`). The Cairo contract hard-codes 1; if this encoding ever
/// changed, approvals would silently stop matching.
#[test]
fn domain_version_one_is_the_number_one() {
    let d = doc();
    let mut as_short = d["typed_data"].clone();
    as_short["domain"]["version"] = Value::from("0x31");
    let mut as_num = d["typed_data"].clone();
    as_num["domain"]["version"] = Value::from("0x1");
    let owner = felt(&d["account_address"]);
    let want = felt(&d["plan_hash"]);
    assert_eq!(typed_data_message_hash(&as_num.to_string(), &owner).unwrap(), want);
    assert_ne!(typed_data_message_hash(&as_short.to_string(), &owner).unwrap(), want);
}

/// The Cairo test file must carry exactly these constants.
#[test]
fn the_cairo_test_carries_the_same_constants() {
    let d = doc();
    let expect = [
        ("VECTOR_CHAIN_ID", felt(&d["inputs"]["chain_id_felt"])),
        ("VECTOR_POOL", felt(&d["inputs"]["pool"])),
        ("VECTOR_OWNER", felt(&d["inputs"]["owner"])),
        ("VECTOR_ACTIONS_HASH", felt(&d["actions_hash"])),
        ("VECTOR_TYPE_HASH", felt(&d["type_hash"])),
        ("VECTOR_PLAN_HASH", felt(&d["plan_hash"])),
    ];
    for (name, want) in expect {
        let decl = format!("const {name}: felt252 =");
        let rest = CAIRO_TEST
            .split_once(&decl)
            .unwrap_or_else(|| panic!("{name} missing from test_owner.cairo"))
            .1;
        let value = rest.split(';').next().unwrap().trim();
        assert_eq!(Felt::from_hex(value).unwrap(), want, "{name} in test_owner.cairo drifted");
    }
    let felts: Vec<String> = d["inputs"]["client_actions_felts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let arr = format!("array![{}]", felts.join(", "));
    assert!(CAIRO_TEST.contains(&arr), "client_actions_felts in test_owner.cairo drifted");
}
