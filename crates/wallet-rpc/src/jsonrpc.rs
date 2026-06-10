//! JSON-RPC 2.0 request/response types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An incoming JSON-RPC request. `params` defaults to null when absent.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub id: Value,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Serialize)]
pub struct RpcErrorObject {
    pub code: i64,
    pub message: String,
}

/// A JSON-RPC response (exactly one of `result` / `error` is set).
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorObject>,
    pub id: Value,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn err(id: Value, error: RpcErrorObject) -> Self {
        Response {
            jsonrpc: "2.0",
            result: None,
            error: Some(error),
            id,
        }
    }
}
