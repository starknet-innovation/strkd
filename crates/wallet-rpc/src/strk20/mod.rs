//! STRK20 (Starknet privacy pool) over the wallet API — wallet-API 0.10.4's
//! `wallet_strk20*` methods plus `companion_strk20Register`.
//!
//! How each operation is proved and submitted (`docs/project/strk20-plan.md`):
//!
//! | Operation | Proved | Submitted |
//! |---|---|---|
//! | register | on this machine | from the user's account (registration is public) |
//! | deposit | remote screening prover ([`remote`]) | from the user's account (deposits are public) |
//! | transfer / withdraw | on this machine | AVNU private relay ([`paymaster`]), or returned unsubmitted by `wallet_strk20PrepareInvoke` |
//!
//! The account's viewing key `user_sk` decrypts and spends its notes. It is
//! derived per call inside the unlocked session, used for discovery and the
//! proof invocation, and never persisted, logged or returned. Proofs go through
//! [`prover::prove_unrecorded`], which keeps the invocation (and so the key) off
//! disk. Only a deposit's invocation leaves the machine, to the screening prover
//! the approval prompt names.
//!
//! External owner contracts (#48) go through [`owners`] (enrolment), [`plan`]
//! (`companion_strk20Plan`: plan now, approve on-chain) and [`prove`]
//! (`companion_strk20Prove`: prove the approved plan later, as a job).

pub mod owners;
pub mod paymaster;
pub mod plan;
pub mod prove;
pub mod remote;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use strk20::discovery::{self, PoolReader};
use strk20::invocation::ProofInvocation;
use strk20::planner::{self, Intent, OsRandomness, Plan, State, Warning};
use wallet_core::{address_hex, get_selector_from_name, AccountRef, Call, ChainId, Felt, InvokeV3Params, Strk20Owner};

use crate::approval::{ApprovalRequest, Decision};
use crate::auth::PairedClient;
use crate::dispatch::{
    chain_name, gated_approval, normalize_address, opt_param_str, prover_network, prover_or_err, resolve_chain,
    scope_for, ServerState,
};
use crate::error::WalletRpcError;
use crate::node::{NodeError, StarknetRpc};
use crate::session::WalletSession;

pub const MAINNET_POOL: &str = "0x040337b1af3c663e86e333bab5a4b28da8d4652a15a69beee2b677776ffe812a";
pub const SEPOLIA_POOL: &str = "0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e";
/// STRK, the pool's fee token (same address on mainnet and Sepolia).
pub const STRK: &str = "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";
/// Proofs read state this many blocks back: the pool and the blockifier need
/// the proof's base block at least 10 blocks old. Discovery reads the same block.
pub const PROVING_LAG: u64 = 12;
/// Wallet-API limits (bramble's `STRK20_LIMITS`).
const MAX_ACTIONS: usize = 32;
const MAX_TOKENS: usize = 64;

fn felt(s: &str) -> Felt {
    Felt::from_hex(s).expect("constant felt")
}
fn hex(f: &Felt) -> String {
    format!("{f:#x}")
}
fn invalid(m: impl Into<String>) -> WalletRpcError {
    WalletRpcError::InvalidRequest(m.into())
}
fn node_err(e: impl std::fmt::Display) -> WalletRpcError {
    WalletRpcError::Node(e.to_string())
}

/// The pool's revert reason (a Cairo short string such as `SCREENING_REQUIRED`)
/// inside a node error, if there is one.
fn pool_revert(e: &str) -> Option<String> {
    let start = e.find("('")? + 2;
    let end = start + e[start..].find("')")?;
    let reason = &e[start..end];
    (!reason.is_empty() && reason.bytes().all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit()))
        .then(|| reason.to_string())
}

/// Remove the viewing key from text that leaves the handler (errors are returned
/// to the caller and written to the request log). Provers are not expected to
/// echo the transaction, but if one did, its calldata would carry `user_sk`.
fn scrub(text: &str, user_sk: Felt) -> String {
    let hex = format!("{user_sk:#x}");
    let padded = format!("0x{user_sk:064x}");
    let decimal = user_sk.to_string();
    text.replace(&padded, "<viewing key>").replace(&hex, "<viewing key>").replace(&decimal, "<viewing key>")
}

/// `INVALID_SIGNATURE` as a Cairo short string, as a prover revert trace
/// spells it in hex (`0x494e56414c49445f5349474e4154555245`).
const INVALID_SIGNATURE_HEX: &str = "494e56414c49445f5349474e4154555245";

/// Whether a prover error says the pool's signature check failed.
fn is_invalid_signature(text: &str) -> bool {
    text.contains("INVALID_SIGNATURE") || text.to_ascii_lowercase().contains(INVALID_SIGNATURE_HEX)
}

/// Prover error prefixes strkd itself writes (the `prover` crate's messages,
/// `snip36::classify_prover_error` first of all), each with a fixed message.
/// Only the start of the error is matched: whatever follows it can quote the
/// prover's output, which the code being proved controls.
const KNOWN_PROVER_ERRORS: &[(&str, &str)] = &[
    (
        "the transaction reverted while it was proved",
        "the batch reverted when the prover executed it at the reference block: a contract it calls failed, or is          not deployed at that block",
    ),
    ("transaction too large to prove", "the batch is too large for the prover: split it into smaller batches"),
    ("upstream RPC throttled", "the prover's RPC node throttled it (HTTP 429): use a higher-quota RPC in Settings"),
    ("RPC too old", "the prover's RPC node is too old (it needs JSON-RPC spec 0.10): change it in Settings"),
    ("prover port in use", "the prover's local port was in use: retry"),
    (
        "the bundled prover does not support",
        "the bundled prover does not support this network's Starknet version: update strkd, or use a compatible          remote prover",
    ),
    ("prover timed out", "the prover timed out"),
    ("snip36 binary not found", "the on-device prover is not installed (snip36 binary not found)"),
    ("failed to launch prover", "the on-device prover could not be started"),
    ("no RPC URL configured", "no RPC URL is configured for the prover's network (Settings)"),
    (
        "nonce mismatch",
        "the virtual transaction's nonce does not match the pool's nonce at the reference block",
    ),
    (
        "transaction is not signed",
        "the prover refuses a virtual transaction with an empty signature: pass at least one signature felt",
    ),
    ("no remote prover configured", "no remote prover is configured for this network (Settings)"),
    ("remote prover", "the remote prover returned an error"),
    ("rpc ", "the prover could not read the chain through its RPC node"),
];

/// What a STRK20 proving error says, as a reason and a fixed message that
/// quotes **nothing** from the prover's output.
///
/// The batch runs the owner contract's `is_custom_signature_valid` (and, for
/// any subject, the pool) inside the prover, with the full `compile_actions`
/// call — `user_sk` and every channel key — as its argument. A revert reason
/// is text that code chose: a malicious contract could panic with a secret,
/// encoded so no scrubbing finds it. So strkd never returns, logs or stores
/// the prover's text for these jobs; it classifies the error by the prefix
/// strkd itself wrote and returns that class (§6).
fn safe_prover_error(err: &str, owner: bool, block: u64) -> (&'static str, String) {
    let err = err.trim_start();
    if err == prover::WIPED_MESSAGE {
        return ("WIPED_ON_LOCK", prover::WIPED_MESSAGE.to_string());
    }
    if is_invalid_signature(err) {
        let why = if owner {
            format!(
                "the owner contract did not validate the batch at reference block {block}: it had not approved \
                 plan_hash at or before that block, or the signature was not one it accepts"
            )
        } else {
            "the account's signature was not accepted".to_string()
        };
        let reason = if owner { "OWNER_SIGNATURE_REJECTED" } else { "INVALID_SIGNATURE" };
        return (reason, format!("the pool's signature check failed (INVALID_SIGNATURE) when the prover executed the batch: {why}"));
    }
    let what = KNOWN_PROVER_ERRORS.iter().find(|(prefix, _)| err.starts_with(prefix)).map_or("proving failed", |(_, m)| m);
    (
        "PROVING_FAILED",
        format!("{what}. The prover's own output is withheld: the contracts it runs can put secrets in it."),
    )
}

/// The prover the plan and prove prompts name: its kind, plus the URL for a
/// remote prover (a changed URL is a different party seeing the viewing key).
async fn prover_identity(prover: &prover::ProverState, chain: ChainId) -> String {
    let kind = prover.prover.kind();
    if kind != "remote" {
        return kind.to_string();
    }
    let url = prover.settings.for_network(prover_network(chain)).await.prover_url;
    if url.is_empty() {
        "remote (no URL set)".to_string()
    } else {
        format!("remote at {url}")
    }
}

/// A submission error: explain pool reverts, pass the rest through.
fn submit_err(e: impl std::fmt::Display) -> WalletRpcError {
    let text = e.to_string();
    match pool_revert(&text).as_deref() {
        Some(r @ "SCREENING_REQUIRED") => WalletRpcError::Precondition(format!(
            "the pool refused the deposit ({r}): the deposit prover returned no screening attestation. Use a \
             screening prover (Starkscan, the bramble gateway, or the pool operator's)."
        )),
        Some(r @ ("SCREENING_EXPIRED" | "SCREENING_FUTURE_DATED")) => WalletRpcError::Precondition(format!(
            "the pool refused the deposit ({r}): its screening attestation is no longer valid; retry the deposit"
        )),
        Some(r @ "NON_ZERO_VALUE") => WalletRpcError::Precondition(format!(
            "the pool refused the batch ({r}): it was proved against state that a more recent transaction from this \
             account has since changed (a note spent, or a note or channel slot taken). Retry in about a minute."
        )),
        Some(r) => WalletRpcError::Precondition(format!("the pool refused the transaction: {r}")),
        None => WalletRpcError::Node(text),
    }
}

// ── reading the pool through the configured node ─────────────────────────────

/// A [`PoolReader`] over the wallet's node for `chain`, pinned to one block.
struct NodeReader {
    node: Arc<dyn StarknetRpc>,
    pool: Felt,
    block: Value,
}

#[async_trait]
impl PoolReader for NodeReader {
    async fn call(&self, selector: Felt, calldata: Vec<Felt>) -> Result<Vec<Felt>, strk20::Error> {
        let request = json!({ "contract_address": hex(&self.pool), "entry_point_selector": hex(&selector),
                              "calldata": calldata.iter().map(hex).collect::<Vec<_>>() });
        let out = self
            .node
            .raw("starknet_call", json!({ "request": request, "block_id": self.block }))
            .await
            .map_err(|e| strk20::Error::Read(e.to_string()))?;
        out.as_array()
            .ok_or_else(|| strk20::Error::Read("starknet_call: not an array".into()))?
            .iter()
            .map(|v| v.as_str().and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| strk20::Error::Read(format!("bad felt {v}"))))
            .collect()
    }
}

async fn block_number(node: &dyn StarknetRpc) -> Result<u64, WalletRpcError> {
    node.raw("starknet_blockNumber", json!([])).await.map_err(node_err)?.as_u64().ok_or_else(|| node_err("blockNumber: not a number"))
}

/// Whether a node error is JSON-RPC `CONTRACT_NOT_FOUND` (code 20).
fn is_contract_not_found(e: &NodeError) -> bool {
    let NodeError::Rpc(m) = e else { return false };
    let code = serde_json::from_str::<Value>(m).ok().and_then(|v| v.get("code").and_then(Value::as_i64));
    code == Some(20) || (code.is_none() && m.to_lowercase().contains("contract not found"))
}

/// Refuse to prove for an account (or owner contract) that has no contract at
/// `block`, the proving block, with an error that says why and when to retry.
async fn ensure_deployed_at(ctx: &Ctx, block: u64) -> Result<(), WalletRpcError> {
    let params = json!({ "block_id": { "block_number": block }, "contract_address": hex(&ctx.user) });
    match ctx.node.raw("starknet_getClassHashAt", params).await {
        Ok(_) => Ok(()),
        Err(e) if is_contract_not_found(&e) => {
            let what = match ctx.subject {
                Subject::Account => "account",
                Subject::Owner(_) => "owner contract",
            };
            let err = WalletRpcError::Precondition(format!(
                "{what} {} is not deployed at reference block {block} (proofs run {PROVING_LAG} blocks behind the chain \
                 head). If you just deployed it, retry in about a minute.",
                ctx.address()
            ));
            Err(match ctx.subject {
                Subject::Account => err,
                Subject::Owner(_) => err.with_reason("OWNER_NOT_DEPLOYED"),
            })
        }
        Err(e) => Err(node_err(e)),
    }
}

fn map_strk20(e: strk20::Error) -> WalletRpcError {
    match e {
        strk20::Error::NotRegistered(a) => WalletRpcError::Strk20NotRegistered(format!("{a:#x}")),
        strk20::Error::InsufficientBalance { token, shortfall, available } => WalletRpcError::InsufficientPrivateBalance(format!(
            "{token:#x}: {shortfall} short of the {available} spendable. Notes younger than {PROVING_LAG} blocks are not \
             spendable yet; retry in a minute if you just received or changed some."
        )),
        strk20::Error::ZeroAmount => invalid("amounts must be positive"),
        strk20::Error::Read(m) => node_err(m),
        e => WalletRpcError::Unknown(e.to_string()),
    }
}

// ── who, where, which pool ───────────────────────────────────────────────────

/// Who is asking. An RPC client is confined to its scoped accounts and every
/// operation goes through the approval broker; the desktop user acts on any
/// account directly (clicking the button is the consent).
#[derive(Clone, Copy)]
pub enum Caller<'a> {
    Rpc(&'a PairedClient),
    Desktop,
}

impl Caller<'_> {
    async fn approve(self, state: &ServerState, method: &str, summary: String) -> Result<(), WalletRpcError> {
        match self {
            Caller::Desktop => Ok(()),
            Caller::Rpc(client) => match gated_approval(state, client, method, summary).await {
                Decision::Reject => Err(WalletRpcError::UserRefused),
                _ => Ok(()),
            },
        }
    }

    /// Like [`Self::approve`], but an active grant never covers it: an RPC
    /// caller always gets a prompt (enrolment, D6; like `companion_requestGrant`).
    async fn approve_always(self, state: &ServerState, method: &str, summary: String) -> Result<(), WalletRpcError> {
        match self {
            Caller::Desktop => Ok(()),
            Caller::Rpc(client) => {
                let request = ApprovalRequest {
                    client_label: format!("{} ({})", client.label, client.id),
                    method: method.to_string(),
                    summary,
                };
                match state.approver.request_approval(request).await {
                    Decision::Reject => Err(WalletRpcError::UserRefused),
                    _ => Ok(()),
                }
            }
        }
    }

    /// The registry scope: an agent's client id, else `None` (the user's).
    fn scope(self) -> Option<String> {
        match self {
            Caller::Rpc(client) => scope_for(client),
            Caller::Desktop => None,
        }
    }
}

/// Whose notes an operation acts on (design note #48 §3.1).
#[derive(Clone)]
enum Subject {
    /// A strkd account: its own key derives `user_sk` and signs the invocation.
    Account,
    /// An enrolled external owner contract. [`Ctx::account`] is its controller,
    /// whose key derives the owner's `user_sk`; strkd never signs for it.
    Owner(Strk20Owner),
}

struct Ctx {
    /// The strkd account whose key is used: the subject itself, or the owner's
    /// controller.
    account: AccountRef,
    subject: Subject,
    /// The subject's address: the pool's `user`.
    user: Felt,
    chain: ChainId,
    pool: Felt,
    node: Arc<dyn StarknetRpc>,
    cfg: prover::Strk20Config,
}

/// The pool for `chain`: the user's override, else the canonical one.
pub fn pool_for(cfg: &prover::Strk20Config, chain: ChainId) -> Result<Felt, WalletRpcError> {
    match cfg.pool.trim() {
        "" => Ok(felt(match chain {
            ChainId::Mainnet => MAINNET_POOL,
            ChainId::Sepolia => SEPOLIA_POOL,
        })),
        p => Felt::from_hex(p).map_err(|_| invalid(format!("Settings: STRK20 pool {p} is not a felt"))),
    }
}

/// The chain a request targets, its STRK20 settings and pool.
async fn chain_and_pool(state: &ServerState, params: &Value) -> Result<(ChainId, prover::Strk20Config, Felt), WalletRpcError> {
    let chain = resolve_chain(state, params).await?;
    let cfg = match &state.prover {
        Some(p) => p.settings.for_network(prover_network(chain)).await.strk20,
        None => prover::Strk20Config::default(),
    };
    let pool = pool_for(&cfg, chain)?;
    Ok((chain, cfg, pool))
}

/// A non-zero address param, as [`address_hex`].
fn address_param(params: &Value, key: &str) -> Result<Option<String>, WalletRpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let f = v.as_str().and_then(|s| Felt::from_hex(s).ok()).filter(|f| *f != Felt::ZERO);
            f.map(|f| Some(address_hex(&f))).ok_or_else(|| invalid(format!("{key}: expected a non-zero address")))
        }
    }
}

/// `OWNER_NOT_ENROLLED` (-32002): unknown, or outside the caller's scope.
fn not_enrolled() -> WalletRpcError {
    WalletRpcError::Forbidden.with_reason("OWNER_NOT_ENROLLED")
}

/// Resolve the subject of a STRK20 call: `account_address` (default: the
/// caller's first scoped account) or `owner_address`, an owner the caller
/// enrolled on this chain and pool. Passing both is a 114.
async fn ctx(state: &ServerState, caller: Caller<'_>, params: &Value) -> Result<Ctx, WalletRpcError> {
    let owner = address_param(params, "owner_address")?;
    if owner.is_some() && params.get("account_address").is_some_and(|v| !v.is_null()) {
        return Err(invalid(
            "pass owner_address or account_address, not both (an owner's controller is fixed at enrolment)",
        ));
    }
    let (chain, cfg, pool) = chain_and_pool(state, params).await?;
    let (account, subject) = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let scope = caller.scope();
        match owner {
            None => {
                let mut scoped = reg.scoped_for(scope.as_deref());
                let account = match opt_param_str(params, "account_address") {
                    Some(a) => {
                        let want = normalize_address(&a)?;
                        scoped.find(|acc| acc.address == want).cloned().ok_or(WalletRpcError::Forbidden)?
                    }
                    None => scoped.next().cloned().ok_or(WalletRpcError::Forbidden)?,
                };
                (account, Subject::Account)
            }
            Some(owner) => {
                let record = reg
                    .strk20_owners_for(scope.as_deref())
                    .find(|o| o.is(&owner, chain, &address_hex(&pool), scope.as_deref()))
                    .cloned()
                    .ok_or_else(not_enrolled)?;
                let c = &record.controller;
                let controller = reg
                    .scoped_for(scope.as_deref())
                    .find(|a| a.address == c.address && a.domain == c.domain && a.index == c.index)
                    .cloned()
                    .ok_or_else(not_enrolled)?;
                (controller, Subject::Owner(record))
            }
        }
    };
    let user = match &subject {
        Subject::Account => &account.address,
        Subject::Owner(o) => &o.owner,
    };
    let user = Felt::from_hex(user).map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
    let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
    Ok(Ctx { account, subject, user, chain, pool, node, cfg })
}

impl Ctx {
    fn reader(&self, block: Value) -> NodeReader {
        NodeReader { node: self.node.clone(), pool: self.pool, block }
    }

    /// The subject's address, as [`address_hex`].
    fn address(&self) -> String {
        address_hex(&self.user)
    }

    /// Refuse an owner subject on a method that signs with or submits from the
    /// subject's own account. Owners get the plan/prove methods (#48, phase 1).
    fn account_only(self, method: &str) -> Result<Self, WalletRpcError> {
        match self.subject {
            Subject::Account => Ok(self),
            Subject::Owner(_) => Err(WalletRpcError::NotImplemented(format!(
                "owner_address on {method}: STRK20 operations for an owner contract come with the plan/prove methods"
            ))),
        }
    }

    /// The subject's viewing key: the account's own, or the owner's, derived
    /// from its controller. Never persisted, logged or returned.
    async fn viewing_key(&self, state: &ServerState) -> Result<Felt, WalletRpcError> {
        let session = state.session.lock().await;
        match &self.subject {
            Subject::Account => session.strk20_viewing_key_for(&self.account, self.chain, &self.pool),
            Subject::Owner(_) => session.strk20_owner_viewing_key_for(&self.account, self.chain, &self.pool, &self.user),
        }
    }
}

// ── actions (wallet-API 0.10.4 STRK20_ACTION) ────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Deposit { token: Felt, amount: u128 },
    Withdraw { token: Felt, amount: u128, recipient: Felt },
    Transfer { token: Felt, amount: u128, recipient: Felt },
}

fn field_felt(a: &Value, k: &str) -> Result<Felt, WalletRpcError> {
    a.get(k).and_then(Value::as_str).and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| invalid(format!("action.{k}: expected a felt")))
}

fn field_amount(a: &Value) -> Result<u128, WalletRpcError> {
    if a.get("amount").and_then(Value::as_str) == Some("OPEN") {
        return Err(WalletRpcError::NotImplemented("open notes (transfer amount \"OPEN\") are not supported yet".into()));
    }
    let f = field_felt(a, "amount")?;
    let n = u128::try_from(f).map_err(|_| invalid("action.amount exceeds u128"))?;
    if n == 0 {
        return Err(invalid("action.amount must be positive"));
    }
    Ok(n)
}

pub fn parse_actions(params: &Value) -> Result<Vec<Action>, WalletRpcError> {
    let list = params.get("actions").and_then(Value::as_array).ok_or_else(|| invalid("missing array param 'actions'"))?;
    if list.is_empty() || list.len() > MAX_ACTIONS {
        return Err(invalid(format!("actions: between 1 and {MAX_ACTIONS} required")));
    }
    list.iter()
        .map(|a| {
            let token = || field_felt(a, "token");
            match a.get("type").and_then(Value::as_str) {
                Some("deposit") => Ok(Action::Deposit { token: token()?, amount: field_amount(a)? }),
                Some("withdraw") => Ok(Action::Withdraw { token: token()?, amount: field_amount(a)?, recipient: field_felt(a, "recipient")? }),
                Some("transfer") => Ok(Action::Transfer { token: token()?, amount: field_amount(a)?, recipient: field_felt(a, "recipient")? }),
                Some(t @ ("invoke" | "shadow_account_invoke" | "subaccount_invoke")) => {
                    Err(WalletRpcError::NotImplemented(format!("STRK20 `{t}` actions are not supported yet")))
                }
                _ => Err(invalid(format!("unknown STRK20 action {a}"))),
            }
        })
        .collect()
}

fn intent_of(actions: &[Action]) -> Intent {
    let mut intent = Intent::default();
    for a in actions {
        match *a {
            Action::Deposit { token, amount } => intent.deposits.push((token, amount)),
            Action::Withdraw { token, amount, recipient } => intent.withdrawals.push((recipient, token, amount)),
            Action::Transfer { token, amount, recipient } => intent.transfers.push((recipient, token, amount)),
        }
    }
    intent
}

fn describe(intent: &Intent) -> String {
    let wei = |a: u128| format!("{a} (base units)");
    let mut parts = Vec::new();
    if intent.register {
        parts.push("register your viewing key (public: links this address to the pool)".to_string());
    }
    for (t, a) in &intent.deposits {
        parts.push(format!("deposit {} of {t:#x} (public)", wei(*a)));
    }
    for (r, t, a) in &intent.transfers {
        parts.push(format!("send {} of {t:#x} privately to {r:#x}", wei(*a)));
    }
    for (r, t, a) in &intent.withdrawals {
        parts.push(format!("withdraw {} of {t:#x} to {r:#x}", wei(*a)));
    }
    parts.join("; ")
}

// ── the shared pipeline: discover → plan → approve → sign → prove ────────────

/// How the batch will be proved.
enum Proving {
    Local,
    Remote(remote::DepositProver),
}

struct Prepared {
    call: Call,
    proved: remote::ProvedInvocation,
}

async fn discover_state(ctx: &Ctx, reader: &NodeReader, user_sk: Felt, intent: &Intent) -> Result<State, WalletRpcError> {
    let registered = discovery::public_key(reader, ctx.user).await.map_err(map_strk20)?.is_some();
    let mut channels = BTreeMap::new();
    if registered {
        let recipients = intent.transfers.iter().map(|t| t.0).chain([ctx.user]);
        for r in recipients {
            if channels.contains_key(&r) {
                continue;
            }
            channels.insert(r, discovery::discover_channel(reader, ctx.user, user_sk, r).await.map_err(map_strk20)?);
        }
    }
    let tokens: Vec<Felt> = intent.transfers.iter().map(|t| t.1).chain(intent.withdrawals.iter().map(|w| w.1)).collect();
    let notes = if registered && !tokens.is_empty() {
        discovery::discover_notes(reader, ctx.user, user_sk, &tokens).await.map_err(map_strk20)?
    } else {
        Vec::new()
    };
    let outgoing_channels = if registered { discovery::outgoing_channel_count(reader, ctx.user, user_sk).await.map_err(map_strk20)? } else { 0 };
    Ok(State { user: ctx.user, user_sk, registered, outgoing_channels, channels, notes })
}

/// Whether two discoveries of the same account agree on everything a plan
/// writes to or spends.
fn same_state(a: &State, b: &State) -> bool {
    a.registered == b.registered && a.outgoing_channels == b.outgoing_channels && a.channels == b.channels && a.notes == b.notes
}

/// A batch planned against the reference block: the part of the pipeline that
/// `prepare()` (one-shot) and `companion_strk20Plan` share.
struct Planned {
    /// The reference block the state was discovered at (`head − PROVING_LAG`).
    block: u64,
    user_sk: Felt,
    /// The discovered state at `block` (it carries `user_sk`).
    state: State,
    plan: Plan,
}

/// Discover the subject's state at `head − PROVING_LAG` and at `latest`,
/// refuse if they differ, and plan `intent` with randomness drawn once from the
/// OS. Refuses a batch that would link channels (`PRIVACY_LEAK`). For an owner,
/// also checks that it still advertises SRC5 custom validation at the
/// reference block (§4, "Failing early").
async fn plan_at_reference(state: &ServerState, ctx: &Ctx, intent: &Intent) -> Result<Planned, WalletRpcError> {
    let head = block_number(ctx.node.as_ref()).await?;
    let block = head.saturating_sub(PROVING_LAG);
    // The prover executes the account's signature check at `block`. An account
    // deployed after it (e.g. just now) would revert in the prover (#50).
    ensure_deployed_at(ctx, block).await?;
    if let Subject::Owner(_) = ctx.subject {
        if !owners::supports_custom_validation(ctx, json!({ "block_number": block })).await? {
            return Err(WalletRpcError::Precondition(format!(
                "owner contract {} does not advertise SRC5 interface {} (is_custom_signature_valid) at reference \
                 block {block}, so the pool would not let it approve this batch.",
                ctx.address(),
                owners::CUSTOM_SIGNATURE_INTERFACE_ID
            ))
            .with_reason("OWNER_NO_CUSTOM_VALIDATION"));
        }
    }
    let reader = ctx.reader(json!({ "block_number": block }));
    let user_sk = ctx.viewing_key(state).await?;

    let st = discover_state(ctx, &reader, user_sk, intent).await?;
    // The proof runs against `block`, but the pool applies it to the latest
    // state. If this account's private state moved since (a spend or a new
    // note in the last PROVING_LAG blocks), the batch would re-spend a note or
    // reuse a slot and revert with NON_ZERO_VALUE after a wasted proof.
    let now = discover_state(ctx, &ctx.reader(json!("latest")), user_sk, intent).await?;
    if !same_state(&st, &now) {
        return Err(WalletRpcError::Precondition(format!(
            "a recent STRK20 transaction from this account is not yet {PROVING_LAG} blocks old, and proofs run \
             {PROVING_LAG} blocks behind the chain head. Retry in about a minute."
        )));
    }
    // Randomness is drawn here, once: a stored plan never redraws it (§5.2).
    let plan: Plan = planner::plan(intent, &st, &mut OsRandomness).map_err(map_strk20)?;
    if plan.warnings.contains(&Warning::UserLinkage) {
        return Err(WalletRpcError::PrivacyLeak(
            "this batch would open more than one channel at once, letting an observer link them to one sender. \
             Send to one new recipient per batch."
                .into(),
        ));
    }
    Ok(Planned { block, user_sk, state: st, plan })
}

/// How the prompt names where the batch is proved.
fn where_proved(prover: &prover::ProverState, proving: &Proving) -> String {
    match proving {
        Proving::Local if prover.prover.kind() == "native" => "proved on this machine".to_string(),
        Proving::Local => "proved by your configured remote prover, which will see your viewing key".to_string(),
        Proving::Remote(p) => format!("proved by {}, which screens the depositor and will see your viewing key", p.describe()),
    }
}

async fn prepare(
    state: &ServerState,
    caller: Caller<'_>,
    method: &str,
    ctx: &Ctx,
    intent: &Intent,
    proving: Proving,
    submission: &str,
) -> Result<Prepared, WalletRpcError> {
    let prover = prover_or_err(state)?;
    let Planned { block, user_sk, plan, .. } = plan_at_reference(state, ctx, intent).await?;

    let where_proved = where_proved(prover, &proving);
    caller
        .approve(
            state,
            method,
            format!(
                "STRK20 on {} from {}: {}. {} ({} action(s), pool {:#x}); {}.",
                chain_name(ctx.chain),
                ctx.address(),
                describe(intent),
                where_proved,
                plan.actions.len(),
                ctx.pool,
                submission,
            ),
        )
        .await?;

    let pool_nonce = ctx
        .node
        .raw("starknet_getNonce", json!({ "block_id": { "block_number": block }, "contract_address": hex(&ctx.pool) }))
        .await
        .map_err(node_err)?;
    let pool_nonce = pool_nonce.as_str().and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| node_err("getNonce: bad felt"))?;
    let invocation = ProofInvocation::new(ctx.pool, ctx.chain, pool_nonce, ctx.user, user_sk, &plan.actions);
    let signature = state.session.lock().await.sign_hash_for(&ctx.account, &invocation.hash())?;
    let tx = invocation.to_rpc_json(&signature);

    let result = match proving {
        Proving::Local => prover::prove_unrecorded(
            prover,
            json!({ "transaction": tx, "block_number": block }),
            Some(format!("STRK20 {method}")),
            prover_network(ctx.chain).to_string(),
        )
        .await
        .map_err(|e| {
            // Never the prover's text (see `safe_prover_error`).
            let (reason, message) = safe_prover_error(&e, false, block);
            WalletRpcError::Unknown(format!("proving failed: {message}")).with_reason(reason)
        })?,
        Proving::Remote(p) => remote::prove(&p, ctx.chain, block, &tx).await.map_err(|e| match e {
            remote::RemoteError::ScreeningRejected(_) => WalletRpcError::Precondition(e.to_string()),
            _ => WalletRpcError::Unknown(scrub(&e.to_string(), user_sk)),
        })?,
    };
    let proved = remote::parse_result(&result, &ctx.pool).map_err(|_| unreadable_proof())?;
    let call = strk20::apply::apply_actions_call(ctx.pool, &proved.output, proved.screening.as_ref()).map_err(map_strk20)?;
    Ok(Prepared { call, proved })
}

/// A prover result strkd can't parse. Fixed text: the result came from the
/// prover, whose output the proved contracts influence.
fn unreadable_proof() -> WalletRpcError {
    WalletRpcError::Unknown("the prover's result could not be read (its text is withheld)".into())
        .with_reason("PROVING_FAILED")
}

fn call_and_proof(p: &Prepared, simulate: bool) -> Value {
    let proof = if simulate {
        json!({ "data": "", "output": [], "proof_facts": [] })
    } else {
        json!({
            "data": p.proved.proof,
            "output": p.proved.output.iter().map(hex).collect::<Vec<_>>(),
            "proof_facts": p.proved.proof_facts.iter().map(hex).collect::<Vec<_>>(),
        })
    };
    json!({
        "call": { "contract_address": hex(&p.call.to), "entry_point": "apply_actions", "calldata": p.call.calldata.iter().map(hex).collect::<Vec<_>>() },
        "proof": proof,
    })
}

fn has_deposit(actions: &[Action]) -> bool {
    actions.iter().any(|a| matches!(a, Action::Deposit { .. }))
}

fn proving_for(ctx: &Ctx, actions: &[Action]) -> Result<Proving, WalletRpcError> {
    if has_deposit(actions) {
        remote::DepositProver::resolve(&ctx.cfg, ctx.chain).map(Proving::Remote).map_err(WalletRpcError::Precondition)
    } else {
        Ok(Proving::Local)
    }
}

// ── submission from the user's own account (register, deposits) ──────────────

async fn pool_fee(ctx: &Ctx) -> Result<u128, WalletRpcError> {
    pool_fee_at(&ctx.node, ctx.pool).await
}

/// The pool's fee (in STRK) at `latest`.
async fn pool_fee_at(node: &Arc<dyn StarknetRpc>, pool: Felt) -> Result<u128, WalletRpcError> {
    let reader = NodeReader { node: node.clone(), pool, block: json!("latest") };
    let out = reader.call(get_selector_from_name("get_fee_amount"), vec![]).await.map_err(map_strk20)?;
    let fee = out.first().copied().unwrap_or(Felt::ZERO);
    u128::try_from(fee).map_err(|_| node_err("get_fee_amount exceeds u128"))
}

/// Refuse a deposit the account can't pay for before asking a screening
/// prover: the pool pulls the deposits plus its fee (in STRK) from the account,
/// which also pays the transaction's gas.
async fn check_public_funds(ctx: &Ctx, deposits: &[(Felt, u128)]) -> Result<(), WalletRpcError> {
    let mut needed: BTreeMap<Felt, u128> = BTreeMap::new();
    for &(token, amount) in deposits {
        let n = needed.entry(token).or_default();
        *n = n.saturating_add(amount);
    }
    let fee = pool_fee(ctx).await?;
    let n = needed.entry(felt(STRK)).or_default();
    *n = n.saturating_add(fee);
    for (token, need) in needed {
        let request = json!({ "contract_address": hex(&token), "entry_point_selector": hex(&get_selector_from_name("balance_of")),
                              "calldata": [hex(&ctx.user)] });
        let out = ctx.node.raw("starknet_call", json!({ "request": request, "block_id": "latest" })).await.map_err(node_err)?;
        let low = out.get(0).and_then(Value::as_str).and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| node_err("balance_of: bad result"))?;
        let have = u128::try_from(low).unwrap_or(u128::MAX);
        if have < need {
            return Err(WalletRpcError::Precondition(format!(
                "{} holds {have} of {token:#x}, and this deposit needs {need} (the deposit{}), plus gas. Fund the account first.",
                ctx.address(),
                if token == felt(STRK) && fee > 0 { format!(" plus the pool's fee of {fee}") } else { String::new() },
            )));
        }
    }
    Ok(())
}

fn approve(token: Felt, spender: Felt, amount: u128) -> Call {
    Call { to: token, selector: get_selector_from_name("approve"), calldata: vec![spender, Felt::from(amount), Felt::ZERO] }
}

/// Submit `apply_actions` (plus the ERC-20 approvals the pool will `transfer_from`)
/// as a proof-carrying invoke from the user's account.
async fn submit_from_account(state: &ServerState, ctx: &Ctx, prepared: &Prepared, deposits: &[(Felt, u128)]) -> Result<Felt, WalletRpcError> {
    let target =
        SubmitTarget { session: state.session.clone(), node: ctx.node.clone(), chain: ctx.chain, pool: ctx.pool, guard: None };
    submit_from(&target, &ctx.account, prepared, deposits).await
}

/// Where and how a proved batch is submitted. Owned (no borrow of the server
/// state), so a background proving job can submit too.
#[derive(Clone)]
struct SubmitTarget {
    session: Arc<tokio::sync::Mutex<WalletSession>>,
    node: Arc<dyn StarknetRpc>,
    chain: ChainId,
    pool: Felt,
    /// For a background job: re-checked under the session lock right before
    /// signing (and again before broadcasting).
    guard: Option<SubmitGuard>,
}

/// What a background job's submission re-checks at the last moment: the job
/// was not wiped by a lock (a lock and a quick unlock must not let it submit),
/// and the submitter is still in the client's scope.
#[derive(Clone)]
struct SubmitGuard {
    jobs: Arc<prover::Jobs>,
    job_id: String,
    /// The client's registry scope (`Caller::scope`).
    scope: Option<String>,
}

impl SubmitGuard {
    fn check_wiped(&self) -> Result<(), WalletRpcError> {
        if self.jobs.is_wiped(&self.job_id) {
            return Err(WalletRpcError::Precondition(
                "the wallet locked while this job ran, so the batch was not submitted".into(),
            )
            .with_reason("WIPED_ON_LOCK"));
        }
        Ok(())
    }

    fn check(&self, session: &WalletSession, submitter: &AccountRef) -> Result<(), WalletRpcError> {
        self.check_wiped()?;
        let in_scope = session
            .registry()?
            .scoped_for(self.scope.as_deref())
            .any(|a| a.address == submitter.address && a.domain == submitter.domain && a.index == submitter.index);
        if !in_scope {
            return Err(WalletRpcError::Forbidden.with_reason("SUBMITTER_NOT_IN_SCOPE"));
        }
        Ok(())
    }
}

/// Submit `apply_actions` (plus the ERC-20 approvals the pool will
/// `transfer_from`) as a proof-carrying invoke from `submitter`, a strkd
/// account in the caller's scope, which signs and pays the gas and the pool's
/// fee. For an account's own batch the submitter is the account itself; for an
/// owner's plan (`companion_strk20Prove` with `submit: "account"`) it is any
/// in-scope strkd account, publicly linked to the owner by doing so.
async fn submit_from(
    target: &SubmitTarget,
    submitter: &AccountRef,
    prepared: &Prepared,
    deposits: &[(Felt, u128)],
) -> Result<Felt, WalletRpcError> {
    let sender = Felt::from_hex(&submitter.address).map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
    if let Some(a) = &prepared.proved.screening {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // Leave a minute for inclusion: the pool checks age against the block timestamp.
        if now.saturating_add(60) > a.issued_at.saturating_add(strk20::apply::ATTESTATION_MAX_AGE_SECS) {
            return Err(WalletRpcError::Precondition(
                "the deposit's screening attestation expired before it could be submitted; retry the deposit".into(),
            ));
        }
    }
    let strk = felt(STRK);
    let mut approvals: BTreeMap<Felt, u128> = BTreeMap::new();
    for &(token, amount) in deposits {
        let a = approvals.entry(token).or_default();
        *a = a.saturating_add(amount);
    }
    let fee = pool_fee_at(&target.node, target.pool).await?;
    if fee > 0 {
        let a = approvals.entry(strk).or_default();
        *a = a.saturating_add(fee);
    }
    let mut calls: Vec<Call> = approvals.into_iter().map(|(t, a)| approve(t, target.pool, a)).collect();
    calls.push(prepared.call.clone());

    let calldata = wallet_core::encode_calls(&calls);
    let nonce = target.node.get_nonce(&sender).await.map_err(node_err)?;
    let bounds = target
        .node
        .estimate_invoke_with_proof(&sender, &calldata, &nonce, &prepared.proved.proof_facts, &prepared.proved.proof)
        .await
        .map_err(submit_err)?;
    let session = target.session.lock().await;
    if let Some(g) = &target.guard {
        g.check(&session, submitter)?;
    }
    let signed = session.sign_invoke_for(
        submitter,
        &calls,
        target.chain,
        &InvokeV3Params {
            nonce,
            l1_gas: bounds.l1_gas,
            l2_gas: bounds.l2_gas,
            l1_data_gas: bounds.l1_data_gas,
            proof_facts: prepared.proved.proof_facts.clone(),
            ..InvokeV3Params::default()
        },
    )?;
    drop(session);
    if let Some(g) = &target.guard {
        g.check_wiped()?;
    }
    target
        .node
        .add_invoke(&sender, &signed.calldata, &signed.signature, &nonce, &bounds, &prepared.proved.proof_facts, Some(&prepared.proved.proof))
        .await
        .map_err(submit_err)
}

// ── handlers ─────────────────────────────────────────────────────────────────

/// `wallet_strk20Balances { tokens, account_address?, chainId? }` → `[{token, balance}]`.
///
/// Reveals private balances to the caller, so it is gated like a signature.
pub async fn balances(state: &ServerState, caller: Caller<'_>, params: &Value) -> Result<Value, WalletRpcError> {
    let tokens: Vec<Felt> = params
        .get("tokens")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("missing array param 'tokens'"))?
        .iter()
        .map(|t| t.as_str().and_then(|s| Felt::from_hex(s).ok()).ok_or_else(|| invalid("tokens: expected felts")))
        .collect::<Result<_, _>>()?;
    if tokens.is_empty() || tokens.len() > MAX_TOKENS {
        return Err(invalid(format!("tokens: between 1 and {MAX_TOKENS} required")));
    }
    let ctx = ctx(state, caller, params).await?.account_only("wallet_strk20Balances")?;
    caller
        .approve(
            state,
            "wallet_strk20Balances",
            format!(
                "Reveal the private (STRK20) balance of {} token(s) held by {} on {}.",
                tokens.len(),
                ctx.address(),
                chain_name(ctx.chain)
            ),
        )
        .await?;
    let user_sk = ctx.viewing_key(state).await?;
    let reader = ctx.reader(json!("latest"));
    if discovery::public_key(&reader, ctx.user).await.map_err(map_strk20)?.is_none() {
        return Err(WalletRpcError::Strk20NotRegistered(ctx.address()));
    }
    let notes = discovery::discover_notes(&reader, ctx.user, user_sk, &tokens).await.map_err(map_strk20)?;
    Ok(json!(tokens
        .iter()
        .map(|t| {
            let total: u128 = notes.iter().filter(|n| n.token == *t).map(|n| n.amount).sum();
            json!({ "token": hex(t), "balance": format!("{total:#x}") })
        })
        .collect::<Vec<_>>()))
}

/// `companion_strk20Register { account_address?, chainId? }` → `{ transaction_hash }`.
///
/// Registers the viewing key and opens the account's channel to itself, proved
/// locally, submitted from the account (registration is public by nature).
pub async fn register(state: &ServerState, caller: Caller<'_>, params: &Value) -> Result<Value, WalletRpcError> {
    let ctx = ctx(state, caller, params).await?.account_only("companion_strk20Register")?;
    let reader = ctx.reader(json!("latest"));
    if discovery::public_key(&reader, ctx.user).await.map_err(map_strk20)?.is_some() {
        return Err(WalletRpcError::Precondition(format!("{} is already registered with the pool", ctx.address())));
    }
    let intent = Intent { register: true, ..Intent::default() };
    let prepared =
        prepare(state, caller, "companion_strk20Register", &ctx, &intent, Proving::Local, "submitted from this account").await?;
    let hash = submit_from_account(state, &ctx, &prepared, &[]).await?;
    Ok(json!({ "transaction_hash": hex(&hash) }))
}

/// `wallet_strk20PrepareInvoke { actions, simulate?, account_address?, chainId? }`
/// → `STRK20_CALL_AND_PROOF`, for the caller to submit from any account.
pub async fn prepare_invoke(state: &ServerState, caller: Caller<'_>, params: &Value) -> Result<Value, WalletRpcError> {
    let actions = parse_actions(params)?;
    let ctx = ctx(state, caller, params).await?.account_only("wallet_strk20PrepareInvoke")?;
    let simulate = params.get("simulate").and_then(Value::as_bool).unwrap_or(false);
    let proving = proving_for(&ctx, &actions)?;
    let prepared = prepare(
        state,
        caller,
        "wallet_strk20PrepareInvoke",
        &ctx,
        &intent_of(&actions),
        proving,
        "returned to the caller unsubmitted",
    )
    .await?;
    Ok(call_and_proof(&prepared, simulate))
}

/// `wallet_strk20InvokeTransaction { actions, account_address?, chainId? }`
/// → `{ transaction_hash }`.
///
/// Deposits are public and go from the user's account; they can't share a
/// batch with private actions. Everything else is relayed by the AVNU
/// paymaster, paying its fee from the shielded balance; without an AVNU key
/// the caller is pointed to `wallet_strk20PrepareInvoke`.
pub async fn invoke_transaction(state: &ServerState, caller: Caller<'_>, params: &Value) -> Result<Value, WalletRpcError> {
    let actions = parse_actions(params)?;
    let ctx = ctx(state, caller, params).await?.account_only("wallet_strk20InvokeTransaction")?;
    let mut intent = intent_of(&actions);

    if has_deposit(&actions) {
        if actions.iter().any(|a| !matches!(a, Action::Deposit { .. })) {
            return Err(invalid(
                "deposits are submitted publicly from your account, so they can't share a batch with transfers or \
                 withdrawals: send the deposit first, then the private actions",
            ));
        }
        let proving = proving_for(&ctx, &actions)?;
        check_public_funds(&ctx, &intent.deposits).await?;
        let prepared =
            prepare(state, caller, "wallet_strk20InvokeTransaction", &ctx, &intent, proving, "submitted from this account").await?;
        let hash = submit_from_account(state, &ctx, &prepared, &intent.deposits).await?;
        return Ok(json!({ "transaction_hash": hex(&hash) }));
    }

    if ctx.cfg.avnu_api_key.trim().is_empty() {
        return Err(WalletRpcError::Precondition(
            "private transfers and withdrawals are relayed through the AVNU paymaster, and no AVNU API key is set \
             (Settings → STRK20). Without one, call wallet_strk20PrepareInvoke and submit the returned call and proof \
             from another account."
                .into(),
        ));
    }
    let url = match ctx.cfg.paymaster_url.trim() {
        "" => paymaster::default_url(ctx.chain).to_string(),
        u => u.to_string(),
    };
    let relay = paymaster::Paymaster::new(&url, ctx.cfg.avnu_api_key.trim(), felt(STRK));
    let fee = relay.quote(&ctx.pool, &ctx.user).await.map_err(WalletRpcError::Unknown)?;
    intent.withdrawals.push((fee.recipient, fee.token, fee.amount));
    let submission = format!("relayed privately by the AVNU paymaster for a fee of {} (base units of {:#x})", fee.amount, fee.token);
    let prepared = prepare(state, caller, "wallet_strk20InvokeTransaction", &ctx, &intent, Proving::Local, &submission)
        .await
        .map_err(|e| match e {
            WalletRpcError::InsufficientPrivateBalance(m) => WalletRpcError::InsufficientPrivateBalance(format!(
                "{m} The amount includes the AVNU relay fee of {} (base units of {:#x}), which covers the pool's fee.",
                fee.amount, fee.token
            )),
            e => e,
        })?;
    let hash = relay.execute(&prepared.call, &prepared.proved.proof, &prepared.proved.proof_facts).await.map_err(WalletRpcError::Unknown)?;
    Ok(json!({ "transaction_hash": hex(&hash) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wallet_api_actions() {
        let p = json!({ "actions": [
            { "type": "deposit", "token": STRK, "amount": "0xde0b6b3a7640000" },
            { "type": "transfer", "token": STRK, "amount": "0x10", "recipient": "0xb0b" },
            { "type": "withdraw", "token": STRK, "amount": "0x5", "recipient": "0xa11ce" },
        ]});
        let a = parse_actions(&p).unwrap();
        assert_eq!(a[0], Action::Deposit { token: felt(STRK), amount: 10u128.pow(18) });
        let i = intent_of(&a);
        assert_eq!((i.deposits.len(), i.transfers.len(), i.withdrawals.len()), (1, 1, 1));
    }

    #[test]
    fn rejects_unsupported_and_malformed_actions() {
        let open = json!({ "actions": [{ "type": "transfer", "token": STRK, "amount": "OPEN", "recipient": "0x1" }] });
        assert!(matches!(parse_actions(&open), Err(WalletRpcError::NotImplemented(_))));
        let invoke = json!({ "actions": [{ "type": "invoke", "contract": "0x1", "calldata": [] }] });
        assert!(matches!(parse_actions(&invoke), Err(WalletRpcError::NotImplemented(_))));
        let zero = json!({ "actions": [{ "type": "deposit", "token": STRK, "amount": "0x0" }] });
        assert!(matches!(parse_actions(&zero), Err(WalletRpcError::InvalidRequest(_))));
        assert!(parse_actions(&json!({ "actions": [] })).is_err());
    }

    #[test]
    fn scrubs_the_viewing_key_in_every_spelling() {
        let sk = Felt::from_hex("0x2f6c0b3c1a9e5d4f8b7a6e5d4c3b2a19087f6e5d4c3b2a1908f7e6d5c4b3a29").unwrap();
        let text = format!("calldata [0x1, {sk:#x}, 0x{sk:064x}, {sk}]");
        let out = scrub(&text, sk);
        assert!(!out.contains(&format!("{sk:#x}")[2..]) && !out.contains(&sk.to_string()), "{out}");
        assert_eq!(out.matches("<viewing key>").count(), 3);
    }

    #[test]
    fn pool_reverts_are_explained() {
        let raw = r#"rpc: {"code":41,"data":{"error":"0x53435245454e494e475f5245515549524544 ('SCREENING_REQUIRED')"}}"#;
        assert_eq!(pool_revert(raw).as_deref(), Some("SCREENING_REQUIRED"));
        assert!(matches!(submit_err(raw), WalletRpcError::Precondition(m) if m.contains("no screening attestation")));
        assert!(matches!(submit_err("rpc: timeout"), WalletRpcError::Node(_)));
        let stale = r#"rpc: {"code":41,"data":{"error":"0x4e4f4e5f5a45524f5f56414c5545 ('NON_ZERO_VALUE')"}}"#;
        assert!(matches!(submit_err(stale), WalletRpcError::Precondition(m) if m.contains("about a minute")));
    }

    #[test]
    fn contract_not_found_is_told_apart_from_other_node_errors() {
        assert!(is_contract_not_found(&NodeError::Rpc(r#"{"code":20,"message":"Contract not found"}"#.into())));
        assert!(is_contract_not_found(&NodeError::Rpc("Contract not found".into())));
        // BLOCK_NOT_FOUND, transport and decode failures are node errors.
        assert!(!is_contract_not_found(&NodeError::Rpc(r#"{"code":24,"message":"Block not found"}"#.into())));
        assert!(!is_contract_not_found(&NodeError::Transport("contract not found".into())));
        assert!(!is_contract_not_found(&NodeError::Decode("missing result".into())));
    }

    #[test]
    fn pool_defaults_and_override() {
        let mut cfg = prover::Strk20Config::default();
        assert_eq!(pool_for(&cfg, ChainId::Mainnet).unwrap(), felt(MAINNET_POOL));
        assert_eq!(pool_for(&cfg, ChainId::Sepolia).unwrap(), felt(SEPOLIA_POOL));
        cfg.pool = "0x123".into();
        assert_eq!(pool_for(&cfg, ChainId::Sepolia).unwrap(), Felt::from(0x123u64));
    }
}

#[cfg(test)]
mod safe_error_tests {
    use super::*;

    #[test]
    fn prover_errors_never_echo_the_prover_text() {
        // An owner contract that panics with an encoded secret: the revert
        // reason is the contract's choice, so none of it may come back.
        let encoded = "0x7365637265742d6b65792d656e636f646564";
        let revert = format!(
            "the transaction reverted while it was proved, and the prover proves only transactions that succeed. \
             revert reason: Error in the called contract: ('{encoded}')"
        );
        let (reason, m) = safe_prover_error(&revert, true, 9);
        assert_eq!(reason, "PROVING_FAILED");
        assert!(m.starts_with("the batch reverted") && !m.contains(encoded), "{m}");
        // The contract can make the pool fail INVALID_SIGNATURE and still not leak.
        let (reason, m) = safe_prover_error(&format!("{revert} 'INVALID_SIGNATURE' {encoded}"), true, 9);
        assert_eq!(reason, "OWNER_SIGNATURE_REJECTED");
        assert!(m.contains("reference block 9") && !m.contains(encoded), "{m}");
        assert_eq!(safe_prover_error("0x494E56414C49445F5349474E4154555245", false, 9).0, "INVALID_SIGNATURE");
        // Only the start counts: a known phrase inside quoted text is ignored.
        let (_, m) = safe_prover_error(&format!("prover failed: {encoded} transaction too large to prove"), true, 9);
        assert!(m.starts_with("proving failed.") && !m.contains(encoded), "{m}");
        let (_, m) = safe_prover_error("transaction too large to prove: raw: whatever", true, 9);
        assert!(m.starts_with("the batch is too large"), "{m}");
        assert_eq!(safe_prover_error(prover::WIPED_MESSAGE, true, 9).0, "WIPED_ON_LOCK");
    }
}
