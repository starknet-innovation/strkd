//! `proof_payload_diff` — diagnose why a Starknet node rejects strkd's SNIP-36
//! proof-carrying broadcast (`error 69: "The proof field in the invoke v3
//! transaction is invalid"`).
//!
//! It does three things:
//!   1. Emits the EXACT `invoke_transaction` object strkd POSTs for a
//!      proof-carrying `verify_move_forward` invoke — via the real
//!      `wallet_rpc::invoke_v3_tx_json`, so this never drifts from production.
//!   2. Checks that payload against the JSON-RPC spec shape for
//!      `BROADCASTED_INVOKE_TXN` (RPC 0.10.1): field presence/types, the
//!      `resource_bounds` sub-structure, `proof` as a base64 string sibling, and
//!      `proof_facts` as a FELT array (pattern `^0x(0|[a-fA-F1-9][a-fA-F0-9]{0,62})$`
//!      — i.e. NO leading zeros / zero-padding).
//!   3. When given a reference payload, diffs strkd's payload against it so any
//!      field/format/nesting delta is obvious. The reference is a known-good
//!      broadcast body — see "capturing a reference" below.
//!
//! Run:
//!   cargo run -p wallet-rpc --example proof_payload_diff                 # payload + spec checks
//!   cargo run -p wallet-rpc --example proof_payload_diff -- reference.json   # + diff
//!   cargo run -p wallet-rpc --example proof_payload_diff -- --emit > tx.json # raw payload only
//!
//! Capturing a reference (a payload the node is known to accept, or one from a
//! reference implementation), as the `invoke_transaction` object:
//!   • sncast: run the proof-carrying invoke with `--proof-file <file>` and log
//!     the JSON-RPC request body it sends to `starknet_addInvokeTransaction`.
//!   • starknet.js proof fork: the `INVOKE_TXN_V3` from
//!     `account.getSignedTransaction(call, { resourceBounds })` plus the `proof`.
//! The example accepts the bare invoke object, `{ "invoke_transaction": {…} }`,
//! or a full `{ "params": { "invoke_transaction": {…} } }` request and unwraps it.

use serde_json::Value;
use wallet_core::{Felt, ResourceBounds};
use wallet_rpc::{invoke_v3_tx_json, FeeBounds};

fn felt(s: &str) -> Felt {
    Felt::from_hex(s).expect("valid felt hex")
}

/// FELT per the spec regex `^0x(0|[a-fA-F1-9][a-fA-F0-9]{0,62})$`: a `0x` prefix
/// then either a lone `0` or a non-zero leading nibble plus up to 62 more hex
/// digits. No leading zeros, no zero-padding, 1..=63 significant nibbles.
fn is_spec_felt(s: &str) -> bool {
    let Some(hex) = s.strip_prefix("0x") else {
        return false;
    };
    if hex == "0" {
        return true;
    }
    if hex.is_empty() || hex.len() > 63 {
        return false;
    }
    let mut it = hex.chars();
    let first = it.next().unwrap();
    first.is_ascii_hexdigit() && first != '0' && it.all(|c| c.is_ascii_hexdigit())
}

/// Standard base64 (with optional trailing `=`), no whitespace. Mirrors the node
/// expectation for the `proof` string.
fn is_clean_std_base64(s: &str) -> bool {
    if s.chars().any(|c| c.is_ascii_whitespace()) {
        return false;
    }
    let b = s.as_bytes();
    let pad = b.iter().rev().take_while(|&&c| c == b'=').count();
    let body = &b[..b.len() - pad];
    !body.is_empty()
        && pad <= 2
        && body
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
}

#[derive(Default)]
struct Checks {
    lines: Vec<String>,
    fails: u32,
    warns: u32,
}

impl Checks {
    fn ok(&mut self, m: impl Into<String>) {
        self.lines.push(format!("  [ ok ] {}", m.into()));
    }
    fn fail(&mut self, m: impl Into<String>) {
        self.lines.push(format!("  [FAIL] {}", m.into()));
        self.fails += 1;
    }
    fn warn(&mut self, m: impl Into<String>) {
        self.lines.push(format!("  [warn] {}", m.into()));
        self.warns += 1;
    }
    fn require(&mut self, cond: bool, m: impl Into<String>) {
        if cond {
            self.ok(m);
        } else {
            self.fail(m);
        }
    }
}

fn ty(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn spec_checks(tx: &Value) -> Checks {
    let mut c = Checks::default();

    c.require(tx["type"] == "INVOKE", "type == \"INVOKE\"");
    c.require(tx["version"] == "0x3", "version == \"0x3\"");

    // Required V3 scalar/array fields.
    for (k, kind) in [
        ("sender_address", "string"),
        ("calldata", "array"),
        ("signature", "array"),
        ("nonce", "string"),
        ("resource_bounds", "object"),
        ("tip", "string"),
        ("paymaster_data", "array"),
        ("account_deployment_data", "array"),
        ("nonce_data_availability_mode", "string"),
        ("fee_data_availability_mode", "string"),
    ] {
        match tx.get(k) {
            Some(v) if ty(v) == kind => c.ok(format!("{k} present ({kind})")),
            Some(v) => c.fail(format!("{k} is {} (expected {kind})", ty(v))),
            None => c.fail(format!("{k} missing")),
        }
    }

    // resource_bounds sub-structure.
    if let Some(rb) = tx.get("resource_bounds") {
        for leg in ["l1_gas", "l2_gas", "l1_data_gas"] {
            match rb.get(leg) {
                Some(b) => {
                    let amt = b.get("max_amount").map(ty);
                    let ppu = b.get("max_price_per_unit").map(ty);
                    c.require(
                        amt == Some("string") && ppu == Some("string"),
                        format!("resource_bounds.{leg} has max_amount+max_price_per_unit (string)"),
                    );
                }
                None => c.fail(format!("resource_bounds.{leg} missing")),
            }
        }
    }

    // proof: optional, but if present must be a clean standard-base64 string.
    match tx.get("proof") {
        Some(Value::String(p)) => {
            c.ok("proof is a string (PROOF schema)");
            c.require(
                is_clean_std_base64(p),
                "proof is whitespace-free standard base64",
            );
        }
        Some(v) => c.fail(format!("proof is {} (spec PROOF = string)", ty(v))),
        None => c.warn("proof absent (ok for proof_facts-only signing; required to broadcast)"),
    }

    // proof_facts: array of spec FELTs.
    match tx.get("proof_facts") {
        Some(Value::Array(facts)) => {
            c.ok(format!("proof_facts is an array (len {})", facts.len()));
            let bad: Vec<String> = facts
                .iter()
                .enumerate()
                .filter_map(|(i, f)| match f.as_str() {
                    Some(s) if is_spec_felt(s) => None,
                    Some(s) => Some(format!("[{i}]={s}")),
                    None => Some(format!("[{i}] not a string")),
                })
                .collect();
            c.require(
                bad.is_empty(),
                if bad.is_empty() {
                    "all proof_facts match the FELT pattern".to_string()
                } else {
                    format!("proof_facts violate FELT pattern: {}", bad.join(", "))
                },
            );
        }
        Some(v) => c.fail(format!("proof_facts is {} (expected array)", ty(v))),
        None => c.warn("proof_facts absent (not a proof-carrying invoke)"),
    }

    // FELT-format the other felt-typed fields. sender_address is zero-padded by
    // strkd (0x{:064x}); flag it as a deviation from the strict FELT pattern —
    // most nodes accept padded addresses, but it is worth ruling out.
    if let Some(s) = tx["nonce"].as_str() {
        c.require(is_spec_felt(s), format!("nonce matches FELT pattern ({s})"));
    }
    if let Some(s) = tx["sender_address"].as_str() {
        if is_spec_felt(s) {
            c.ok("sender_address matches FELT pattern");
        } else {
            c.warn(format!(
                "sender_address {s} is zero-padded (strict FELT forbids leading zeros; \
                 usually tolerated for ADDRESS — rule out if the node is strict)"
            ));
        }
    }
    for (field, label) in [("calldata", "calldata"), ("signature", "signature")] {
        if let Some(arr) = tx[field].as_array() {
            let bad = arr
                .iter()
                .filter(|f| f.as_str().map(|s| !is_spec_felt(s)).unwrap_or(true))
                .count();
            c.require(bad == 0, format!("{label} elements match the FELT pattern"));
        }
    }

    c
}

/// Recursive structural + value diff. Structural deltas (missing keys, type
/// mismatches, array-length mismatches) are the ones that matter for error 69;
/// scalar value diffs are expected (it's a different tx) and bucketed separately.
fn diff(path: &str, a: &Value, b: &Value, structural: &mut Vec<String>, values: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(am), Value::Object(bm)) => {
            for k in am.keys() {
                if !bm.contains_key(k) {
                    structural.push(format!("- only in strkd:     {path}/{k}"));
                }
            }
            for k in bm.keys() {
                if !am.contains_key(k) {
                    structural.push(format!("+ only in reference: {path}/{k}"));
                }
            }
            for (k, av) in am {
                if let Some(bv) = bm.get(k) {
                    diff(&format!("{path}/{k}"), av, bv, structural, values);
                }
            }
        }
        (Value::Array(aa), Value::Array(ba)) => {
            if aa.len() != ba.len() {
                structural.push(format!(
                    "~ array length: {path} strkd={} reference={}",
                    aa.len(),
                    ba.len()
                ));
            }
            for (i, (x, y)) in aa.iter().zip(ba.iter()).enumerate() {
                diff(&format!("{path}/{i}"), x, y, structural, values);
            }
        }
        _ if ty(a) != ty(b) => {
            structural.push(format!(
                "~ type: {path} strkd={} reference={}",
                ty(a),
                ty(b)
            ));
        }
        _ if a != b => values.push(format!("~ {path}: strkd={a} reference={b}")),
        _ => {}
    }
}

fn unwrap_invoke(v: Value) -> Value {
    if let Some(it) = v.pointer("/params/invoke_transaction") {
        return it.clone();
    }
    if let Some(it) = v.get("invoke_transaction") {
        return it.clone();
    }
    v
}

fn main() {
    // A representative proof-carrying verify_move_forward(MoveMessage) invoke.
    // Felt VALUES are illustrative; their FORMATTING is strkd's real `fh`
    // (unpadded lowercase hex), so the spec checks reflect production output.
    let sender = felt("0x49d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7");
    let calldata: Vec<Felt> = ["0x1", "0x6d6f7665", "0x7665726966795f6d6f7665", "0x3", "0xa", "0xb", "0xc"]
        .iter()
        .map(|s| felt(s))
        .collect();
    let signature = vec![felt("0x1a2b3c"), felt("0x4d5e6f")];
    let nonce = felt("0x7");
    // SNIP-36 proof_facts, indices [0..=8]: [7]=n_messages, [8]=message hash.
    let proof_facts: Vec<Felt> = ["0x0", "0x0", "0x1234", "0x0", "0x64", "0xabc", "0xdef", "0x1", "0x9f8e7d"]
        .iter()
        .map(|s| felt(s))
        .collect();
    let proof = "AQIDBAUGBwgJCgsMDQ4PEA==";

    let bounds = FeeBounds {
        l1_gas: ResourceBounds { max_amount: 0xbd2a, max_price_per_unit: 0x5af3107a4000 },
        l2_gas: ResourceBounds { max_amount: 0x279fc0, max_price_per_unit: 0x174876e800 },
        l1_data_gas: ResourceBounds { max_amount: 0xc0, max_price_per_unit: 0x3b9aca00 },
    };

    let tx = invoke_v3_tx_json(&sender, &calldata, &signature, &nonce, &bounds, &proof_facts, Some(proof));

    let arg = std::env::args().nth(1);

    // --emit: raw payload only (for piping into a file to capture/compare).
    if arg.as_deref() == Some("--emit") {
        println!("{}", serde_json::to_string_pretty(&tx).unwrap());
        return;
    }

    println!("== strkd broadcast payload (params.invoke_transaction) ==");
    println!("{}\n", serde_json::to_string_pretty(&tx).unwrap());

    println!("== spec checks (BROADCASTED_INVOKE_TXN, RPC 0.10.1) ==");
    let c = spec_checks(&tx);
    for l in &c.lines {
        println!("{l}");
    }
    println!("  → {} ok, {} warn, {} FAIL\n", c.lines.len() as u32 - c.warns - c.fails, c.warns, c.fails);

    match arg {
        None => {
            println!("No reference supplied — pass a known-good payload to diff:");
            println!("  cargo run -p wallet-rpc --example proof_payload_diff -- reference.json");
            println!("(See the file header for how to capture one from sncast / the starknet.js proof fork.)");
        }
        Some(path) => match std::fs::read_to_string(&path) {
            Err(e) => eprintln!("could not read reference {path}: {e}"),
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Err(e) => eprintln!("reference {path} is not valid JSON: {e}"),
                Ok(v) => {
                    let reference = unwrap_invoke(v);
                    let (mut structural, mut values) = (Vec::new(), Vec::new());
                    diff("", &tx, &reference, &mut structural, &mut values);

                    println!("== structural differences vs {path} (these matter for error 69) ==");
                    if structural.is_empty() {
                        println!("  none — strkd's payload is structurally identical to the reference.");
                    } else {
                        for l in &structural {
                            println!("  {l}");
                        }
                    }
                    println!("\n== value differences (expected: different tx/proof) ==");
                    if values.is_empty() {
                        println!("  none.");
                    } else {
                        for l in &values {
                            println!("  {l}");
                        }
                    }
                }
            },
        },
    }
}
