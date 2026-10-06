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

pub mod paymaster;
pub mod remote;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use strk20::discovery::{self, PoolReader};
use strk20::invocation::ProofInvocation;
use strk20::planner::{self, Intent, OsRandomness, Plan, State, Warning};
use wallet_core::{get_selector_from_name, AccountRef, Call, ChainId, Felt, InvokeV3Params};

use crate::approval::Decision;
use crate::auth::PairedClient;
use crate::dispatch::{
    chain_name, gated_approval, normalize_address, opt_param_str, prover_network, prover_or_err, resolve_chain,
    scope_for, ServerState,
};
use crate::error::WalletRpcError;
use crate::node::StarknetRpc;

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

struct Ctx {
    account: AccountRef,
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

async fn ctx(state: &ServerState, client: &PairedClient, params: &Value) -> Result<Ctx, WalletRpcError> {
    let chain = resolve_chain(state, params).await?;
    let account = {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        let scope = scope_for(client);
        let mut scoped = reg.scoped_for(scope.as_deref());
        match opt_param_str(params, "account_address") {
            Some(a) => {
                let want = normalize_address(&a)?;
                scoped.find(|acc| acc.address == want).cloned().ok_or(WalletRpcError::Forbidden)?
            }
            None => scoped.next().cloned().ok_or(WalletRpcError::Forbidden)?,
        }
    };
    let user = Felt::from_hex(&account.address).map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
    let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
    let cfg = match &state.prover {
        Some(p) => p.settings.for_network(prover_network(chain)).await.strk20,
        None => prover::Strk20Config::default(),
    };
    let pool = pool_for(&cfg, chain)?;
    Ok(Ctx { account, user, chain, pool, node, cfg })
}

impl Ctx {
    fn reader(&self, block: Value) -> NodeReader {
        NodeReader { node: self.node.clone(), pool: self.pool, block }
    }

    async fn viewing_key(&self, state: &ServerState) -> Result<Felt, WalletRpcError> {
        state.session.lock().await.strk20_viewing_key_for(&self.account, self.chain, &self.pool)
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

async fn prepare(
    state: &ServerState,
    client: &PairedClient,
    method: &str,
    ctx: &Ctx,
    intent: &Intent,
    proving: Proving,
    submission: &str,
) -> Result<Prepared, WalletRpcError> {
    let prover = prover_or_err(state)?;
    let head = block_number(ctx.node.as_ref()).await?;
    let block = head.saturating_sub(PROVING_LAG);
    let reader = ctx.reader(json!({ "block_number": block }));
    let user_sk = ctx.viewing_key(state).await?;

    let st = discover_state(ctx, &reader, user_sk, intent).await?;
    let plan: Plan = planner::plan(intent, &st, &mut OsRandomness).map_err(map_strk20)?;
    if plan.warnings.contains(&Warning::UserLinkage) {
        return Err(WalletRpcError::PrivacyLeak(
            "this batch would open more than one channel at once, letting an observer link them to one sender. \
             Send to one new recipient per batch."
                .into(),
        ));
    }

    let where_proved = match &proving {
        Proving::Local if prover.prover.kind() == "native" => "proved on this machine".to_string(),
        Proving::Local => "proved by your configured remote prover, which will see your viewing key".to_string(),
        Proving::Remote(p) => format!("proved by {}, which screens the depositor and will see your viewing key", p.describe()),
    };
    let decision = gated_approval(
        state,
        client,
        method,
        format!(
            "STRK20 on {} from {}: {}. {} ({} action(s), pool {:#x}); {}.",
            chain_name(ctx.chain),
            ctx.account.address,
            describe(intent),
            where_proved,
            plan.actions.len(),
            ctx.pool,
            submission,
        ),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

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
        .map_err(|e| WalletRpcError::Unknown(format!("proving failed: {e}")))?,
        Proving::Remote(p) => remote::prove(&p, ctx.chain, block, &tx).await.map_err(|e| match e {
            remote::RemoteError::ScreeningRejected(_) => WalletRpcError::Precondition(e.to_string()),
            _ => WalletRpcError::Unknown(e.to_string()),
        })?,
    };
    let proved = remote::parse_result(&result, &ctx.pool).map_err(WalletRpcError::Unknown)?;
    let call = strk20::apply::apply_actions_call(ctx.pool, &proved.output, proved.screening.as_ref()).map_err(map_strk20)?;
    Ok(Prepared { call, proved })
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
    let out = ctx.reader(json!("latest")).call(get_selector_from_name("get_fee_amount"), vec![]).await.map_err(map_strk20)?;
    let fee = out.first().copied().unwrap_or(Felt::ZERO);
    u128::try_from(fee).map_err(|_| node_err("get_fee_amount exceeds u128"))
}

fn approve(token: Felt, spender: Felt, amount: u128) -> Call {
    Call { to: token, selector: get_selector_from_name("approve"), calldata: vec![spender, Felt::from(amount), Felt::ZERO] }
}

/// Submit `apply_actions` (plus the ERC-20 approvals the pool will `transfer_from`)
/// as a proof-carrying invoke from the user's account.
async fn submit_from_account(state: &ServerState, ctx: &Ctx, prepared: &Prepared, deposits: &[(Felt, u128)]) -> Result<Felt, WalletRpcError> {
    if let Some(a) = &prepared.proved.screening {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // Leave a minute for inclusion: the pool checks age against the block timestamp.
        if now + 60 > a.issued_at + strk20::apply::ATTESTATION_MAX_AGE_SECS {
            return Err(WalletRpcError::Precondition(
                "the deposit's screening attestation expired before it could be submitted; retry the deposit".into(),
            ));
        }
    }
    let strk = felt(STRK);
    let mut approvals: BTreeMap<Felt, u128> = BTreeMap::new();
    for &(token, amount) in deposits {
        *approvals.entry(token).or_default() += amount;
    }
    let fee = pool_fee(ctx).await?;
    if fee > 0 {
        *approvals.entry(strk).or_default() += fee;
    }
    let mut calls: Vec<Call> = approvals.into_iter().map(|(t, a)| approve(t, ctx.pool, a)).collect();
    calls.push(prepared.call.clone());

    let calldata = wallet_core::encode_calls(&calls);
    let nonce = ctx.node.get_nonce(&ctx.user).await.map_err(node_err)?;
    let bounds = ctx
        .node
        .estimate_invoke_with_proof(&ctx.user, &calldata, &nonce, &prepared.proved.proof_facts, &prepared.proved.proof)
        .await
        .map_err(node_err)?;
    let signed = state.session.lock().await.sign_invoke_for(
        &ctx.account,
        &calls,
        ctx.chain,
        &InvokeV3Params {
            nonce,
            l1_gas: bounds.l1_gas,
            l2_gas: bounds.l2_gas,
            l1_data_gas: bounds.l1_data_gas,
            proof_facts: prepared.proved.proof_facts.clone(),
            ..InvokeV3Params::default()
        },
    )?;
    ctx.node
        .add_invoke(&ctx.user, &signed.calldata, &signed.signature, &nonce, &bounds, &prepared.proved.proof_facts, Some(&prepared.proved.proof))
        .await
        .map_err(node_err)
}

// ── handlers ─────────────────────────────────────────────────────────────────

/// `wallet_strk20Balances { tokens, account_address?, chainId? }` → `[{token, balance}]`.
///
/// Reveals private balances to the caller, so it is gated like a signature.
pub async fn handle_balances(state: &ServerState, client: &PairedClient, params: &Value) -> Result<Value, WalletRpcError> {
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
    let ctx = ctx(state, client, params).await?;
    let decision = gated_approval(
        state,
        client,
        "wallet_strk20Balances",
        format!("Reveal the private (STRK20) balance of {} token(s) held by {} on {}.", tokens.len(), ctx.account.address, chain_name(ctx.chain)),
    )
    .await;
    if decision == Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }
    let user_sk = ctx.viewing_key(state).await?;
    let reader = ctx.reader(json!("latest"));
    if discovery::public_key(&reader, ctx.user).await.map_err(map_strk20)?.is_none() {
        return Err(WalletRpcError::Strk20NotRegistered(ctx.account.address.clone()));
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
pub async fn handle_register(state: &ServerState, client: &PairedClient, params: &Value) -> Result<Value, WalletRpcError> {
    let ctx = ctx(state, client, params).await?;
    let reader = ctx.reader(json!("latest"));
    if discovery::public_key(&reader, ctx.user).await.map_err(map_strk20)?.is_some() {
        return Err(WalletRpcError::Precondition(format!("{} is already registered with the pool", ctx.account.address)));
    }
    let intent = Intent { register: true, ..Intent::default() };
    let prepared =
        prepare(state, client, "companion_strk20Register", &ctx, &intent, Proving::Local, "submitted from this account").await?;
    let hash = submit_from_account(state, &ctx, &prepared, &[]).await?;
    Ok(json!({ "transaction_hash": hex(&hash) }))
}

/// `wallet_strk20PrepareInvoke { actions, simulate?, account_address?, chainId? }`
/// → `STRK20_CALL_AND_PROOF`, for the caller to submit from any account.
pub async fn handle_prepare_invoke(state: &ServerState, client: &PairedClient, params: &Value) -> Result<Value, WalletRpcError> {
    let actions = parse_actions(params)?;
    let ctx = ctx(state, client, params).await?;
    let simulate = params.get("simulate").and_then(Value::as_bool).unwrap_or(false);
    let proving = proving_for(&ctx, &actions)?;
    let prepared = prepare(
        state,
        client,
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
pub async fn handle_invoke_transaction(state: &ServerState, client: &PairedClient, params: &Value) -> Result<Value, WalletRpcError> {
    let actions = parse_actions(params)?;
    let ctx = ctx(state, client, params).await?;
    let mut intent = intent_of(&actions);

    if has_deposit(&actions) {
        if actions.iter().any(|a| !matches!(a, Action::Deposit { .. })) {
            return Err(invalid(
                "deposits are submitted publicly from your account, so they can't share a batch with transfers or \
                 withdrawals: send the deposit first, then the private actions",
            ));
        }
        let proving = proving_for(&ctx, &actions)?;
        let prepared =
            prepare(state, client, "wallet_strk20InvokeTransaction", &ctx, &intent, proving, "submitted from this account").await?;
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
    let prepared =
        prepare(state, client, "wallet_strk20InvokeTransaction", &ctx, &intent, Proving::Local, &submission).await?;
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
    fn pool_defaults_and_override() {
        let mut cfg = prover::Strk20Config::default();
        assert_eq!(pool_for(&cfg, ChainId::Mainnet).unwrap(), felt(MAINNET_POOL));
        assert_eq!(pool_for(&cfg, ChainId::Sepolia).unwrap(), felt(SEPOLIA_POOL));
        cfg.pool = "0x123".into();
        assert_eq!(pool_for(&cfg, ChainId::Sepolia).unwrap(), Felt::from(0x123u64));
    }
}
