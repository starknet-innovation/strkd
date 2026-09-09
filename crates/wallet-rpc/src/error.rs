//! Wallet RPC errors and their wire codes.
//!
//! Codes 111–163 are the standard `wallet_rpc.json` application codes (spec
//! §7.5). Negative codes in the JSON-RPC server-reserved range (-32000..-32099)
//! and the standard -32600/-32601/-32700 are used for transport/condition
//! errors the wallet spec does not enumerate: -32001 Locked, -32002 Forbidden,
//! -32003 TransportRejected, -32601 NotImplemented, -32700 Parse.

use crate::jsonrpc::RpcErrorObject;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletRpcError {
    /// 113 — user rejected the operation (or the approval timed out).
    UserRefused,
    /// 114 — request payload was invalid / could not be parsed.
    InvalidRequest(String),
    /// 116 — deployment data not available (e.g. no account in scope).
    DeploymentDataNotAvailable,
    /// 117 — requested chain id is not supported.
    ChainIdNotSupported,
    /// 118 — caller is not a registered/paired client.
    NotRegistered,
    /// 162 — requested API version is not supported.
    ApiVersionNotSupported,
    /// 163 — catch-all internal error.
    Unknown(String),
    /// -32001 — the wallet is locked and the operation needs the seed.
    Locked,
    /// -32002 — the caller is not permitted to act on this account/method.
    Forbidden,
    /// -32003 — rejected by transport hardening (CSRF / DNS-rebinding guard).
    TransportRejected(String),
    /// -32004 — Starknet node interaction failed (estimate / nonce / broadcast).
    Node(String),
    /// -32005 — an operation needed a node but none is configured.
    NoNode,
    /// -32006 — the request is well-formed but the on-chain state blocks it
    /// (e.g. the sender account isn't deployed, or is already deployed). Carries
    /// a human-actionable message naming the account and the fix.
    Precondition(String),
    /// -32601 — method is unknown or not implemented in this phase.
    NotImplemented(String),
    /// -32700 — request body was not valid JSON.
    Parse,
}

impl WalletRpcError {
    pub fn code(&self) -> i64 {
        match self {
            WalletRpcError::UserRefused => 113,
            WalletRpcError::InvalidRequest(_) => 114,
            WalletRpcError::DeploymentDataNotAvailable => 116,
            WalletRpcError::ChainIdNotSupported => 117,
            WalletRpcError::NotRegistered => 118,
            WalletRpcError::ApiVersionNotSupported => 162,
            WalletRpcError::Unknown(_) => 163,
            WalletRpcError::Locked => -32001,
            WalletRpcError::Forbidden => -32002,
            WalletRpcError::TransportRejected(_) => -32003,
            WalletRpcError::Node(_) => -32004,
            WalletRpcError::NoNode => -32005,
            WalletRpcError::Precondition(_) => -32006,
            WalletRpcError::NotImplemented(_) => -32601,
            WalletRpcError::Parse => -32700,
        }
    }

    pub fn message(&self) -> String {
        match self {
            WalletRpcError::UserRefused => "user refused the operation".into(),
            WalletRpcError::InvalidRequest(m) => format!("invalid request payload: {m}"),
            WalletRpcError::DeploymentDataNotAvailable => "deployment data not available".into(),
            WalletRpcError::ChainIdNotSupported => "chain id not supported".into(),
            WalletRpcError::NotRegistered => "caller is not a registered client".into(),
            WalletRpcError::ApiVersionNotSupported => "api version not supported".into(),
            WalletRpcError::Unknown(m) => format!("unknown error: {m}"),
            WalletRpcError::Locked => "wallet is locked".into(),
            WalletRpcError::Forbidden => "operation not permitted for this caller".into(),
            WalletRpcError::TransportRejected(m) => format!("request rejected: {m}"),
            WalletRpcError::Node(m) => format!("node error: {m}"),
            WalletRpcError::NoNode => {
                "no Starknet node configured: supply nonce + resource_bounds, or set an RPC URL"
                    .into()
            }
            WalletRpcError::Precondition(m) => m.clone(),
            WalletRpcError::NotImplemented(m) => format!("not implemented: {m}"),
            WalletRpcError::Parse => "parse error: body is not valid JSON".into(),
        }
    }

    pub fn to_object(&self) -> RpcErrorObject {
        RpcErrorObject {
            code: self.code(),
            message: self.message(),
        }
    }
}

impl std::fmt::Display for WalletRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for WalletRpcError {}

impl From<wallet_core::CoreError> for WalletRpcError {
    fn from(e: wallet_core::CoreError) -> Self {
        use wallet_core::CoreError::*;
        match e {
            Locked => WalletRpcError::Locked,
            BadPassphraseOrCorrupt => {
                WalletRpcError::InvalidRequest("incorrect passphrase or corrupt vault".into())
            }
            // A malformed caller-supplied contract class is the caller's error
            // (114), and the detail carries no key material.
            InvalidContractClass(m) => {
                WalletRpcError::InvalidRequest(format!("contract_class: {m}"))
            }
            // Everything else is an internal crypto/serialization failure that
            // must not surface key detail.
            _ => WalletRpcError::Unknown("core operation failed".into()),
        }
    }
}
