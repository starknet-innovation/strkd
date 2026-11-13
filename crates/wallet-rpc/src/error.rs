//! Wallet RPC errors and their wire codes.
//!
//! Codes 111–163 are the standard `wallet_rpc.json` application codes (spec
//! §7.5). Negative codes in the JSON-RPC server-reserved range (-32000..-32099)
//! and the standard -32600/-32601/-32700 are used for transport/condition
//! errors the wallet spec does not enumerate: -32001 Locked, -32002 Forbidden,
//! -32003 TransportRejected, -32601 NotImplemented, -32700 Parse.
//!
//! An error can also carry a machine-readable reason (`OWNER_NOT_DEPLOYED`, …,
//! design note #48 §7) through [`WalletRpcError::with_reason`]: the code is the
//! wrapped error's, the message is prefixed with the reason, and the JSON-RPC
//! error object gains `data: { reason }`. Errors without one serialize exactly
//! as before (no `data` member).

use serde_json::json;

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
    /// STRK20: the account has no viewing key on the pool (`NOT_REGISTERED`).
    Strk20NotRegistered(String),
    /// STRK20 `INSUFFICIENT_PRIVATE_BALANCE`.
    InsufficientPrivateBalance(String),
    /// STRK20 `PRIVACY_LEAK`: the batch would link the user's activity.
    PrivacyLeak(String),
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
    /// `error` with a machine-readable `reason` (`data.reason` on the wire).
    /// Build it with [`WalletRpcError::with_reason`].
    Reasoned { reason: &'static str, error: Box<WalletRpcError> },
}

impl WalletRpcError {
    /// Attach a machine-readable reason (an `UPPER_SNAKE` constant from the
    /// design note's table). The code stays the wrapped error's.
    pub fn with_reason(self, reason: &'static str) -> Self {
        match self {
            WalletRpcError::Reasoned { error, .. } => WalletRpcError::Reasoned { reason, error },
            e => WalletRpcError::Reasoned { reason, error: Box::new(e) },
        }
    }

    /// The machine-readable reason, if one was attached.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            WalletRpcError::Reasoned { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// The error without its reason.
    pub fn inner(&self) -> &WalletRpcError {
        match self {
            WalletRpcError::Reasoned { error, .. } => error.inner(),
            e => e,
        }
    }

    pub fn code(&self) -> i64 {
        match self {
            WalletRpcError::Reasoned { error, .. } => error.code(),
            WalletRpcError::UserRefused => 113,
            WalletRpcError::InvalidRequest(_) => 114,
            WalletRpcError::DeploymentDataNotAvailable => 116,
            WalletRpcError::ChainIdNotSupported => 117,
            WalletRpcError::NotRegistered => 118,
            WalletRpcError::Strk20NotRegistered(_) => 118,
            WalletRpcError::InsufficientPrivateBalance(_) => 119,
            WalletRpcError::PrivacyLeak(_) => 120,
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
            WalletRpcError::Reasoned { reason, error } => format!("{reason}: {}", error.message()),
            WalletRpcError::UserRefused => "user refused the operation".into(),
            WalletRpcError::InvalidRequest(m) => format!("invalid request payload: {m}"),
            WalletRpcError::DeploymentDataNotAvailable => "deployment data not available".into(),
            WalletRpcError::ChainIdNotSupported => "chain id not supported".into(),
            WalletRpcError::NotRegistered => "caller is not a registered client".into(),
            WalletRpcError::Strk20NotRegistered(a) => {
                format!("{a} has not registered a viewing key with the STRK20 pool (call companion_strk20Register)")
            }
            WalletRpcError::InsufficientPrivateBalance(m) => format!("insufficient private balance: {m}"),
            WalletRpcError::PrivacyLeak(m) => format!("privacy leak: {m}"),
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
            data: self.reason().map(|reason| json!({ "reason": reason })),
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
            // A caller-supplied account number out of its branch's range.
            e @ IndexOutOfRange { .. } => WalletRpcError::InvalidRequest(e.to_string()),
            // Everything else is an internal crypto/serialization failure that
            // must not surface key detail.
            _ => WalletRpcError::Unknown("core operation failed".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_errors_have_no_data_member() {
        let v = serde_json::to_value(WalletRpcError::Forbidden.to_object()).unwrap();
        assert_eq!(v, json!({ "code": -32002, "message": "operation not permitted for this caller" }));
    }

    #[test]
    fn a_reason_keeps_the_code_prefixes_the_message_and_adds_data() {
        let e = WalletRpcError::Precondition("no class at 0x1".into()).with_reason("OWNER_NOT_DEPLOYED");
        assert_eq!(e.code(), -32006);
        assert_eq!(e.reason(), Some("OWNER_NOT_DEPLOYED"));
        assert_eq!(e.inner(), &WalletRpcError::Precondition("no class at 0x1".into()));
        let v = serde_json::to_value(e.to_object()).unwrap();
        assert_eq!(
            v,
            json!({ "code": -32006, "message": "OWNER_NOT_DEPLOYED: no class at 0x1",
                    "data": { "reason": "OWNER_NOT_DEPLOYED" } })
        );
        // Re-tagging replaces the reason instead of nesting it.
        let e = e.with_reason("OWNER_KEY_MISMATCH");
        assert_eq!(e.message(), "OWNER_KEY_MISMATCH: no class at 0x1");
    }
}
