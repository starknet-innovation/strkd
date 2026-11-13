//! The STRK20 plan step (`docs/project/design-48-strk20-external-owner.md`
//! §5.1–5.4, §5.6): `companion_strk20Plan` and `companion_strk20DiscardPlan`.
//!
//! A plan fixes a batch's `client_actions` (every index, channel key, salt and
//! random value) without signing or proving it, and returns `plan_hash`: the
//! SNIP-12 digest an owner contract approves on-chain before the proof is built
//! (`companion_strk20Prove`, [`super::prove`]). Plans work for enrolled owners and
//! for strkd accounts (§2.1); for an account, strkd signs at prove time.
//!
//! Plans live in a [`PlanStore`] held by the unlocked session: in memory only,
//! bound to the client that made them, capped, expiring, and dropped (with a
//! best-effort wipe of their felts) on lock, discard, expiry and restart. They
//! hold channel keys, so nothing here derives `Debug`, and the request log
//! redacts revealed actions (`dispatch.rs`, `redact_for_log`).

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};
use strk20::actions::{serialize_actions, ClientAction};
use strk20::discovery;
use strk20::hashes::poseidon_hash_many;
use strk20::planner::{Intent, State};
use wallet_core::{address_hex, AccountRef, ChainId, Felt};

use super::{
    ctx, describe, has_deposit, hex, intent_of, invalid, map_strk20, parse_actions,
    plan_at_reference, pool_fee, prover_identity, where_proved, Caller, Ctx, Planned, Proving,
    Subject, STRK,
};
use crate::dispatch::{chain_name, param_str, prover_or_err, ServerState};
use crate::error::WalletRpcError;
use crate::log::now_unix_ms;

/// The plan format. Bumped together with the SNIP-12 domain `version` (§5.3).
pub const PLAN_VERSION: u64 = 1;
/// SNIP-12 domain of the approval hash (§5.3).
pub const PLAN_DOMAIN_NAME: &str = "strkd.Strk20Plan";
pub const PLAN_DOMAIN_VERSION: &str = "1";
/// Plan lifetime: 1 h by default, 24 h at most (D2).
pub const DEFAULT_TTL_SECS: u64 = 3600;
pub const MAX_TTL_SECS: u64 = 86_400;
/// Caps (§5.4). One live plan per (subject, chain, pool) is checked separately.
pub const MAX_PLANS_PER_CLIENT: usize = 4;
pub const MAX_PLANS_TOTAL: usize = 32;

const METHOD: &str = "companion_strk20Plan";
/// The method label of the separate `reveal_actions` prompt.
const REVEAL_METHOD: &str = "companion_strk20Plan (reveal_actions)";

// ── the approval hash (§5.3) ─────────────────────────────────────────────────

/// A plan's SNIP-12 approval hash and the typed data behind it.
pub struct PlanHash {
    /// `poseidon_hash_many(client_actions_felts)`.
    pub actions_hash: Felt,
    /// The full SNIP-12 object: `companion_typedDataHash` with
    /// `account_address = owner` reproduces `plan_hash`.
    pub typed_data: Value,
    pub plan_hash: Felt,
}

/// The `Strk20Plan` typed data for `(chain, pool, owner, actions_hash)`.
///
/// The domain's `"version": "1"` encodes as the felt 1 (digit strings are
/// numbers in krusty and starknet.js), not the shortstring `0x31`; the Cairo
/// side must use `1` (PR #53's vector pins this).
pub fn plan_typed_data(chain: ChainId, pool: &Felt, owner: &Felt, actions_hash: &Felt) -> Value {
    json!({
        "types": {
            "StarknetDomain": [
                { "name": "name", "type": "shortstring" },
                { "name": "version", "type": "shortstring" },
                { "name": "chainId", "type": "shortstring" },
                { "name": "revision", "type": "shortstring" }
            ],
            "Strk20Plan": [
                { "name": "pool", "type": "ContractAddress" },
                { "name": "owner", "type": "ContractAddress" },
                { "name": "actions_hash", "type": "felt" }
            ]
        },
        "primaryType": "Strk20Plan",
        "domain": {
            "name": PLAN_DOMAIN_NAME,
            "version": PLAN_DOMAIN_VERSION,
            "chainId": chain_name(chain),
            "revision": "1"
        },
        "message": {
            "pool": address_hex(pool),
            "owner": address_hex(owner),
            "actions_hash": address_hex(actions_hash)
        }
    })
}

/// The approval hash of a batch. `client_actions_felts` is the whole
/// `serialize_actions` output, `Span` length prefix included: the pool's
/// `compile_actions` calldata from index 2 on (after `owner` and `user_sk`).
/// The SNIP-12 account address is the owner. This is the only function that
/// computes it: the plan handler and the vector test both go through it.
pub fn plan_hash(
    chain: ChainId,
    pool: &Felt,
    owner: &Felt,
    client_actions_felts: &[Felt],
) -> Result<PlanHash, WalletRpcError> {
    let actions_hash = poseidon_hash_many(client_actions_felts);
    let typed_data = plan_typed_data(chain, pool, owner, &actions_hash);
    let plan_hash = wallet_core::typed_data_message_hash(&typed_data.to_string(), owner)?;
    Ok(PlanHash {
        actions_hash,
        typed_data,
        plan_hash,
    })
}

// ── stored plans ─────────────────────────────────────────────────────────────

/// A plan as the store keeps it. No `Debug`: it holds channel keys.
pub(super) struct StoredPlan {
    pub(super) id: String,
    /// The paired client that made it. Only that client can see, discard or
    /// prove it.
    pub(super) client_id: String,
    pub(super) subject: Subject,
    /// The strkd account whose key derives `user_sk`: the subject itself, or
    /// the owner's controller. It signs at prove time only for an account.
    pub(super) account: AccountRef,
    /// The pool's `user`: the owner or the account address.
    pub(super) user: Felt,
    pub(super) chain: ChainId,
    pub(super) pool: Felt,
    /// The reference block the plan was discovered at. Prove needs `B ≥ plan_block`.
    pub(super) plan_block: u64,
    pub(super) intent: Intent,
    /// The planned actions, randomness included. Never re-planned.
    pub(super) actions: Vec<ClientAction>,
    /// `serialize_actions(actions)`: what `plan_hash` commits to.
    pub(super) client_actions_felts: Vec<Felt>,
    pub(super) plan_hash: Felt,
    /// The state discovered at `plan_block`, with `user_sk` zeroed. Prove
    /// compares a fresh discovery against it (`same_state`, §5.5).
    pub(super) state: State,
    /// The prover (its kind, and a remote prover's URL) and submission route
    /// the plan prompt named. Prove prompts again if either changes (§5.6).
    pub(super) prover_id: String,
    pub(super) submission: &'static str,
    /// The plan prompt, as shown.
    pub(super) prompt: String,
    pub(super) created_at_ms: u64,
    pub(super) expires_at_ms: u64,
}

/// Overwrite a felt so its value does not linger in freed memory. Best
/// effort: no `unsafe`, `black_box` keeps the store from being optimised out.
fn wipe(f: &mut Felt) {
    *f = Felt::ZERO;
    std::hint::black_box(f);
}

fn wipe_action(a: &mut ClientAction) {
    match a {
        ClientAction::SetViewingKey { random } => wipe(random),
        ClientAction::OpenChannel {
            recipient_addr,
            index,
            random,
            salt,
        } => {
            wipe(recipient_addr);
            wipe(random);
            wipe(salt);
            *index = 0;
        }
        ClientAction::OpenSubchannel {
            recipient_addr,
            recipient_public_key,
            channel_key,
            index,
            token,
            salt,
        } => {
            for f in [
                recipient_addr,
                recipient_public_key,
                channel_key,
                token,
                salt,
            ] {
                wipe(f);
            }
            *index = 0;
        }
        ClientAction::CreateEncNote {
            recipient_addr,
            recipient_public_key,
            token,
            amount,
            index,
            salt,
        } => {
            for f in [recipient_addr, recipient_public_key, token] {
                wipe(f);
            }
            *amount = 0;
            *index = 0;
            *salt = 0;
            std::hint::black_box(salt);
        }
        ClientAction::Deposit { token, amount } => {
            wipe(token);
            *amount = 0;
        }
        ClientAction::UseNote {
            channel_key,
            token,
            index,
        } => {
            wipe(channel_key);
            wipe(token);
            *index = 0;
        }
        ClientAction::Withdraw {
            to_addr,
            token,
            amount,
            random,
        } => {
            wipe(to_addr);
            wipe(token);
            wipe(random);
            *amount = 0;
        }
    }
}

impl StoredPlan {
    fn wipe(&mut self) {
        self.actions.iter_mut().for_each(wipe_action);
        self.client_actions_felts.iter_mut().for_each(wipe);
        wipe(&mut self.state.user_sk);
        for n in &mut self.state.notes {
            wipe(&mut n.channel_key);
            wipe(&mut n.id);
        }
        for c in self.state.channels.values_mut() {
            wipe(&mut c.key);
        }
    }
}

impl Drop for StoredPlan {
    fn drop(&mut self) {
        self.wipe();
    }
}

/// `PLAN_NOT_FOUND` (114): unknown, expired, discarded, consumed, wiped by a
/// lock, or another client's. One answer for all of them.
fn plan_not_found() -> WalletRpcError {
    WalletRpcError::InvalidRequest(
        "no such plan for this client (unknown, expired, discarded, consumed, or wiped when the wallet locked)".into(),
    )
    .with_reason("PLAN_NOT_FOUND")
}

fn plan_limit(m: String) -> WalletRpcError {
    WalletRpcError::Precondition(m).with_reason("PLAN_LIMIT")
}

/// In-memory plans, keyed by `plan_id` (§5.4). Lives inside the unlocked
/// session, so a lock drops it whole. Expired plans are swept whenever the
/// store is touched.
#[derive(Default)]
pub(crate) struct PlanStore {
    plans: HashMap<String, StoredPlan>,
}

impl PlanStore {
    fn sweep(&mut self, now_ms: u64) {
        self.plans.retain(|_, p| p.expires_at_ms > now_ms);
    }

    /// Refuse a new plan for `(user, chain, pool)` by `client` if it would
    /// break a cap: one live plan per client, subject and pool (a client's
    /// concurrent plans would claim the same slots), [`MAX_PLANS_PER_CLIENT`],
    /// [`MAX_PLANS_TOTAL`].
    ///
    /// The one-plan cap is per client, so one client can't block another's
    /// planning (or learn that it has a plan). Two clients' plans for the same
    /// subject race on the same notes: whichever is proved and lands second
    /// fails, with `PLAN_STALE` at prove time or on-chain (nullifiers are
    /// single-use), and loses nothing but the pool fee if it got that far.
    pub(super) fn check_caps(
        &mut self,
        client: &str,
        user: &Felt,
        chain: ChainId,
        pool: &Felt,
        now_ms: u64,
    ) -> Result<(), WalletRpcError> {
        self.sweep(now_ms);
        if let Some(p) = self.plans.values().find(|p| {
            p.client_id == client && p.user == *user && p.chain == chain && p.pool == *pool
        }) {
            return Err(plan_limit(format!(
                "{} already has a live plan on pool {} (your plan {}: discard it with \
                 companion_strk20DiscardPlan or let it expire). Only one plan per owner and pool can be live, \
                 because concurrent plans would claim the same note slots.",
                address_hex(user),
                address_hex(pool),
                p.id
            )));
        }
        if self
            .plans
            .values()
            .filter(|p| p.client_id == client)
            .count()
            >= MAX_PLANS_PER_CLIENT
        {
            return Err(plan_limit(format!(
                "this client already has {MAX_PLANS_PER_CLIENT} live plans: discard one first"
            )));
        }
        if self.plans.len() >= MAX_PLANS_TOTAL {
            return Err(plan_limit(format!(
                "strkd already holds {MAX_PLANS_TOTAL} live plans: discard one or let them expire"
            )));
        }
        Ok(())
    }

    /// Add `plan`, re-checking the caps (another request may have planned
    /// while this one was prompting).
    pub(super) fn insert(&mut self, plan: StoredPlan, now_ms: u64) -> Result<(), WalletRpcError> {
        self.check_caps(&plan.client_id, &plan.user, plan.chain, &plan.pool, now_ms)?;
        self.plans.insert(plan.id.clone(), plan);
        Ok(())
    }

    /// The live plan `id`, if `client` made it.
    pub(super) fn get(
        &mut self,
        id: &str,
        client: &str,
        now_ms: u64,
    ) -> Result<&StoredPlan, WalletRpcError> {
        self.sweep(now_ms);
        self.plans
            .get(id)
            .filter(|p| p.client_id == client)
            .ok_or_else(plan_not_found)
    }

    /// Remove and return the live plan `id`, if `client` made it. Discard
    /// drops it (wiping it); prove consumes it once its job is queued.
    pub(super) fn take(
        &mut self,
        id: &str,
        client: &str,
        now_ms: u64,
    ) -> Result<StoredPlan, WalletRpcError> {
        self.get(id, client, now_ms)?;
        self.plans.remove(id).ok_or_else(plan_not_found)
    }
}

// ── the handlers ─────────────────────────────────────────────────────────────

/// The id plans are bound to.
pub(super) fn client_id(caller: Caller<'_>) -> String {
    match caller {
        Caller::Rpc(c) => c.id.clone(),
        Caller::Desktop => "desktop".into(),
    }
}

fn flag(params: &Value, key: &str) -> Result<bool, WalletRpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid(format!("{key}: expected a boolean"))),
    }
}

fn ttl_secs(params: &Value) -> Result<u64, WalletRpcError> {
    match params.get("ttl_secs") {
        None | Some(Value::Null) => Ok(DEFAULT_TTL_SECS),
        Some(v) => v
            .as_u64()
            .filter(|t| (1..=MAX_TTL_SECS).contains(t))
            .ok_or_else(|| invalid(format!("ttl_secs: an integer from 1 to {MAX_TTL_SECS}"))),
    }
}

/// A 128-bit random plan id, as 32 hex digits.
fn new_plan_id() -> Result<String, WalletRpcError> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b)
        .map_err(|_| WalletRpcError::Unknown("randomness unavailable".into()))?;
    Ok(::hex::encode(b))
}

fn amount(a: u128) -> String {
    format!("{a:#x}")
}

/// A client action as typed JSON (the `reveal_actions` view).
fn action_json(a: &ClientAction) -> Value {
    match *a {
        ClientAction::SetViewingKey { random } => {
            json!({ "type": "set_viewing_key", "random": hex(&random) })
        }
        ClientAction::OpenChannel {
            recipient_addr,
            index,
            random,
            salt,
        } => {
            json!({ "type": "open_channel", "recipient_addr": hex(&recipient_addr), "index": index,
                      "random": hex(&random), "salt": hex(&salt) })
        }
        ClientAction::OpenSubchannel {
            recipient_addr,
            recipient_public_key,
            channel_key,
            index,
            token,
            salt,
        } => json!({ "type": "open_subchannel", "recipient_addr": hex(&recipient_addr),
                      "recipient_public_key": hex(&recipient_public_key), "channel_key": hex(&channel_key),
                      "index": index, "token": hex(&token), "salt": hex(&salt) }),
        ClientAction::CreateEncNote {
            recipient_addr,
            recipient_public_key,
            token,
            amount: a,
            index,
            salt,
        } => json!({ "type": "create_enc_note", "recipient_addr": hex(&recipient_addr),
                      "recipient_public_key": hex(&recipient_public_key), "token": hex(&token),
                      "amount": amount(a), "index": index, "salt": amount(salt) }),
        ClientAction::Deposit { token, amount: a } => {
            json!({ "type": "deposit", "token": hex(&token), "amount": amount(a) })
        }
        ClientAction::UseNote {
            channel_key,
            token,
            index,
        } => {
            json!({ "type": "use_note", "channel_key": hex(&channel_key), "token": hex(&token), "index": index })
        }
        ClientAction::Withdraw {
            to_addr,
            token,
            amount: a,
            random,
        } => {
            json!({ "type": "withdraw", "to_addr": hex(&to_addr), "token": hex(&token), "amount": amount(a),
                      "random": hex(&random) })
        }
    }
}

/// The channel keys a batch's actions carry (new subchannels and spent notes).
fn channel_keys(actions: &[ClientAction]) -> usize {
    actions
        .iter()
        .filter_map(|a| match a {
            ClientAction::OpenSubchannel { channel_key, .. }
            | ClientAction::UseNote { channel_key, .. } => Some(*channel_key),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .len()
}

/// Who the plan is for, in prompts.
fn subject_text(ctx: &Ctx) -> String {
    match &ctx.subject {
        Subject::Owner(o) => {
            let label = match o.label.as_str() {
                "" => String::new(),
                l => format!(" (caller-supplied label \"{l}\")"),
            };
            format!(
                "owner contract {}{label}, controlled by your account {}",
                ctx.address(),
                ctx.account.address
            )
        }
        Subject::Account => format!("your account {}", ctx.address()),
    }
}

fn plan_prompt(
    ctx: &Ctx,
    intent: &Intent,
    n_actions: usize,
    plan_hash: &Felt,
    where_proved: &str,
) -> String {
    let next = match ctx.subject {
        Subject::Owner(_) => format!(
            "Nothing is signed or proved now. Before the proof can be built, the owner contract must approve this \
             plan on-chain (plan_hash {}); strkd never signs for it. The proof is built later, {where_proved}, and \
             returned unsubmitted unless you choose otherwise then.",
            hex(plan_hash)
        ),
        Subject::Account => format!(
            "Nothing is signed or proved now; strkd signs with this account when the plan is proved later, \
             {where_proved}, and returns it unsubmitted unless you choose otherwise then."
        ),
    };
    format!(
        "STRK20 plan on {} for {}: {}. {n_actions} action(s), {}. {next}",
        chain_name(ctx.chain),
        subject_text(ctx),
        describe(intent),
        ctx.pool_label(),
    )
}

fn reveal_prompt(ctx: &Ctx, plan_id: &str, n_channel_keys: usize) -> String {
    format!(
        "Reveal the full action list of STRK20 plan {plan_id} on {} for {}. It contains {n_channel_keys} channel \
         key(s), plus note indices, salts and random values. Whoever holds a channel key can see every past and \
         future private payment on that channel (tokens, amounts, and the transactions that carried them), and this \
         cannot be revoked: the key is fixed for that sender and recipient. It does not let anyone spend these funds \
         or send from this address. Refusing still returns the plan, without the actions.",
        chain_name(ctx.chain),
        subject_text(ctx),
    )
}

fn summary_json(
    intent: &Intent,
    plan: &strk20::planner::Plan,
    pool: &Felt,
    pool_fee: u128,
) -> Value {
    let moves = |list: &[(Felt, Felt, u128)]| {
        list.iter()
            .map(|(r, t, a)| json!({ "token": hex(t), "amount": amount(*a), "recipient": hex(r) }))
            .collect::<Vec<_>>()
    };
    json!({
        "description": describe(intent),
        "register": intent.register,
        "transfers": moves(&intent.transfers),
        "withdrawals": moves(&intent.withdrawals),
        "action_count": plan.actions.len(),
        "notes_spent": plan.spent.len(),
        "change": plan.change.iter().map(|(t, a)| json!({ "token": hex(t), "amount": amount(*a) })).collect::<Vec<_>>(),
        "pool": address_hex(pool),
        // Charged by the pool on apply_actions, in STRK, to whoever submits it.
        "pool_fee": amount(pool_fee),
        "pool_fee_token": STRK,
    })
}

/// `companion_strk20Plan { owner_address | account_address, actions?, register?,
/// reveal_actions?, ttl_secs?, chainId? }` → `{ plan_id, plan_version,
/// expires_at, plan_block, chain_id, pool, owner, subject, controller,
/// plan_hash, actions_hash, typed_data, summary, actions_revealed?,
/// client_actions?, client_actions_felts? }`.
///
/// Plans `actions` (or `register: true`) against `head − PROVING_LAG`, prompts
/// (a grant may cover it), and stores the plan. Signs nothing, proves nothing.
/// `reveal_actions: true` adds a second prompt that a grant never covers; if
/// refused, the plan still comes back, without the actions.
pub async fn plan(
    state: &ServerState,
    caller: Caller<'_>,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let reveal = flag(params, "reveal_actions")?;
    let register = flag(params, "register")?;
    let ttl = ttl_secs(params)?;
    let intent = if register {
        if params.get("actions").is_some_and(|v| !v.is_null()) {
            return Err(invalid("pass actions or register: true, not both"));
        }
        Intent {
            register: true,
            ..Intent::default()
        }
    } else {
        if matches!(params.get("actions"), None | Some(Value::Null)) {
            return Err(invalid("pass actions, or register: true"));
        }
        let actions = parse_actions(params)?;
        if has_deposit(&actions) {
            return Err(WalletRpcError::NotImplemented(
                "deposits can't be planned yet (they need a screening prover): use wallet_strk20InvokeTransaction \
                 from a strkd account"
                    .into(),
            )
            .with_reason("DEPOSIT_UNSUPPORTED"));
        }
        intent_of(&actions)
    };
    let ctx = ctx(state, caller, params).await?;
    let prover = prover_or_err(state)?;
    let client = client_id(caller);
    state.session.lock().await.strk20_plans()?.check_caps(
        &client,
        &ctx.user,
        ctx.chain,
        &ctx.pool,
        now_unix_ms(),
    )?;

    if register {
        let registered = discovery::public_key(&ctx.reader(json!("latest")), ctx.user)
            .await
            .map_err(map_strk20)?;
        if registered.is_some() {
            return Err(WalletRpcError::Precondition(format!(
                "{} is already registered with the pool",
                ctx.address()
            ))
            .with_reason("ALREADY_REGISTERED"));
        }
    }
    let Planned {
        block,
        state: mut snapshot,
        plan,
        ..
    } = plan_at_reference(state, &ctx, &intent).await?;
    snapshot.user_sk = Felt::ZERO;
    let felts = serialize_actions(&plan.actions);
    let hashed = plan_hash(ctx.chain, &ctx.pool, &ctx.user, &felts)?;
    let fee = pool_fee(&ctx).await?;
    let where_proved = where_proved(prover, &Proving::Local);
    let prompt = plan_prompt(
        &ctx,
        &intent,
        plan.actions.len(),
        &hashed.plan_hash,
        &where_proved,
    );

    caller.approve(state, METHOD, prompt.clone()).await?;

    let id = new_plan_id()?;
    let created_at_ms = now_unix_ms();
    let expires_at_ms = created_at_ms + ttl * 1000;
    let n_channel_keys = channel_keys(&plan.actions);
    let mut out = json!({
        "plan_id": id,
        "plan_version": PLAN_VERSION,
        "expires_at": expires_at_ms,
        "plan_block": block,
        "chain_id": chain_name(ctx.chain),
        "pool": address_hex(&ctx.pool),
        "owner": ctx.address(),
        "subject": match ctx.subject { Subject::Owner(_) => "owner", Subject::Account => "account" },
        "controller": ctx.account.address,
        "plan_hash": hex(&hashed.plan_hash),
        "actions_hash": hex(&hashed.actions_hash),
        "typed_data": hashed.typed_data,
        "summary": summary_json(&intent, &plan, &ctx.pool, fee),
    });
    let stored = StoredPlan {
        id: id.clone(),
        client_id: client.clone(),
        subject: ctx.subject.clone(),
        account: ctx.account.clone(),
        user: ctx.user,
        chain: ctx.chain,
        pool: ctx.pool,
        plan_block: block,
        intent,
        actions: plan.actions,
        client_actions_felts: felts,
        plan_hash: hashed.plan_hash,
        state: snapshot,
        prover_id: prover_identity(prover, ctx.chain).await,
        submission: "none",
        prompt,
        created_at_ms,
        expires_at_ms,
    };
    state
        .session
        .lock()
        .await
        .strk20_plans()?
        .insert(stored, now_unix_ms())?;

    if reveal {
        let approved = caller
            .approve_always(
                state,
                REVEAL_METHOD,
                reveal_prompt(&ctx, &id, n_channel_keys),
            )
            .await;
        match approved {
            Ok(()) => {
                let mut session = state.session.lock().await;
                let p = session.strk20_plans()?.get(&id, &client, now_unix_ms())?;
                out["client_actions"] =
                    json!(p.actions.iter().map(action_json).collect::<Vec<_>>());
                out["client_actions_felts"] =
                    json!(p.client_actions_felts.iter().map(hex).collect::<Vec<_>>());
                out["actions_revealed"] = json!(true);
            }
            Err(WalletRpcError::UserRefused) => out["actions_revealed"] = json!(false),
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// `companion_strk20DiscardPlan { plan_id }` → `true`. No prompt. Another
/// client's plan, or an unknown one, is `PLAN_NOT_FOUND`.
pub async fn discard(
    state: &ServerState,
    caller: Caller<'_>,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let id = param_str(params, "plan_id")?;
    let client = client_id(caller);
    let plan = state
        .session
        .lock()
        .await
        .strk20_plans()?
        .take(&id, &client, now_unix_ms())?;
    drop(plan);
    Ok(json!(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> Felt {
        Felt::from_hex(s).unwrap()
    }

    /// PR #53's SNIP-12 vector (`crates/wallet-core/tests/fixtures/strk20-plan-vector.json`),
    /// through the same function the plan handler uses.
    #[test]
    fn plan_hash_matches_the_snip12_vector() {
        let pool = f("0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e");
        let owner = f("0x0712d1ab5b6e5d3e3c0b2a1f9e8d7c6b5a4938271605f4e3d2c1b0a998877665");
        let felts: Vec<Felt> = ["0x2", "0x7", "0x2a", "0x3", "0x1"]
            .iter()
            .map(|s| f(s))
            .collect();
        let h = plan_hash(ChainId::Sepolia, &pool, &owner, &felts).unwrap();
        assert_eq!(
            h.actions_hash,
            f("0x03342f4106e584ae6fa004f9a518687fa0160dfd61720b8aae0edf25fe89f84a")
        );
        assert_eq!(
            h.plan_hash,
            f("0x062a76d34a73ac201527a84ddcf018608bf9443c3daf8597c539304ff5ce3301")
        );
        // The typed data is the vector's, field for field.
        let td = &h.typed_data;
        assert_eq!(td["primaryType"], json!("Strk20Plan"));
        assert_eq!(
            td["domain"],
            json!({ "name": "strkd.Strk20Plan", "version": "1", "chainId": "SN_SEPOLIA", "revision": "1" })
        );
        assert_eq!(
            td["types"]["Strk20Plan"],
            json!([
                { "name": "pool", "type": "ContractAddress" },
                { "name": "owner", "type": "ContractAddress" },
                { "name": "actions_hash", "type": "felt" }
            ])
        );
        assert_eq!(
            td["message"]["actions_hash"],
            json!("0x03342f4106e584ae6fa004f9a518687fa0160dfd61720b8aae0edf25fe89f84a")
        );
    }

    /// The hash binds the chain, pool, owner and every action felt.
    #[test]
    fn plan_hash_binds_chain_pool_owner_and_actions() {
        let (pool, owner) = (f("0x123"), f("0xc0ffee"));
        let felts = vec![Felt::ONE, Felt::from(7u64)];
        let base = plan_hash(ChainId::Sepolia, &pool, &owner, &felts)
            .unwrap()
            .plan_hash;
        assert_ne!(
            base,
            plan_hash(ChainId::Mainnet, &pool, &owner, &felts)
                .unwrap()
                .plan_hash
        );
        assert_ne!(
            base,
            plan_hash(ChainId::Sepolia, &f("0x124"), &owner, &felts)
                .unwrap()
                .plan_hash
        );
        assert_ne!(
            base,
            plan_hash(ChainId::Sepolia, &pool, &f("0xc0ffef"), &felts)
                .unwrap()
                .plan_hash
        );
        let other = vec![Felt::ONE, Felt::from(8u64)];
        assert_ne!(
            base,
            plan_hash(ChainId::Sepolia, &pool, &owner, &other)
                .unwrap()
                .plan_hash
        );
    }

    fn stored(id: &str, client: &str, user: u64, expires_at_ms: u64) -> StoredPlan {
        StoredPlan {
            id: id.into(),
            client_id: client.into(),
            subject: Subject::Account,
            account: AccountRef {
                domain: wallet_core::Domain::User,
                index: 0,
                address: "0x1".into(),
                label: String::new(),
                contract: Default::default(),
                owner_client_id: None,
            },
            user: Felt::from(user),
            chain: ChainId::Sepolia,
            pool: f("0x123"),
            plan_block: 1,
            intent: Intent::default(),
            actions: vec![ClientAction::UseNote {
                channel_key: f("0xc4a77e1"),
                token: f("0x4718"),
                index: 3,
            }],
            client_actions_felts: vec![Felt::ONE],
            plan_hash: Felt::ONE,
            state: State {
                user: Felt::from(user),
                user_sk: Felt::ZERO,
                registered: true,
                outgoing_channels: 0,
                channels: Default::default(),
                notes: Vec::new(),
            },
            prover_id: "native".into(),
            submission: "none",
            prompt: String::new(),
            created_at_ms: 0,
            expires_at_ms,
        }
    }

    fn reason(e: WalletRpcError) -> Option<&'static str> {
        e.reason()
    }

    #[test]
    fn store_binds_plans_to_their_client_and_expires_them() {
        let mut s = PlanStore::default();
        s.insert(stored("a", "c1", 1, 1000), 0).unwrap();
        assert!(s.get("a", "c1", 10).is_ok());
        assert_eq!(
            s.get("a", "c2", 10).err().and_then(reason),
            Some("PLAN_NOT_FOUND")
        );
        assert_eq!(
            s.take("a", "c2", 10).err().and_then(reason),
            Some("PLAN_NOT_FOUND")
        );
        // Expired at 1000: swept, and the slot is free again.
        assert_eq!(
            s.get("a", "c1", 1000).err().and_then(reason),
            Some("PLAN_NOT_FOUND")
        );
        assert!(s.plans.is_empty());
        s.insert(stored("b", "c1", 1, 5000), 1000).unwrap();
        assert!(s.take("b", "c1", 1001).is_ok());
        assert_eq!(
            s.take("b", "c1", 1001).err().and_then(reason),
            Some("PLAN_NOT_FOUND")
        );
    }

    #[test]
    fn store_caps_one_per_subject_four_per_client_thirty_two_in_all() {
        let mut s = PlanStore::default();
        s.insert(stored("a", "c1", 1, u64::MAX), 0).unwrap();
        // Same subject and pool, same client.
        let e = s.insert(stored("x", "c1", 1, u64::MAX), 0).unwrap_err();
        assert_eq!(e.reason(), Some("PLAN_LIMIT"));
        assert_eq!(e.code(), -32006);
        // Another client is neither blocked nor told about c1's plan.
        s.insert(stored("y", "c2", 1, u64::MAX), 0).unwrap();
        let e = s.insert(stored("y2", "c2", 1, u64::MAX), 0).unwrap_err();
        assert!(e.message().contains("your plan y:") && !e.message().contains("\"a\"") && !e.message().contains(" a:"), "{}", e.message());
        assert!(s.take("y", "c2", 0).is_ok());
        for i in 2..=4 {
            s.insert(stored(&format!("a{i}"), "c1", i, u64::MAX), 0)
                .unwrap();
        }
        let e = s.insert(stored("a5", "c1", 5, u64::MAX), 0).unwrap_err();
        assert!(e.message().contains("4 live plans"), "{}", e.message());
        for i in 5..MAX_PLANS_TOTAL as u64 + 1 {
            let client = format!("k{}", i / 4);
            s.insert(stored(&format!("p{i}"), &client, 100 + i, u64::MAX), 0)
                .unwrap();
        }
        assert_eq!(s.plans.len(), MAX_PLANS_TOTAL);
        let e = s
            .insert(stored("z", "fresh", 9999, u64::MAX), 0)
            .unwrap_err();
        assert!(e.message().contains("32 live plans"), "{}", e.message());
    }

    #[test]
    fn dropping_a_plan_wipes_its_felts() {
        let mut p = stored("a", "c1", 1, 1);
        p.wipe();
        assert!(
            matches!(p.actions[0], ClientAction::UseNote { channel_key, .. } if channel_key == Felt::ZERO)
        );
        assert_eq!(p.client_actions_felts, vec![Felt::ZERO]);
    }
}
