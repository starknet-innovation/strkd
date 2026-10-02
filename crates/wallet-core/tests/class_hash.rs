//! Sierra class-hash derivation (strkd #9): our hash must equal what a node
//! derives from the broadcast `contract_class`, or the DECLARE signature fails
//! on-chain validation. Fixtures are public artifacts; no key material.

use serde_json::{json, Value};
use wallet_core::class_hash::python_json;
use wallet_core::{Felt, SierraClass};

/// A minimal `Counter` contract compiled with scarb 2.18.0 / cairo 2.18.0
/// (`*.contract_class.json`: ABI as a JSON array, plus debug info). Reference
/// hashes computed independently with starknet.js 10.0.2:
/// `hash.computeSierraContractClassHash` / `hash.computeCompiledClassHash`.
const MINIMAL_COUNTER: &str = include_str!("fixtures/minimal_counter.contract_class.json");
const MINIMAL_COUNTER_CLASS_HASH: &str =
    "0x50b7a36b2af957551f6b829a690666c0f212a324dbc8ccff325e38a1e0843a5";

/// A class **live on Sepolia**, fetched verbatim with `starknet_getClass`
/// (2026-09-09, RPC v0.10): the Braavos base-account class. RPC form — ABI is a
/// string, in the compiler's Python-style `", "` / `": "` form. Pins that string
/// ABIs are hashed byte-for-byte (a compact re-serialisation is another class).
const SEPOLIA_CLASS: &str = include_str!("fixtures/sepolia_class_0x03d16c7a.json");
const SEPOLIA_CLASS_HASH: &str =
    "0x03d16c7a9a60b0593bd202f660a28c5d76e0403601d9ccc7e4fa253b6a70c201";

fn felt(h: &str) -> Felt {
    Felt::from_hex(h).unwrap()
}

#[test]
fn scarb_artifact_hashes_like_starknet_js() {
    let v: Value = serde_json::from_str(MINIMAL_COUNTER).unwrap();
    assert!(v["abi"].is_array(), "fixture carries the ABI as an array");
    let class = SierraClass::from_json(&v).unwrap();
    assert_eq!(class.class_hash(), felt(MINIMAL_COUNTER_CLASS_HASH));
    assert_eq!(class.sierra_program.len(), 222);
    assert_eq!(class.entry_points_by_type.external.len(), 2);
    assert_eq!(class.entry_points_by_type.constructor.len(), 1);
    assert!(class.entry_points_by_type.l1_handler.is_empty());
}

#[test]
fn live_sepolia_class_hashes_to_its_onchain_class_hash() {
    let v: Value = serde_json::from_str(SEPOLIA_CLASS).unwrap();
    let abi = v["abi"].as_str().expect("RPC form: abi is a string");
    assert!(
        abi.contains("\", \""),
        "fixture ABI is deliberately non-compact"
    );
    let class = SierraClass::from_json(&v).unwrap();
    assert_eq!(class.class_hash(), felt(SEPOLIA_CLASS_HASH));
    // Re-serialising that ABI compactly would be a *different* class.
    let compact = serde_json::to_string(&serde_json::from_str::<Value>(abi).unwrap()).unwrap();
    let mut alt = v.clone();
    alt["abi"] = json!(compact);
    assert_ne!(
        SierraClass::from_json(&alt).unwrap().class_hash(),
        felt(SEPOLIA_CLASS_HASH)
    );
}

#[test]
fn abi_array_serialises_the_way_the_compiler_hashes_it() {
    let v: Value = serde_json::from_str(MINIMAL_COUNTER).unwrap();
    let class = SierraClass::from_json(&v).unwrap();
    // Python json.dumps default separators, key order preserved.
    assert!(
        class
            .abi
            .starts_with("[{\"type\": \"impl\", \"name\": \"CounterImpl\", "),
        "{}",
        class.abi
    );
    assert!(!class.abi.contains('\n'));
    // The same ABI handed over as that exact string is the same class.
    let mut as_string = v.clone();
    as_string["abi"] = json!(class.abi.clone());
    assert_eq!(SierraClass::from_json(&as_string).unwrap(), class);
    // Handed over compact or pretty-printed, it is hashed verbatim → a
    // different class hash (which the wallet reports as a mismatch instead of
    // signing a declare the node would reject).
    for other in [
        serde_json::to_string(&v["abi"]).unwrap(),
        serde_json::to_string_pretty(&v["abi"]).unwrap(),
    ] {
        let mut alt = v.clone();
        alt["abi"] = json!(other);
        assert_ne!(
            SierraClass::from_json(&alt).unwrap().class_hash(),
            felt(MINIMAL_COUNTER_CLASS_HASH)
        );
    }
}

#[test]
fn python_json_uses_python_default_separators() {
    let v = json!({"b": [1, "x", {"c": null}], "a": true, "d": {}, "e": []});
    assert_eq!(
        python_json(&v).unwrap(),
        r#"{"b": [1, "x", {"c": null}], "a": true, "d": {}, "e": []}"#
    );
}

#[test]
fn to_rpc_json_is_the_canonical_contract_class_object() {
    let v: Value = serde_json::from_str(MINIMAL_COUNTER).unwrap();
    assert!(
        v.get("sierra_program_debug_info").is_some(),
        "scarb artifact carries debug info"
    );
    let class = SierraClass::from_json(&v).unwrap();
    let rpc = class.to_rpc_json();
    let keys: Vec<&str> = rpc
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "sierra_program",
            "contract_class_version",
            "entry_points_by_type",
            "abi"
        ]
    );
    assert!(rpc["abi"].is_string());
    assert_eq!(rpc["contract_class_version"], json!("0.1.0"));
    assert_eq!(rpc["entry_points_by_type"]["L1_HANDLER"], json!([]));
    assert_eq!(
        rpc["entry_points_by_type"]["CONSTRUCTOR"][0]["function_idx"],
        json!(2)
    );
    // Round-trips to the same class (and hash).
    let again = SierraClass::from_json(&rpc).unwrap();
    assert_eq!(again, class);
}

#[test]
fn accepts_decimal_and_integer_felts_and_missing_entry_point_groups() {
    let v = json!({
        "sierra_program": ["0x1", "2", 3],
        "entry_points_by_type": { "EXTERNAL": [{ "selector": "0xab", "function_idx": "0" }] },
        "abi": "[]"
    });
    let class = SierraClass::from_json(&v).unwrap();
    assert_eq!(
        class.sierra_program,
        vec![Felt::from(1u64), Felt::from(2u64), Felt::from(3u64)]
    );
    assert_eq!(class.contract_class_version, "0.1.0");
    assert_eq!(class.entry_points_by_type.external[0].function_idx, 0);
    assert!(class.entry_points_by_type.constructor.is_empty());
}

#[test]
fn rejects_malformed_classes_with_a_reason() {
    let err = |v: Value| SierraClass::from_json(&v).unwrap_err().to_string();
    assert!(err(json!({ "abi": "[]" })).contains("sierra_program"));
    assert!(err(json!({ "sierra_program": "H4sIAAAA", "abi": "[]" })).contains("compressed"));
    assert!(err(json!({ "sierra_program": ["0x1"] })).contains("abi"));
    assert!(err(json!({ "sierra_program": ["zz"], "abi": "[]" })).contains("sierra_program[0]"));
    assert!(err(
        json!({ "sierra_program": ["0x1"], "abi": "[]", "contract_class_version": "0.2.0" })
    )
    .contains("contract_class_version"));
    assert!(err(json!({
        "sierra_program": ["0x1"], "abi": "[]",
        "entry_points_by_type": { "EXTERNAL": [{ "selector": "0x1" }] }
    }))
    .contains("function_idx"));
    assert!(err(json!("not an object")).contains("object"));
}
