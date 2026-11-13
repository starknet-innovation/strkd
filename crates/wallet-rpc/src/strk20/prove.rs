//! The STRK20 prove step (`docs/project/design-48-strk20-external-owner.md`
//! §5.1, §5.2, §5.5–5.7): `companion_strk20Prove`.
//!
//! Prove takes a stored plan (see [`super::plan`]) and proves it against a
//! reference block `B` at or after the owner's on-chain approval. Every check
//! runs synchronously, so a bad request fails at once with a `data.reason`:
//! the plan (client-bound), the subject still in the caller's scope, the
//! bounds on `B`, deployment and SRC5 at `B`, a fresh discovery at `B` and at
//! `latest` against the plan's snapshot, and the plan hash recomputed from the
//! stored actions. Then the plan is consumed and an **unrecorded** proving job
//! is queued ([`prover::enqueue_prove_unrecorded`]): nothing about it reaches
//! proof storage, only the calling client can poll it, and its result is
//! dropped an hour after it finishes or when the wallet locks.
//!
//! strkd never signs for an owner contract: the virtual transaction carries
//! the caller's `signature` felts as given. For a strkd account, strkd signs
//! the invocation hash with the account's key, as the one-shot methods do.

use serde_json::{json, Value};
use strk20::actions::serialize_actions;
use strk20::invocation::ProofInvocation;
use strk20::planner::{Intent, State};
use wallet_core::{address_hex, get_selector_from_name, AccountRef, ChainId, Felt};

use super::plan::{client_id, plan_hash, StoredPlan};
use super::{
    block_number, call_and_proof, chain_name, ctx, describe, discover_state, ensure_deployed_at,
    hex, invalid, map_strk20, node_err, notes_json, owners, pool_fee_at, prover_identity, remote,
    safe_prover_error, same_state, scrub, submit_from, where_proved, Caller, Ctx, Prepared,
    Proving, Subject, SubmitGuard, SubmitTarget, PROVING_LAG,
};
use crate::dispatch::{normalize_address, param_str, prover_network, prover_or_err, ServerState};
use crate::error::WalletRpcError;
use crate::log::now_unix_ms;

const METHOD: &str = "companion_strk20Prove";
/// Blocks kept in hand when checking the pool's proof window: proving and
/// inclusion take time, and the proof must land within
/// `get_proof_validity_blocks` of `B` (§5.5).
pub const PROOF_WINDOW_MARGIN: u64 = 120;
/// Caller signatures are at most this many felts.
pub const MAX_SIGNATURE_FELTS: usize = 256;

/// Where the proved batch goes (D5).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Submit {
    /// Return `{ call, proof }` for the caller to submit.
    None,
    /// Submit from a strkd account in the caller's scope.
    Account,
}

impl Submit {
    fn as_str(self) -> &'static str {
        match self {
            Submit::None => "none",
            Submit::Account => "account",
        }
    }
}

/// What prove needs from a stored plan, without its secrets.
struct Meta {
    subject: Subject,
    account: AccountRef,
    user: Felt,
    chain: ChainId,
    pool: Felt,
    plan_block: u64,
    intent: Intent,
    prover_id: String,
    submission: &'static str,
    plan_hash: Felt,
    /// The plan's transfer-note locators (#49), returned in the job result.
    notes: Vec<strk20::planner::NoteLocator>,
    /// The plan prompt as the user saw it, quoted when prove prompts again.
    plan_prompt: String,
    created_at_ms: u64,
}

impl Meta {
    fn of(p: &StoredPlan) -> Self {
        Meta {
            subject: p.subject.clone(),
            account: p.account.clone(),
            user: p.user,
            chain: p.chain,
            pool: p.pool,
            plan_block: p.plan_block,
            intent: p.intent.clone(),
            prover_id: p.prover_id.clone(),
            submission: p.submission,
            plan_hash: p.plan_hash,
            notes: p.notes.clone(),
            plan_prompt: p.prompt.clone(),
            created_at_ms: p.created_at_ms,
        }
    }

    fn is_owner(&self) -> bool {
        matches!(self.subject, Subject::Owner(_))
    }
}

// ── params ───────────────────────────────────────────────────────────────────

fn signature_param(params: &Value) -> Result<Option<Vec<Felt>>, WalletRpcError> {
    match params.get("signature") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(list)) => {
            if list.len() > MAX_SIGNATURE_FELTS {
                return Err(invalid(format!(
                    "signature: at most {MAX_SIGNATURE_FELTS} felts"
                )));
            }
            list.iter()
                .map(|v| {
                    v.as_str()
                        .and_then(|s| Felt::from_hex(s).ok())
                        .ok_or_else(|| invalid("signature: expected an array of hex felts"))
                })
                .collect::<Result<_, _>>()
                .map(Some)
        }
        Some(_) => Err(invalid("signature: expected an array of hex felts")),
    }
}

fn block_param(params: &Value) -> Result<Option<u64>, WalletRpcError> {
    match params.get("block_number") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid("block_number: expected a non-negative integer")),
    }
}

fn submit_param(params: &Value) -> Result<Submit, WalletRpcError> {
    match params.get("submit") {
        None | Some(Value::Null) => Ok(Submit::None),
        Some(v) => match v.as_str() {
            Some("none") => Ok(Submit::None),
            Some("account") => Ok(Submit::Account),
            Some("relay") => Err(WalletRpcError::NotImplemented(
                "submit: \"relay\" (the AVNU private relay) is not available for plans yet: use \"none\" and \
                 submit the returned call and proof yourself, or \"account\""
                    .into(),
            )
            .with_reason("RELAY_UNSUPPORTED")),
            _ => Err(invalid("submit: \"none\" or \"account\"")),
        },
    }
}

// ── the reference block (§5.5 step 2) ────────────────────────────────────────

/// Pick and check the reference block `B`. `head` is the chain head,
/// `requested` the caller's `block_number`, `validity` the pool's
/// `get_proof_validity_blocks`.
///
/// - `B ≤ head − PROVING_LAG`, else `BLOCK_TOO_RECENT` (with the blocks to wait).
///   Without `block_number`, `B = head − PROVING_LAG`, which must still be
///   `≥ plan_block`.
/// - `B ≥ plan_block`, else `BLOCK_BEFORE_PLAN`.
/// - `head − B < validity − PROOF_WINDOW_MARGIN`, else `BLOCK_TOO_OLD`.
fn reference_block(
    head: u64,
    requested: Option<u64>,
    plan_block: u64,
    validity: u64,
) -> Result<u64, WalletRpcError> {
    let newest = head.saturating_sub(PROVING_LAG);
    let too_recent = |b: u64| {
        // `b` can be any caller-supplied u64.
        let wait = b.saturating_add(PROVING_LAG).saturating_sub(head);
        WalletRpcError::Precondition(format!(
            "reference block {b} is less than {PROVING_LAG} blocks behind the chain head ({head}): proofs read \
             state at least {PROVING_LAG} blocks back. Retry in {wait} block(s)."
        ))
        .with_reason("BLOCK_TOO_RECENT")
    };
    let block = match requested {
        None if newest < plan_block || head < PROVING_LAG => return Err(too_recent(plan_block)),
        None => newest,
        Some(b) if b > newest || head < PROVING_LAG => return Err(too_recent(b)),
        Some(b) if b < plan_block => {
            return Err(WalletRpcError::Precondition(format!(
                "reference block {b} is before the plan's block {plan_block}: prove at or after the block the \
                 plan was discovered at"
            ))
            .with_reason("BLOCK_BEFORE_PLAN"))
        }
        Some(b) => b,
    };
    let max_age = validity.saturating_sub(PROOF_WINDOW_MARGIN);
    if head - block >= max_age {
        let oldest = head.saturating_sub(max_age) + 1;
        return Err(WalletRpcError::Precondition(format!(
            "reference block {block} is {} blocks behind the chain head ({head}); the pool accepts a proof for \
             {validity} blocks after its reference block, and strkd keeps {PROOF_WINDOW_MARGIN} in hand for proving \
             and inclusion. Use a block_number from {oldest} on (or omit it).",
            head - block
        ))
        .with_reason("BLOCK_TOO_OLD"));
    }
    Ok(block)
}

/// The pool's `get_proof_validity_blocks` at `latest`.
async fn proof_validity_blocks(ctx: &Ctx) -> Result<u64, WalletRpcError> {
    let out = super::PoolReader::call(
        &ctx.reader(json!("latest")),
        get_selector_from_name("get_proof_validity_blocks"),
        vec![],
    )
    .await
    .map_err(map_strk20)?;
    let v = out.first().copied().unwrap_or(Felt::ZERO);
    u64::try_from(v).map_err(|_| node_err("get_proof_validity_blocks exceeds u64"))
}

// ── checks against the stored plan ───────────────────────────────────────────

fn stale(m: String) -> WalletRpcError {
    WalletRpcError::Precondition(m).with_reason("PLAN_STALE")
}

/// The plan's snapshot still matches fresh discoveries at `B` and `latest`
/// (strict `same_state`, phase 1), and its hash still recomputes from its
/// actions (§5.5 steps 1 and 4).
fn verify_plan(p: &StoredPlan, at_block: &State, at_latest: &State) -> Result<(), WalletRpcError> {
    if !same_state(&p.state, at_block) || !same_state(&p.state, at_latest) {
        return Err(stale(format!(
            "the private state of {} changed since the plan was made (a note was spent or received, or a channel \
             or slot it uses was taken), so the planned batch would revert. The plan was discarded: make a new \
             plan and have it approved again.",
            address_hex(&p.user)
        )));
    }
    let felts = serialize_actions(&p.actions);
    let rehashed = plan_hash(p.chain, &p.pool, &p.user, &felts)?.plan_hash;
    if felts != p.client_actions_felts || rehashed != p.plan_hash {
        return Err(WalletRpcError::Unknown(
            "the stored plan's actions no longer hash to its plan_hash; it was discarded. Make a new plan.".into(),
        )
        .with_reason("PLAN_HASH_MISMATCH"));
    }
    Ok(())
}

/// The plan's subject, resolved again in the caller's scope now: an owner
/// removed since (or re-enrolled under another controller) is
/// `OWNER_NOT_ENROLLED`; an account no longer in scope is -32002.
async fn resolve_now(
    state: &ServerState,
    caller: Caller<'_>,
    meta: &Meta,
) -> Result<Ctx, WalletRpcError> {
    let mut params = json!({ "chainId": format!("{:#x}", meta.chain.as_felt()) });
    match meta.subject {
        Subject::Owner(_) => params["owner_address"] = json!(address_hex(&meta.user)),
        Subject::Account => params["account_address"] = json!(meta.account.address),
    }
    let ctx = ctx(state, caller, &params).await?;
    let same_account = ctx.account.address == meta.account.address
        && ctx.account.domain == meta.account.domain
        && ctx.account.index == meta.account.index;
    if !same_account || ctx.user != meta.user {
        return Err(WalletRpcError::Forbidden.with_reason("OWNER_NOT_ENROLLED"));
    }
    if ctx.pool != meta.pool {
        return Err(stale(format!(
            "the STRK20 pool setting for {} changed since the plan was made (plan: {}, now: {}). Make a new plan.",
            chain_name(meta.chain),
            address_hex(&meta.pool),
            address_hex(&ctx.pool)
        )));
    }
    Ok(ctx)
}

/// The strkd account that submits for `submit: "account"`: `submitter_address`
/// (in the caller's scope), else the plan's account (an owner's controller).
async fn resolve_submitter(
    state: &ServerState,
    caller: Caller<'_>,
    requested: Option<String>,
    ctx: &Ctx,
) -> Result<AccountRef, WalletRpcError> {
    let Some(addr) = requested else {
        return Ok(ctx.account.clone());
    };
    let want = normalize_address(&addr)?;
    let session = state.session.lock().await;
    let reg = session.registry()?;
    let scope = caller.scope();
    let found = reg
        .scoped_for(scope.as_deref())
        .find(|a| a.address == want)
        .cloned();
    found.ok_or(WalletRpcError::Forbidden)
}

// ── prompts (§5.6) ───────────────────────────────────────────────────────────

fn prove_prompt(
    plan_id: &str,
    ctx: &Ctx,
    meta: &Meta,
    prover_id: &str,
    where_proved: &str,
    submitter: Option<&AccountRef>,
    pool_fee: u128,
) -> String {
    let who = match &ctx.subject {
        Subject::Owner(_) => format!("owner contract {}", ctx.address()),
        Subject::Account => format!("your account {}", ctx.address()),
    };
    let mut text = format!(
        "Prove STRK20 plan {plan_id} on {} for {who}: {} ({}). It will be {where_proved}.",
        chain_name(ctx.chain),
        describe(&meta.intent),
        ctx.pool_label(),
    );
    if prover_id != meta.prover_id {
        text.push_str(&format!(
            " The prover changed since the plan was approved (then: {}, now: {prover_id}).",
            meta.prover_id
        ));
    }
    match submitter {
        None => text.push_str(" The call and proof are returned to the caller unsubmitted."),
        Some(s) => {
            text.push_str(&format!(
                " Then it is submitted from your account {}, which signs and pays the gas plus the pool's fee \
                 ({pool_fee} base units of STRK).",
                s.address
            ));
            if s.address != ctx.address() {
                text.push_str(&format!(
                    " Submitting links {} publicly to {who} and the pool.",
                    s.address
                ));
            } else {
                text.push_str(" The submission is public: it shows this account using the pool.");
            }
        }
    }
    text.push_str(&format!(
        " (The plan as you approved it: \"{}\")",
        meta.plan_prompt
    ));
    text
}

// ── job outcome ──────────────────────────────────────────────────────────────

fn failure(reason: &str, message: String) -> prover::JobFailure {
    prover::JobFailure {
        reason: Some(reason.to_string()),
        message: format!("{reason}: {message}"),
    }
}

/// A prover error as a job failure: a reason and fixed text, never the
/// prover's own output (see [`safe_prover_error`]).
fn prover_failure(err: &str, owner: bool, block: u64) -> prover::JobFailure {
    let (reason, message) = safe_prover_error(err, owner, block);
    let message = match reason {
        "OWNER_SIGNATURE_REJECTED" | "INVALID_SIGNATURE" => {
            format!("{message}. The plan was consumed: make a new plan.")
        }
        _ => message,
    };
    failure(reason, message)
}

// ── the handler ──────────────────────────────────────────────────────────────

/// `companion_strk20Prove { plan_id, block_number?, signature?, submit?:
/// "none"|"account", submitter_address? }` → `{ job_id, status: "queued",
/// plan_id, block_number, subject, submit, submitter_address?, next }`.
///
/// Checks everything up front, prompts only if the prover kind changed since
/// the plan's prompt or `submit` is `"account"`, consumes the plan, and queues
/// an unrecorded proving job. Poll `companion_proveStatus { job_id }`: on
/// success `result` is `{ call, proof, notes }` (`submit: "none"`) or
/// `{ transaction_hash, submitter_address, call, notes }` (`"account"`), where
/// `notes` locates the plan's transfer notes (#49, no key).
pub async fn prove(
    state: &ServerState,
    caller: Caller<'_>,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let plan_id = param_str(params, "plan_id")?;
    let signature = signature_param(params)?;
    let requested_block = block_param(params)?;
    let submit = submit_param(params)?;
    let submitter_param = super::address_param(params, "submitter_address")?;
    if submitter_param.is_some() && submit != Submit::Account {
        return Err(invalid("submitter_address needs submit: \"account\""));
    }
    let prover = prover_or_err(state)?;
    let client = client_id(caller);

    // 1. The plan: live, this client's.
    let meta = {
        let mut session = state.session.lock().await;
        Meta::of(
            session
                .strk20_plans()?
                .get(&plan_id, &client, now_unix_ms())?,
        )
    };
    match (&meta.subject, &signature) {
        (Subject::Owner(_), None) => {
            return Err(invalid(
                "signature: required for an owner contract's plan (the felts its is_custom_signature_valid reads; \
                 pass [] if it only checks an on-chain approval). strkd never signs for an owner.",
            )
            .with_reason("SIGNATURE_REQUIRED"))
        }
        (Subject::Account, Some(_)) => {
            return Err(invalid(
                "signature: not accepted for a strkd account's plan: strkd signs it with the account's key",
            )
            .with_reason("SIGNATURE_NOT_ALLOWED"))
        }
        _ => {}
    }

    // 2. The subject, still in scope (an owner may have been removed).
    let ctx = resolve_now(state, caller, &meta).await?;

    // 3. The reference block.
    let head = block_number(ctx.node.as_ref()).await?;
    let validity = proof_validity_blocks(&ctx).await?;
    let block = reference_block(head, requested_block, meta.plan_block, validity)?;

    // 4. Deployed and (owners) still advertising SRC5 at B.
    ensure_deployed_at(&ctx, block).await?;
    if meta.is_owner()
        && !owners::supports_custom_validation(&ctx, json!({ "block_number": block })).await?
    {
        return Err(WalletRpcError::Precondition(format!(
            "owner contract {} does not advertise SRC5 interface {} (is_custom_signature_valid) at reference block \
             {block}, so the pool would not let it approve this batch.",
            ctx.address(),
            owners::CUSTOM_SIGNATURE_INTERFACE_ID
        ))
        .with_reason("OWNER_NO_CUSTOM_VALIDATION"));
    }

    // 5. Rediscover at B and latest; the plan's snapshot must still hold.
    let user_sk = ctx.viewing_key(state).await?;
    let at_block = discover_state(
        &ctx,
        &ctx.reader(json!({ "block_number": block })),
        user_sk,
        &meta.intent,
    )
    .await
    .map_err(|e| scrub_err(e, user_sk))?;
    let at_latest = discover_state(&ctx, &ctx.reader(json!("latest")), user_sk, &meta.intent)
        .await
        .map_err(|e| scrub_err(e, user_sk))?;
    {
        let mut session = state.session.lock().await;
        let store = session.strk20_plans()?;
        let checked = verify_plan(
            store.get(&plan_id, &client, now_unix_ms())?,
            &at_block,
            &at_latest,
        );
        if let Err(e) = checked {
            // A stale or mismatched plan can never succeed: drop it.
            drop(store.take(&plan_id, &client, now_unix_ms()));
            return Err(e);
        }
    }

    // 6. The submitter, and a prompt only if the route changed (§5.6).
    let submitter = match submit {
        Submit::Account => Some(resolve_submitter(state, caller, submitter_param, &ctx).await?),
        Submit::None => None,
    };
    let prover_id = prover_identity(prover, ctx.chain).await;
    let submission = submit.as_str();
    if prover_id != meta.prover_id || submission != meta.submission {
        let fee = match submitter {
            Some(_) => pool_fee_at(&ctx.node, ctx.pool).await?,
            None => 0,
        };
        let text = prove_prompt(
            &plan_id,
            &ctx,
            &meta,
            &prover_id,
            &where_proved(prover, &Proving::Local),
            submitter.as_ref(),
            fee,
        );
        caller.approve(state, METHOD, text).await?;
    }

    // 7. The pool nonce at B, then build the invocation from the stored
    // actions (never re-planned), attach the signature, consume the plan.
    let pool_nonce = ctx
        .node
        .raw(
            "starknet_getNonce",
            json!({ "block_id": { "block_number": block }, "contract_address": hex(&ctx.pool) }),
        )
        .await
        .map_err(node_err)?;
    let pool_nonce = pool_nonce
        .as_str()
        .and_then(|s| Felt::from_hex(s).ok())
        .ok_or_else(|| node_err("getNonce: bad felt"))?;
    let tx = {
        let mut session = state.session.lock().await;
        let invocation = {
            let store = session.strk20_plans()?;
            let p = store.get(&plan_id, &client, now_unix_ms())?;
            if let Err(e) = verify_plan(p, &at_block, &at_latest) {
                drop(store.take(&plan_id, &client, now_unix_ms()));
                return Err(e);
            }
            ProofInvocation::new(p.pool, p.chain, pool_nonce, p.user, user_sk, &p.actions)
        };
        let signature = match (&meta.subject, signature) {
            // The caller's felts, verbatim. strkd never signs for an owner.
            (Subject::Owner(_), Some(sig)) => sig,
            (Subject::Account, None) => session.sign_hash_for(&meta.account, &invocation.hash())?,
            _ => unreachable!("checked above"),
        };
        drop(
            session
                .strk20_plans()?
                .take(&plan_id, &client, now_unix_ms())?,
        );
        invocation.to_rpc_json(&signature)
    };

    // 8. Queue the unrecorded job.
    let pool = ctx.pool;
    let notes = meta.notes.clone();
    let owner = meta.is_owner();
    let jobs = prover.jobs.clone();
    let scope = caller.scope();
    let route = submitter.clone().map(|s| {
        let target = SubmitTarget {
            session: state.session.clone(),
            node: ctx.node.clone(),
            chain: ctx.chain,
            pool,
            guard: None,
        };
        (target, s)
    });
    let finish = move |job_id: String, res: Result<Value, String>| async move {
        // `user_sk` stays in this task only to scrub submission errors.
        let result = res.map_err(|e| prover_failure(&e, owner, block))?;
        let proved = remote::parse_result(&result, &pool).map_err(|_| {
            failure(
                "PROVING_FAILED",
                "the prover's result could not be read (its text is withheld)".into(),
            )
        })?;
        let call = strk20::apply::apply_actions_call(pool, &proved.output, None)
            .map_err(|e| failure("PROVING_FAILED", scrub(&e.to_string(), user_sk)))?;
        let prepared = Prepared {
            call,
            proved,
            notes,
        };
        let Some((mut target, submitter)) = route else {
            let mut out = call_and_proof(&prepared, false);
            out["notes"] = notes_json(&prepared.notes);
            return Ok(out);
        };
        // Checked here, and again under the session lock right before
        // signing (a lock and a quick unlock while the node is queried).
        let guard = SubmitGuard {
            jobs: jobs.clone(),
            job_id,
            scope,
        };
        guard.check_wiped().map_err(|e| failure("WIPED_ON_LOCK", e_text(&e)))?;
        target.guard = Some(guard);
        let hash = submit_from(&target, &submitter, &prepared, &[])
            .await
            .map_err(|e| {
                failure(
                    e.reason().unwrap_or("SUBMIT_FAILED"),
                    scrub(
                        &format!(
                            "submitting from {} failed: {}",
                            submitter.address,
                            e.message()
                        ),
                        user_sk,
                    ),
                )
            })?;
        let mut out = call_and_proof(&prepared, false);
        if let Some(o) = out.as_object_mut() {
            o.remove("proof");
        }
        out["transaction_hash"] = json!(hex(&hash));
        out["submitter_address"] = json!(submitter.address);
        out["notes"] = notes_json(&prepared.notes);
        Ok(out)
    };
    let job_id = prover::enqueue_prove_unrecorded(
        prover,
        json!({ "transaction": tx, "block_number": block }),
        Some(format!("STRK20 {METHOD}")),
        prover_network(ctx.chain).to_string(),
        Some(client),
        finish,
    )
    .await;

    let mut out = json!({
        "job_id": job_id,
        "status": "queued",
        "plan_id": plan_id,
        "plan_hash": hex(&meta.plan_hash),
        "plan_created_at": meta.created_at_ms,
        "block_number": block,
        "subject": if owner { "owner" } else { "account" },
        "submit": submission,
        "next": "poll companion_proveStatus { job_id } until status is \"succeeded\" (result = { call, proof, notes }, \
                 or { transaction_hash, submitter_address, call, notes } after submit: \"account\") or \"failed\" (error, \
                 error_reason). The result is kept 1 h, and dropped if the wallet locks.",
    });
    if let Some(s) = &submitter {
        out["submitter_address"] = json!(s.address);
    }
    Ok(out)
}

/// An error's message without the `REASON: ` prefix `with_reason` adds.
fn e_text(e: &WalletRpcError) -> String {
    let m = e.message();
    match e.reason() {
        Some(r) => m.strip_prefix(&format!("{r}: ")).unwrap_or(&m).to_string(),
        None => m,
    }
}

/// A discovery error, scrubbed of `user_sk` (node errors quote calldata).
fn scrub_err(e: WalletRpcError, user_sk: Felt) -> WalletRpcError {
    match e {
        WalletRpcError::Node(m) => WalletRpcError::Node(scrub(&m, user_sk)),
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(r: Result<u64, WalletRpcError>) -> Option<&'static str> {
        r.err().and_then(|e| e.reason())
    }

    #[test]
    fn reference_block_defaults_to_head_minus_lag() {
        assert_eq!(reference_block(1000, None, 988, 450).unwrap(), 988);
        assert_eq!(reference_block(1010, None, 988, 450).unwrap(), 998);
    }

    #[test]
    fn reference_block_bounds() {
        // Too recent: explicit, and the default before the head moved on.
        let e = reference_block(1000, Some(989), 900, 450).unwrap_err();
        assert_eq!(e.reason(), Some("BLOCK_TOO_RECENT"));
        assert!(e.message().contains("Retry in 1 block"), "{}", e.message());
        assert_eq!(
            reason(reference_block(1000, None, 989, 450)),
            Some("BLOCK_TOO_RECENT")
        );
        assert_eq!(
            reason(reference_block(5, None, 0, 450)),
            Some("BLOCK_TOO_RECENT")
        );
        // Before the plan.
        assert_eq!(
            reason(reference_block(1000, Some(950), 960, 450)),
            Some("BLOCK_BEFORE_PLAN")
        );
        // Inside the window: head − B < 450 − 120 = 330.
        assert_eq!(reference_block(1000, Some(671), 600, 450).unwrap(), 671);
        let e = reference_block(1000, Some(670), 600, 450).unwrap_err();
        assert_eq!(e.reason(), Some("BLOCK_TOO_OLD"));
        assert!(e.message().contains("from 671 on"), "{}", e.message());
        // A pool without a window (validity 0) can't be proved for.
        assert_eq!(
            reason(reference_block(1000, None, 988, 0)),
            Some("BLOCK_TOO_OLD")
        );
    }

    #[test]
    fn reference_block_never_overflows_on_a_huge_block_number() {
        let e = reference_block(1000, Some(u64::MAX), 900, 450).unwrap_err();
        assert_eq!(e.reason(), Some("BLOCK_TOO_RECENT"));
        assert!(e.message().contains(&format!("Retry in {} block", u64::MAX - 1000)), "{}", e.message());
        let e = reference_block(5, Some(u64::MAX - 3), 0, 450).unwrap_err();
        assert_eq!(e.reason(), Some("BLOCK_TOO_RECENT"));
    }

    #[test]
    fn prover_failures_carry_a_reason_and_no_prover_text() {
        let sk =
            Felt::from_hex("0x2f6c0b3c1a9e5d4f8b7a6e5d4c3b2a19087f6e5d4c3b2a1908f7e6d5c4b3a29")
                .unwrap();
        // The secret, encoded so no scrubbing would find it.
        let encoded: String = format!("{sk:#x}").chars().rev().collect();
        let f = prover_failure(
            &format!("calldata [{sk:#x}] {encoded} ('INVALID_SIGNATURE')"),
            true,
            77,
        );
        assert_eq!(f.reason.as_deref(), Some("OWNER_SIGNATURE_REJECTED"));
        assert!(f.message.starts_with("OWNER_SIGNATURE_REJECTED: "));
        assert!(f.message.contains("reference block 77"));
        assert!(f.message.contains("make a new plan"));
        assert!(!f.message.contains(&format!("{sk:#x}")) && !f.message.contains(&encoded), "{}", f.message);
        let f = prover_failure(&format!("boom {encoded}"), true, 77);
        assert_eq!(f.reason.as_deref(), Some("PROVING_FAILED"));
        assert!(!f.message.contains(&encoded) && !f.message.contains("boom"), "{}", f.message);
        let f = prover_failure("INVALID_SIGNATURE", false, 77);
        assert_eq!(f.reason.as_deref(), Some("INVALID_SIGNATURE"));
    }

    fn snapshot(registered: bool) -> State {
        State {
            user: Felt::from(0xc0ffeeu64),
            user_sk: Felt::ZERO,
            registered,
            outgoing_channels: 0,
            channels: Default::default(),
            notes: Vec::new(),
        }
    }

    fn plan_with(actions: Vec<strk20::actions::ClientAction>) -> StoredPlan {
        let (user, pool) = (Felt::from(0xc0ffeeu64), Felt::from(0x123u64));
        let felts = serialize_actions(&actions);
        let hash = plan_hash(ChainId::Sepolia, &pool, &user, &felts)
            .unwrap()
            .plan_hash;
        StoredPlan {
            id: "a".into(),
            client_id: "c".into(),
            subject: Subject::Account,
            account: AccountRef {
                domain: wallet_core::Domain::User,
                index: 0,
                address: "0x1".into(),
                label: String::new(),
                contract: Default::default(),
                owner_client_id: None,
            },
            user,
            chain: ChainId::Sepolia,
            pool,
            plan_block: 1,
            intent: Intent::default(),
            actions,
            client_actions_felts: felts,
            plan_hash: hash,
            notes: Vec::new(),
            state: snapshot(false),
            prover_id: "native".into(),
            submission: "none",
            prompt: String::new(),
            created_at_ms: 0,
            expires_at_ms: u64::MAX,
        }
    }

    /// A plan whose stored actions, felts or hash disagree is detected (it
    /// can't happen through the API: prove takes only a plan id).
    #[test]
    fn verify_plan_detects_staleness_and_tampering() {
        use strk20::actions::ClientAction;
        let action = || ClientAction::SetViewingKey {
            random: Felt::from(7u64),
        };
        let (fresh, moved) = (snapshot(false), snapshot(true));
        assert!(verify_plan(&plan_with(vec![action()]), &fresh, &fresh).is_ok());
        let p = plan_with(vec![action()]);
        assert_eq!(
            verify_plan(&p, &fresh, &moved)
                .err()
                .and_then(|e| e.reason()),
            Some("PLAN_STALE")
        );
        assert_eq!(
            verify_plan(&p, &moved, &fresh)
                .err()
                .and_then(|e| e.reason()),
            Some("PLAN_STALE")
        );

        let mut tampered = plan_with(vec![action()]);
        tampered.actions = vec![ClientAction::SetViewingKey {
            random: Felt::from(8u64),
        }];
        let e = verify_plan(&tampered, &fresh, &fresh).unwrap_err();
        assert_eq!(e.reason(), Some("PLAN_HASH_MISMATCH"));
        assert_eq!(e.code(), 163);
        let mut rehashed = plan_with(vec![action()]);
        rehashed.plan_hash = Felt::ONE;
        assert_eq!(
            verify_plan(&rehashed, &fresh, &fresh)
                .err()
                .and_then(|e| e.reason()),
            Some("PLAN_HASH_MISMATCH")
        );
    }

    #[test]
    fn params_are_validated() {
        assert!(signature_param(&json!({})).unwrap().is_none());
        assert_eq!(
            signature_param(&json!({ "signature": [] })).unwrap(),
            Some(vec![])
        );
        assert!(signature_param(&json!({ "signature": ["zz"] })).is_err());
        assert!(signature_param(&json!({ "signature": "0x1" })).is_err());
        let long: Vec<String> = (0..=MAX_SIGNATURE_FELTS)
            .map(|i| format!("{i:#x}"))
            .collect();
        assert!(signature_param(&json!({ "signature": long })).is_err());
        assert!(block_param(&json!({ "block_number": -1 })).is_err());
        assert!(block_param(&json!({ "block_number": "5" })).is_err());
        assert!(submit_param(&json!({ "submit": "account" })).unwrap() == Submit::Account);
        assert!(submit_param(&json!({ "submit": "nope" })).is_err());
        let relay = submit_param(&json!({ "submit": "relay" })).err().unwrap();
        assert_eq!(relay.reason(), Some("RELAY_UNSUPPORTED"));
        assert_eq!(relay.code(), -32601);
    }
}
