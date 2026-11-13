//! External STRK20 owners: enrolment, listing and removal
//! (`docs/project/design-48-strk20-external-owner.md` §3).
//!
//! An owner is a deployed contract that advertises SRC5
//! `is_custom_signature_valid`, so the pool lets its own logic approve each
//! spend. strkd holds the owner's viewing key, derived from a controller
//! account's key and bound to the owner address (§3.2), and records the
//! enrolment in the sealed registry. The record holds no secrets; the key is
//! derived per call, never persisted, logged or returned.
//!
//! Planning and proving for owners come in later parts of #48.

use serde_json::{json, Value};
use strk20::discovery;
use wallet_core::{address_hex, get_selector_from_name, Felt, OwnerController, Strk20Owner};

use super::{
    address_param, chain_and_pool, ctx, hex, invalid, is_contract_not_found, map_strk20, node_err,
    not_enrolled, scrub, Caller, Ctx, Subject,
};
use crate::dispatch::{chain_name, persist_registry, ServerState};
use crate::error::WalletRpcError;
use crate::node::NodeError;

/// SRC5 interface id the pool checks before calling `is_custom_signature_valid`
/// (`selector!("is_custom_signature_valid")`, starknet-privacy `utils.cairo:39`).
pub const CUSTOM_SIGNATURE_INTERFACE_ID: &str =
    "0x2eb9faa4ce06e09879e93803798b87d22877b73964334356bf9e0d8cc22ded";
/// Labels are caller text shown in prompts and lists: keep them short.
const MAX_LABEL_CHARS: usize = 64;

fn owner_json(o: &Strk20Owner) -> Value {
    json!({
        "owner_address": o.owner,
        "chain": chain_name(o.chain),
        "pool": o.pool,
        "controller": { "address": o.controller.address, "domain": o.controller.domain, "index": o.controller.index },
        "label": o.label,
    })
}

/// Whether `owner` has a class at the latest block.
async fn ensure_owner_deployed(ctx: &Ctx) -> Result<(), WalletRpcError> {
    let params = json!({ "block_id": "latest", "contract_address": hex(&ctx.user) });
    match ctx.node.raw("starknet_getClassHashAt", params).await {
        Ok(_) => Ok(()),
        Err(e) if is_contract_not_found(&e) => Err(WalletRpcError::Precondition(format!(
            "no contract is deployed at {} on {}: deploy the owner contract first",
            ctx.address(),
            chain_name(ctx.chain)
        ))
        .with_reason("OWNER_NOT_DEPLOYED")),
        Err(e) => Err(node_err(e)),
    }
}

/// Whether `owner` answers `supports_interface(CUSTOM_SIGNATURE_INTERFACE_ID)`
/// with `true` at `block`. A contract without SRC5 reverts or has no such entry
/// point: the node returns an RPC error, which reads as "not supported". A
/// transport or decode failure stays a node error.
pub(super) async fn supports_custom_validation(
    ctx: &Ctx,
    block: Value,
) -> Result<bool, WalletRpcError> {
    let request = json!({
        "contract_address": hex(&ctx.user),
        "entry_point_selector": hex(&get_selector_from_name("supports_interface")),
        "calldata": [CUSTOM_SIGNATURE_INTERFACE_ID],
    });
    match ctx
        .node
        .raw(
            "starknet_call",
            json!({ "request": request, "block_id": block }),
        )
        .await
    {
        Ok(out) => {
            let first = out
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .and_then(|s| Felt::from_hex(s).ok());
            Ok(first == Some(Felt::ONE))
        }
        Err(NodeError::Rpc(_)) => Ok(false),
        Err(e) => Err(node_err(e)),
    }
}

/// `companion_strk20AddOwner { owner_address, account_address?, label?, chainId? }`
/// → `{ owner_address, chain, pool, controller, label, registered, viewing_public_key }`.
///
/// `account_address` names the controller (default: the caller's first scoped
/// account). Checks, in order: the owner is deployed (`OWNER_NOT_DEPLOYED`), it
/// advertises SRC5 custom validation (`OWNER_NO_CUSTOM_VALIDATION`), and if it
/// is already registered with the pool, under the key this controller derives
/// (`OWNER_KEY_MISMATCH`, D8). Then it always prompts — a grant never covers
/// enrolment (D6) — and records the owner.
pub async fn add_owner(
    state: &ServerState,
    caller: Caller<'_>,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let owner = address_param(params, "owner_address")?
        .ok_or_else(|| invalid("missing 'owner_address'"))?;
    let label: String = match params.get("label") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_LABEL_CHARS)
            .collect(),
        Some(_) => return Err(invalid("label: expected a string")),
    };
    // The controller: `account_address` (or the default), resolved and scoped
    // like any STRK20 call. `owner_address` here names the new owner, not the
    // subject, so it is left out of that resolution.
    let mut controller_params = params.clone();
    if let Some(m) = controller_params.as_object_mut() {
        m.remove("owner_address");
    }
    let base = ctx(state, caller, &controller_params).await?;
    let scope = caller.scope();
    let pool_hex = address_hex(&base.pool);
    {
        let session = state.session.lock().await;
        let reg = session.registry()?;
        if reg.accounts.iter().any(|a| a.address == owner) {
            return Err(invalid(format!(
                "{owner} is a strkd account: use it with account_address, not as an owner"
            )));
        }
        if reg
            .strk20_owners_for(scope.as_deref())
            .any(|o| o.is(&owner, base.chain, &pool_hex, scope.as_deref()))
        {
            return Err(WalletRpcError::Precondition(format!(
                "{owner} is already enrolled on {} for pool {pool_hex}",
                chain_name(base.chain)
            ))
            .with_reason("OWNER_ALREADY_ENROLLED"));
        }
    }

    let record = Strk20Owner {
        owner: owner.clone(),
        chain: base.chain,
        pool: pool_hex.clone(),
        controller: OwnerController {
            domain: base.account.domain,
            index: base.account.index,
            address: base.account.address.clone(),
        },
        client_id: scope.clone(),
        label,
    };
    let user = Felt::from_hex(&owner).map_err(|_| invalid("owner_address: expected a felt"))?;
    let ctx = Ctx {
        subject: Subject::Owner(record.clone()),
        user,
        ..base
    };

    ensure_owner_deployed(&ctx).await?;
    if !supports_custom_validation(&ctx, json!("latest")).await? {
        return Err(WalletRpcError::Precondition(format!(
            "{owner} does not advertise SRC5 interface {CUSTOM_SIGNATURE_INTERFACE_ID} (is_custom_signature_valid), \
             so the pool would not let it approve spends itself. Only owners with that interface can be enrolled."
        ))
        .with_reason("OWNER_NO_CUSTOM_VALIDATION"));
    }

    let user_sk = ctx.viewing_key(state).await?;
    let viewing_public_key = strk20::crypto::public_key(user_sk);
    let registered = discovery::public_key(&ctx.reader(json!("latest")), ctx.user)
        .await
        .map_err(|e| match map_strk20(e) {
            WalletRpcError::Node(m) => WalletRpcError::Node(scrub(&m, user_sk)),
            e => e,
        })?;
    if let Some(pk) = registered {
        if pk != viewing_public_key {
            return Err(WalletRpcError::Precondition(format!(
                "{owner} is registered with the pool under a viewing key that controller {} does not derive, so strkd \
                 cannot read or spend its notes and will not enrol it (importing keys is not supported).",
                ctx.account.address
            ))
            .with_reason("OWNER_KEY_MISMATCH"));
        }
    }

    let label_note = match record.label.as_str() {
        "" => String::new(),
        l => format!(" Caller-supplied label: \"{l}\"."),
    };
    caller
        .approve_always(
            state,
            "companion_strk20AddOwner",
            format!(
                "Enrol contract {owner} as a STRK20 owner on {}, pool {pool_hex}, controlled by your account {}.{label_note} \
                 strkd will derive and hold a private key for this contract's private notes. strkd cannot check who \
                 controls this contract; its own signature check decides every spend. {}",
                chain_name(ctx.chain),
                ctx.account.address,
                if registered.is_some() {
                    "It is already registered with the pool under the key this account derives."
                } else {
                    "It is not registered with the pool yet."
                },
            ),
        )
        .await?;

    let mut session = state.session.lock().await;
    if !session.add_strk20_owner(record.clone())? {
        return Err(WalletRpcError::Precondition(format!(
            "{owner} was enrolled by a concurrent request"
        ))
        .with_reason("OWNER_ALREADY_ENROLLED"));
    }
    persist_registry(state, &mut session, |s| s.rollback_strk20_owner(&record))?;
    drop(session);

    let mut out = owner_json(&record);
    out["registered"] = json!(registered.is_some());
    out["viewing_public_key"] = json!(hex(&viewing_public_key));
    Ok(out)
}

/// `companion_strk20ListOwners {}` → `[{ owner_address, chain, pool, controller, label }]`:
/// the owners in the caller's scope, on every chain and pool. No prompt.
pub async fn list_owners(state: &ServerState, caller: Caller<'_>) -> Result<Value, WalletRpcError> {
    let session = state.session.lock().await;
    let scope = caller.scope();
    let owners: Vec<Value> = session
        .registry()?
        .strk20_owners_for(scope.as_deref())
        .map(owner_json)
        .collect();
    Ok(json!(owners))
}

/// `companion_strk20RemoveOwner { owner_address, chainId? }` → `true`.
///
/// Forgets the record for the active (or given) chain and its pool. Prompts
/// (a grant may cover it: nothing is signed or revealed). The key is not lost:
/// enrolling again derives the same one.
pub async fn remove_owner(
    state: &ServerState,
    caller: Caller<'_>,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let owner = address_param(params, "owner_address")?
        .ok_or_else(|| invalid("missing 'owner_address'"))?;
    let (chain, _, pool) = chain_and_pool(state, params).await?;
    let pool_hex = address_hex(&pool);
    let scope = caller.scope();
    let record = {
        let session = state.session.lock().await;
        let found = session
            .registry()?
            .strk20_owners_for(scope.as_deref())
            .find(|o| o.is(&owner, chain, &pool_hex, scope.as_deref()))
            .cloned();
        found.ok_or_else(not_enrolled)?
    };
    caller
        .approve(
            state,
            "companion_strk20RemoveOwner",
            format!(
                "Forget STRK20 owner {owner} on {} (pool {pool_hex}, controller {}). Its private notes stay on chain; \
                 enrolling it again restores access.",
                chain_name(chain),
                record.controller.address
            ),
        )
        .await?;
    let mut session = state.session.lock().await;
    let removed = session
        .remove_strk20_owner(&owner, chain, &pool_hex, scope.as_deref())?
        .ok_or_else(not_enrolled)?;
    persist_registry(state, &mut session, |s| {
        let _ = s.add_strk20_owner(removed.clone());
    })?;
    Ok(json!(true))
}
