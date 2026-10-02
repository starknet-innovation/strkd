//! `wallet-rpc` — the local JSON-RPC service for the Starknet wallet companion.
//!
//! Implements the read-only + signing slice of the standard `wallet_*` API plus
//! the `companion_*` extensions (spec §7), with pairing-based caller auth
//! (§5.5) and a blocking approval broker (§8). It holds no key material itself:
//! all signing goes through [`session::WalletSession`], which delegates to
//! `wallet-core`.
//!
//! The [`dispatch`] entry point is transport-agnostic and fully testable; the
//! [`server`] module wraps it in a loopback HTTP listener.

pub mod approval;
pub mod auth;
pub mod dispatch;
pub mod error;
pub mod jsonrpc;
pub mod log;
pub mod node;
pub mod server;
pub mod session;
pub mod store;
pub mod sweep;
pub mod usage;

pub use approval::{
    ApprovalRequest, Approver, AutoApprover, ChannelApprover, Decision, PendingApproval,
};
pub use auth::{ClientInfo, ClientKind, ClientStore, PairedClient};
pub use dispatch::{dispatch, ServerState};
pub use error::WalletRpcError;
pub use jsonrpc::{Request, Response, RpcErrorObject};
pub use log::{now_unix_ms, LogEntry, RequestLog};
pub use node::{
    declare_v3_tx_json, invoke_v3_tx_json, FeeBounds, HttpStarknetRpc, NodeError, StarknetRpc,
    TxState,
};
pub use server::{bind_loopback, router, transport_guard, write_port_lock};
pub use session::{VaultContents, WalletSession};
pub use store::VaultStore;
pub use sweep::{
    AccountOutcome, AccountPlan, SweepAccount, SweepEvent, SweepPlan, SweepReport, SweepToken,
    TokenBalance,
};
