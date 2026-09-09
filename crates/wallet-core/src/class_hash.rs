//! Sierra (Cairo 1) contract-class hashing — the `class_hash` a DECLARE v3
//! commits to.
//!
//! A node never takes the caller's word for `class_hash`. It recomputes the
//! hash from the `contract_class` object in the broadcast, and *that* value
//! goes into the transaction hash the account's `__validate_declare__` checks
//! the signature against. A wallet that signs a caller-supplied `class_hash`
//! which does not match the class actually broadcast therefore produces a
//! signature the node rejects as invalid (strkd #9). Deriving the hash here,
//! from the exact object we broadcast, closes that gap.
//!
//! Algorithm (`cairo-lang-starknet-classes` `ContractClass::hash`, matched by
//! starknet.js `computeSierraContractClassHash`):
//! ```text
//! class_hash = Poseidon(
//!     "CONTRACT_CLASS_V0.1.0",
//!     Poseidon(external    entry points as [selector, function_idx]*),
//!     Poseidon(l1_handler  entry points as [selector, function_idx]*),
//!     Poseidon(constructor entry points as [selector, function_idx]*),
//!     starknet_keccak(abi_string_bytes),
//!     Poseidon(sierra_program),
//! )
//! ```
//! The ABI is hashed as the **exact string** carried in the RPC object — byte
//! for byte, whitespace included — so the same ABI serialised two ways is two
//! different classes. We therefore never re-serialise a string ABI. When a
//! caller hands us the ABI as a JSON array (scarb's `*.contract_class.json`) we
//! serialise it the way the compiler hashes it: Python `json.dumps` default
//! separators (`", "` / `": "`, no newlines; `PythonJsonFormatter` in
//! cairo-lang-starknet-classes, `formatSpaces` in starknet.js), key order
//! preserved. That reproduces the class hash scarb/sncast/starknet.js print,
//! and is the form live Sepolia classes carry.

use std::io;

use serde::Serialize;
use serde_json::ser::Formatter;
use serde_json::{json, Value};
use starknet_types_core::felt::Felt;
use starknet_types_core::hash::{Poseidon, StarkHash};

use crate::error::{CoreError, Result};
use crate::tx::starknet_keccak;

/// The only Sierra class format version in existence; also the hash prefix.
pub const CONTRACT_CLASS_VERSION: &str = "0.1.0";
const CLASS_HASH_PREFIX: &[u8] = b"CONTRACT_CLASS_V0.1.0";

/// One Sierra entry point: `selector` → index of the function in the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SierraEntryPoint {
    pub selector: Felt,
    pub function_idx: u64,
}

/// Entry points grouped as in the RPC `entry_points_by_type` object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SierraEntryPoints {
    pub external: Vec<SierraEntryPoint>,
    pub l1_handler: Vec<SierraEntryPoint>,
    pub constructor: Vec<SierraEntryPoint>,
}

/// A parsed Sierra contract class in the shape the RPC `CONTRACT_CLASS` object
/// carries — i.e. exactly what a node hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SierraClass {
    /// The (uncompressed) Sierra program as field elements.
    pub sierra_program: Vec<Felt>,
    pub contract_class_version: String,
    pub entry_points_by_type: SierraEntryPoints,
    /// The ABI as the JSON **string** that is hashed and broadcast.
    pub abi: String,
}

impl SierraClass {
    /// Parse a class from JSON. Accepts both the RPC `CONTRACT_CLASS` object and
    /// scarb's `*.contract_class.json` artifact (which carries `abi` as a JSON
    /// array plus debug info the node does not want):
    ///
    /// - `sierra_program`: array of felts (hex or decimal strings, or integers).
    ///   The compressed gateway form (a base64 string) is rejected.
    /// - `contract_class_version`: must be `0.1.0` (defaults to it when absent).
    /// - `entry_points_by_type`: `EXTERNAL` / `L1_HANDLER` / `CONSTRUCTOR`
    ///   arrays of `{ selector, function_idx }`; missing groups are empty.
    /// - `abi`: the ABI as a JSON **string** (kept byte-for-byte: that is what
    ///   the node hashes) **or** as the ABI array/object, which is serialised
    ///   the way the compiler hashes it (Python-style separators, key order
    ///   preserved) — see the module docs.
    ///
    /// Unknown fields (e.g. `sierra_program_debug_info`) are ignored.
    pub fn from_json(v: &Value) -> Result<Self> {
        let obj = v
            .as_object()
            .ok_or_else(|| invalid("must be a JSON object (RPC CONTRACT_CLASS)"))?;

        let sierra_program = match obj.get("sierra_program") {
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    felt_of(f).ok_or_else(|| invalid(&format!("sierra_program[{i}] is not a felt")))
                })
                .collect::<Result<Vec<_>>>()?,
            Some(Value::String(_)) => return Err(invalid(
                "sierra_program must be the uncompressed felt array (RPC CONTRACT_CLASS form), \
                     not the compressed gateway string",
            )),
            _ => return Err(invalid("missing 'sierra_program' (array of felts)")),
        };
        if sierra_program.is_empty() {
            return Err(invalid("sierra_program is empty"));
        }

        let contract_class_version = match obj.get("contract_class_version") {
            None | Some(Value::Null) => CONTRACT_CLASS_VERSION.to_string(),
            Some(Value::String(s)) if s == CONTRACT_CLASS_VERSION => s.clone(),
            Some(other) => {
                return Err(invalid(&format!(
                    "unsupported contract_class_version {other} (expected \"{CONTRACT_CLASS_VERSION}\")"
                )))
            }
        };

        let entry_points_by_type = match obj.get("entry_points_by_type") {
            None | Some(Value::Null) => SierraEntryPoints::default(),
            Some(Value::Object(groups)) => SierraEntryPoints {
                external: entry_points(groups.get("EXTERNAL"), "EXTERNAL")?,
                l1_handler: entry_points(groups.get("L1_HANDLER"), "L1_HANDLER")?,
                constructor: entry_points(groups.get("CONSTRUCTOR"), "CONSTRUCTOR")?,
            },
            Some(_) => return Err(invalid("entry_points_by_type must be an object")),
        };

        let abi = match obj.get("abi") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => {
                return Err(invalid(
                    "missing 'abi' (the ABI as a JSON string, or the ABI array from the compiled class)",
                ))
            }
            // Array/object straight from a compiled artifact: the compiler's form.
            Some(other) => python_json(other)?,
        };

        Ok(SierraClass {
            sierra_program,
            contract_class_version,
            entry_points_by_type,
            abi,
        })
    }

    /// The class hash a node derives from this class (see module docs).
    pub fn class_hash(&self) -> Felt {
        let eps = &self.entry_points_by_type;
        Poseidon::hash_array(&[
            Felt::from_bytes_be_slice(CLASS_HASH_PREFIX),
            hash_entry_points(&eps.external),
            hash_entry_points(&eps.l1_handler),
            hash_entry_points(&eps.constructor),
            starknet_keccak(self.abi.as_bytes()),
            Poseidon::hash_array(&self.sierra_program),
        ])
    }

    /// The canonical RPC `CONTRACT_CLASS` object for `starknet_addDeclareTransaction`
    /// / `starknet_estimateFee`: exactly the four spec fields, felts as hex, ABI as
    /// the string [`Self::class_hash`] hashed.
    pub fn to_rpc_json(&self) -> Value {
        let ep = |eps: &[SierraEntryPoint]| -> Vec<Value> {
            eps.iter()
                .map(|e| json!({ "selector": hex(&e.selector), "function_idx": e.function_idx }))
                .collect()
        };
        json!({
            "sierra_program": self.sierra_program.iter().map(hex).collect::<Vec<_>>(),
            "contract_class_version": self.contract_class_version,
            "entry_points_by_type": {
                "EXTERNAL": ep(&self.entry_points_by_type.external),
                "L1_HANDLER": ep(&self.entry_points_by_type.l1_handler),
                "CONSTRUCTOR": ep(&self.entry_points_by_type.constructor),
            },
            "abi": self.abi,
        })
    }
}

/// serde_json formatter reproducing Python's `json.dumps` default separators
/// (`", "` between items, `": "` after keys, no newlines): the form the Cairo
/// compiler serialises the ABI in before hashing it. ABIs are ASCII (Cairo
/// identifiers), so Python's `\uXXXX` escaping of non-ASCII never comes up.
struct PythonJsonFormatter;

impl Formatter for PythonJsonFormatter {
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
}

/// Serialise a JSON value the way the Cairo compiler serialises an ABI for
/// hashing (see [`PythonJsonFormatter`]). Key order is preserved (serde_json
/// `preserve_order`), as it must be for the bytes to match.
pub fn python_json(v: &Value) -> Result<String> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PythonJsonFormatter);
    v.serialize(&mut ser)
        .map_err(|_| CoreError::Serialization)?;
    String::from_utf8(out).map_err(|_| CoreError::Serialization)
}

fn hash_entry_points(eps: &[SierraEntryPoint]) -> Felt {
    let flat: Vec<Felt> = eps
        .iter()
        .flat_map(|e| [e.selector, Felt::from(e.function_idx)])
        .collect();
    Poseidon::hash_array(&flat)
}

fn entry_points(group: Option<&Value>, name: &str) -> Result<Vec<SierraEntryPoint>> {
    match group {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let selector = e
                    .get("selector")
                    .and_then(felt_of)
                    .ok_or_else(|| invalid(&format!("{name}[{i}].selector is not a felt")))?;
                let function_idx = match e.get("function_idx") {
                    Some(Value::Number(n)) => n.as_u64(),
                    Some(Value::String(s)) => s.parse::<u64>().ok(),
                    _ => None,
                }
                .ok_or_else(|| invalid(&format!("{name}[{i}].function_idx is not an integer")))?;
                Ok(SierraEntryPoint {
                    selector,
                    function_idx,
                })
            })
            .collect(),
        Some(_) => Err(invalid(&format!(
            "entry_points_by_type.{name} must be an array"
        ))),
    }
}

/// A felt from a JSON value: `"0x…"` hex, decimal string, or integer.
fn felt_of(v: &Value) -> Option<Felt> {
    match v {
        Value::String(s) => {
            if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                Felt::from_hex(&format!("0x{h}")).ok()
            } else {
                Felt::from_dec_str(s).ok()
            }
        }
        Value::Number(n) => n.as_u64().map(Felt::from),
        _ => None,
    }
}

fn hex(f: &Felt) -> String {
    format!("0x{f:x}")
}

fn invalid(msg: &str) -> CoreError {
    CoreError::InvalidContractClass(msg.to_string())
}
