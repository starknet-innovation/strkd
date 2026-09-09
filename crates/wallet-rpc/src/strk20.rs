//! STRK20 (Tongo) privacy-pool handlers — Phase 3, targeting `wallet_rpc.json`
//! v0.10.4-rc.0.
//!
//! Backend: the Tongo confidential-balance protocol (audited ElGamal + sigma
//! proofs over the Stark curve) via `krusty-kms-sdk` (proof generation — pure,
//! millisecond-scale) and `krusty-kms-client` (Tongo contract calldata
//! builders — also pure). All chain I/O goes through the wallet's own
//! `StarknetRpc` seam (`call_contract`), so dispatch stays fully mockable.
//!
//! ## Spec surface vs. what Tongo can express
//!
//! The community spec models a note-based pool (open notes, arbitrary invoke
//! actions, sub-accounts, an external proving service). Tongo has none of
//! those: it is a per-token encrypted balance with five operations and proofs
//! embedded in the call's calldata. strkd implements the intersection and is
//! explicit about the rest:
//!
//! - Actions `deposit`, `withdraw`, `transfer` (fixed amount): supported.
//! - Action `rollover` (strkd EXTENSION): Tongo lands incoming transfers in a
//!   *pending* balance that an explicit rollover makes spendable; the spec has
//!   no hook for this, so strkd exposes it as an extra action type.
//! - `transfer` with amount `"OPEN"`, `invoke`, `subaccount_invoke`: parsed,
//!   rejected with a clear error — not expressible in Tongo.
//! - `wallet_strk20SubaccountCommitment`: stays `-32601` (see `is_deferred`).
//! - One action per request: every Tongo operation consumes the pool-side
//!   account nonce, so bundling two would bind the second proof to a stale
//!   nonce. (Deposit still expands to two *calls*: ERC-20 approve + fund.)
//! - `proof` in results is always the empty `STRK20_PROOF` shape: Tongo's
//!   proofs ride inside calldata, so there is no separate proof object and no
//!   expensive proving step for `simulate` to skip — the returned call is
//!   submittable either way.
//! - Transfer recipients are **Tongo public keys** (base58 `tongo1…` string or
//!   an `{x, y}` felt object), not Starknet addresses: Tongo has no on-chain
//!   address→key registry. Recipients share their Tongo address out of band
//!   (it is public, safe-to-share data).
//! - Fees: strkd submits through the user's own (public) Starknet account, so
//!   no relayer fee-withdraw action is added (the spec's fee-action rule
//!   assumes paymaster submission). Amounts and balances stay encrypted; the
//!   *sender account* of the wrapping transaction is public, as with every
//!   Tongo transaction.
//!
//! Pool discovery: a Tongo contract wraps exactly one ERC-20, so the wallet
//! keeps a per-network registry `token address → pool address`
//! ([`ServerState::set_strk20_pool`]). A token with no registered pool errors
//! with the spec's `NOT_REGISTERED` (118) and an actionable message.

use serde_json::{json, Value};
use starknet_types_core::curve::ProjectivePoint;
use starknet_types_core::felt::Felt;

use krusty_kms_client::{
    build_fund_calls, build_rollover_call, build_transfer_call, build_withdraw_call,
    decrypt_cipher_balance_with_limit, pub_key_to_tongo_address, tongo_address_to_pub_key,
    CipherBalance,
};
use krusty_kms_common::ElGamalCiphertext;
use krusty_kms_sdk::crypto::{derive_shared_secret, encrypt_audit_hint};
use krusty_kms_sdk::{
    fund, rollover, transfer, withdraw, FundParams, RolloverParams, TongoAccount, TransferParams,
    WithdrawParams,
};
use wallet_core::{AccountRef, Call, ChainId};

use crate::auth::PairedClient;
use crate::dispatch::{
    chain_name, felt_hex, gated_approval, normalize_address, opt_param_str, parse_u128_str,
    resolve_chain, resolve_exec, scope_for, sign_and_submit, ServerState,
};
use crate::error::WalletRpcError;
use crate::node::StarknetRpc;

/// A parsed, supported STRK20 action. Unsupported spec variants are rejected
/// at parse time with an explanatory error rather than modeled here.
#[derive(Debug, Clone)]
pub enum Strk20Action {
    /// Public ERC-20 → encrypted pool balance (always to self).
    Deposit { token: Felt, amount: u128 },
    /// Encrypted pool balance → public ERC-20 at `recipient`.
    Withdraw { token: Felt, amount: u128, recipient: Felt },
    /// Confidential in-pool transfer to another Tongo key.
    Transfer {
        token: Felt,
        amount: u128,
        recipient: ProjectivePoint,
        /// How the caller wrote the recipient (for prompts/logs — public data).
        recipient_display: String,
    },
    /// strkd extension: make pending (received) funds spendable.
    Rollover { token: Felt },
}

impl Strk20Action {
    fn token(&self) -> Felt {
        match self {
            Strk20Action::Deposit { token, .. }
            | Strk20Action::Withdraw { token, .. }
            | Strk20Action::Transfer { token, .. }
            | Strk20Action::Rollover { token } => *token,
        }
    }

    /// One-line human summary for the approval prompt (amounts in the token's
    /// smallest unit; all values here are public or chosen by the caller).
    fn summary(&self) -> String {
        match self {
            Strk20Action::Deposit { token, amount } => {
                format!("deposit {amount} (smallest unit) of {} into the privacy pool", felt_hex(token))
            }
            Strk20Action::Withdraw { token, amount, recipient } => format!(
                "withdraw {amount} (smallest unit) of {} from the privacy pool to {}",
                felt_hex(token),
                felt_hex(recipient)
            ),
            Strk20Action::Transfer { token, amount, recipient_display, .. } => format!(
                "confidentially transfer {amount} (smallest unit) of {} to {recipient_display}",
                felt_hex(token)
            ),
            Strk20Action::Rollover { token } => {
                format!("roll over pending {} funds into the spendable private balance", felt_hex(token))
            }
        }
    }
}

fn felt_param(v: &Value, what: &str) -> Result<Felt, WalletRpcError> {
    v.as_str()
        .and_then(|s| Felt::from_hex(s).ok())
        .ok_or_else(|| WalletRpcError::InvalidRequest(format!("{what} must be a 0x-felt string")))
}

fn amount_param(item: &Value) -> Result<u128, WalletRpcError> {
    let amount = match item.get("amount") {
        Some(Value::String(s)) if s == "OPEN" => {
            return Err(WalletRpcError::NotImplemented(
                "STRK20 open notes (amount \"OPEN\") are not supported: the Tongo backend has \
                 no note model. Use a fixed amount."
                    .into(),
            ))
        }
        Some(Value::String(s)) => parse_u128_str(s)?,
        Some(Value::Number(n)) => n
            .as_u64()
            .map(u128::from)
            .ok_or_else(|| WalletRpcError::InvalidRequest("bad amount".into()))?,
        _ => return Err(WalletRpcError::InvalidRequest("action missing 'amount'".into())),
    };
    if amount == 0 {
        return Err(WalletRpcError::InvalidRequest("amount must be > 0".into()));
    }
    Ok(amount)
}

/// Transfer recipient: a base58 Tongo address, or an `{x, y}` felt object.
/// A 0x string is almost certainly a Starknet address — reject it with an
/// explanation instead of mis-parsing it as a curve point.
fn recipient_key(item: &Value) -> Result<(ProjectivePoint, String), WalletRpcError> {
    match item.get("recipient") {
        Some(Value::String(s)) if s.starts_with("0x") => Err(WalletRpcError::InvalidRequest(
            "transfer 'recipient' must be the recipient's Tongo PUBLIC KEY (their base58 Tongo \
             address, or {x, y} felts) — not a Starknet address. Tongo has no on-chain \
             address→key registry; ask the recipient for their Tongo address (it is public)."
                .into(),
        )),
        Some(Value::String(s)) => {
            let p = tongo_address_to_pub_key(s).map_err(|_| {
                WalletRpcError::InvalidRequest("recipient is not a valid Tongo address".into())
            })?;
            Ok((p, s.clone()))
        }
        Some(Value::Object(o)) => {
            let x = felt_param(o.get("x").unwrap_or(&Value::Null), "recipient.x")?;
            let y = felt_param(o.get("y").unwrap_or(&Value::Null), "recipient.y")?;
            let p = ProjectivePoint::from_affine(x, y).map_err(|_| {
                WalletRpcError::InvalidRequest("recipient {x, y} is not a point on the Stark curve".into())
            })?;
            let display = pub_key_to_tongo_address(&p).unwrap_or_else(|_| format!("{}, {}", felt_hex(&x), felt_hex(&y)));
            Ok((p, display))
        }
        _ => Err(WalletRpcError::InvalidRequest("transfer action missing 'recipient'".into())),
    }
}

/// Parse the spec's `actions` array. All five spec variants are recognized;
/// the ones Tongo cannot express fail with a targeted error, unknown types
/// with a generic one.
pub fn parse_actions(params: &Value) -> Result<Vec<Strk20Action>, WalletRpcError> {
    let arr = params
        .get("actions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| WalletRpcError::InvalidRequest("missing 'actions' array".into()))?;
    if arr.is_empty() {
        return Err(WalletRpcError::InvalidRequest("'actions' is empty".into()));
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let ty = item
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| WalletRpcError::InvalidRequest("action missing 'type'".into()))?;
        let token = || felt_param(item.get("token").unwrap_or(&Value::Null), "token");
        out.push(match ty {
            "deposit" => Strk20Action::Deposit { token: token()?, amount: amount_param(item)? },
            "withdraw" => Strk20Action::Withdraw {
                token: token()?,
                amount: amount_param(item)?,
                recipient: felt_param(item.get("recipient").unwrap_or(&Value::Null), "recipient")?,
            },
            "transfer" => {
                let amount = amount_param(item)?; // rejects "OPEN" with its own message
                let (recipient, recipient_display) = recipient_key(item)?;
                Strk20Action::Transfer { token: token()?, amount, recipient, recipient_display }
            }
            "rollover" => Strk20Action::Rollover { token: token()? },
            "invoke" | "subaccount_invoke" => {
                return Err(WalletRpcError::NotImplemented(format!(
                    "STRK20 action '{ty}' is not supported: the Tongo backend has no \
                     open-note/sub-account model (spec 0.10.4 sub-accounts need a pool \
                     contract that does not exist for Tongo). Supported actions: deposit, \
                     withdraw, transfer, rollover (strkd extension)."
                )))
            }
            other => {
                return Err(WalletRpcError::InvalidRequest(format!(
                    "unknown STRK20 action type '{other}'"
                )))
            }
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pool chain reads (through the StarknetRpc seam)
// ---------------------------------------------------------------------------

struct PoolAccount {
    balance_cipher: ElGamalCiphertext,
    pending_cipher: ElGamalCiphertext,
    nonce: Felt,
}

async fn read_felts(
    node: &dyn StarknetRpc,
    pool: &Felt,
    entrypoint: &str,
    calldata: &[Felt],
) -> Result<Vec<Felt>, WalletRpcError> {
    let sel = wallet_core::get_selector_from_name(entrypoint);
    node.call_contract(pool, &sel, calldata)
        .await
        .map_err(|e| WalletRpcError::Node(format!("{entrypoint}: {e}")))
}

fn point_at(out: &[Felt], i: usize, what: &str) -> Result<ProjectivePoint, WalletRpcError> {
    ProjectivePoint::from_affine(out[i], out[i + 1])
        .map_err(|_| WalletRpcError::Node(format!("{what}: invalid curve point from pool")))
}

async fn read_pool_account(
    node: &dyn StarknetRpc,
    pool: &Felt,
    key: &ProjectivePoint,
) -> Result<PoolAccount, WalletRpcError> {
    let a = key
        .to_affine()
        .map_err(|_| WalletRpcError::Unknown("tongo public key at infinity".into()))?;
    let out = read_felts(node, pool, "get_state", &[a.x(), a.y()]).await?;
    // AccountState { balance: CipherBalance, pending: CipherBalance, nonce } = 9 felts.
    if out.len() < 9 {
        return Err(WalletRpcError::Node(format!(
            "get_state: expected 9 felts, got {}",
            out.len()
        )));
    }
    Ok(PoolAccount {
        balance_cipher: ElGamalCiphertext {
            l: point_at(&out, 0, "balance.L")?,
            r: point_at(&out, 2, "balance.R")?,
        },
        pending_cipher: ElGamalCiphertext {
            l: point_at(&out, 4, "pending.L")?,
            r: point_at(&out, 6, "pending.R")?,
        },
        nonce: out[8],
    })
}

async fn read_u128(
    node: &dyn StarknetRpc,
    pool: &Felt,
    entrypoint: &str,
) -> Result<u128, WalletRpcError> {
    let out = read_felts(node, pool, entrypoint, &[]).await?;
    let first = out
        .first()
        .ok_or_else(|| WalletRpcError::Node(format!("{entrypoint}: empty result")))?;
    u128::try_from(first.to_biguint())
        .map_err(|_| WalletRpcError::Node(format!("{entrypoint}: value exceeds u128")))
}

/// The pool's ERC-20 (sanity check against the requested token).
async fn read_pool_erc20(node: &dyn StarknetRpc, pool: &Felt) -> Result<Felt, WalletRpcError> {
    let out = read_felts(node, pool, "ERC20", &[]).await?;
    out.first()
        .copied()
        .ok_or_else(|| WalletRpcError::Node("ERC20: empty result".into()))
}

/// The pool's auditor key, if configured (Cairo `Option`: variant 0 = Some).
async fn read_auditor(
    node: &dyn StarknetRpc,
    pool: &Felt,
) -> Result<Option<ProjectivePoint>, WalletRpcError> {
    let out = read_felts(node, pool, "auditor_key", &[]).await?;
    match out.first() {
        Some(v) if *v == Felt::ZERO && out.len() >= 3 => Ok(Some(point_at(&out, 1, "auditor_key")?)),
        Some(_) => Ok(None),
        None => Err(WalletRpcError::Node("auditor_key: empty result".into())),
    }
}

fn felt_to_u64(f: &Felt) -> u64 {
    let b = f.to_bytes_be();
    u64::from_be_bytes(b[24..32].try_into().expect("8 bytes"))
}

/// Discrete-log search bound for balance decryption. Tongo pools range-prove
/// balances to 32 bits, so a valid balance always lies in `[0, 2^32)`.
const DECRYPT_LIMIT: u128 = 1 << 32;

fn decrypt(kp_private: &Felt, cipher: &ElGamalCiphertext) -> Result<u128, WalletRpcError> {
    let cb = CipherBalance { l: cipher.l.clone(), r: cipher.r.clone() };
    decrypt_cipher_balance_with_limit(kp_private, &cb, DECRYPT_LIMIT)
        .map_err(|_| WalletRpcError::Unknown("private balance decryption failed".into()))
}

/// Convert a spec amount (token smallest unit) into Tongo pool units.
fn to_tongo_units(amount: u128, rate: u128, what: &str) -> Result<u128, WalletRpcError> {
    if rate == 0 {
        return Err(WalletRpcError::Node("pool reports rate 0".into()));
    }
    if !amount.is_multiple_of(rate) {
        return Err(WalletRpcError::InvalidRequest(format!(
            "{what} amount must be a multiple of the pool's rate ({rate} smallest units per \
             pool unit)"
        )));
    }
    Ok(amount / rate)
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// A prepared STRK20 transaction: the Starknet calls that materialize one
/// action, plus a human summary for the approval prompt.
pub struct Prepared {
    pub calls: Vec<Call>,
    pub summary: String,
}

/// Convert the starknet-rs `Call` the krusty builders return into this
/// workspace's `wallet_core::Call` (same felt values, different crates; the
/// byte round-trip is the version-skew-proof conversion).
fn to_core_call(c: starknet_rust::core::types::Call) -> Call {
    let f = |x: starknet_rust::core::types::Felt| Felt::from_bytes_be(&x.to_bytes_be());
    Call {
        to: f(c.to),
        selector: f(c.selector),
        calldata: c.calldata.into_iter().map(f).collect(),
    }
}

/// Resolve the account this STRK20 request operates on. The spec assumes a
/// single-account wallet and has no account parameter; strkd accepts an
/// optional `account_address` (extension) and otherwise requires exactly one
/// in-scope account.
pub async fn resolve_account(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<AccountRef, WalletRpcError> {
    let session = state.session.lock().await;
    let reg = session.registry()?;
    let scoped: Vec<AccountRef> = reg.scoped_for(scope_for(client).as_deref()).cloned().collect();
    match opt_param_str(params, "account_address") {
        Some(a) => {
            let want = normalize_address(&a)?;
            scoped
                .into_iter()
                .find(|x| x.address == want)
                .ok_or(WalletRpcError::Forbidden)
        }
        None => match scoped.len() {
            0 => Err(WalletRpcError::DeploymentDataNotAvailable),
            1 => Ok(scoped.into_iter().next().expect("len checked")),
            _ => Err(WalletRpcError::InvalidRequest(
                "multiple accounts in scope: pass 'account_address' (strkd extension — the \
                 spec assumes a single-account wallet)"
                    .into(),
            )),
        },
    }
}

/// Build the Tongo calls for one action. Reads pool state through the node,
/// generates the proof, and returns ready-to-sign calls. Everything here is
/// per the recipe in krusty's `tongo_sepolia_integration` test.
pub async fn prepare_action(
    state: &ServerState,
    chain: ChainId,
    account: &AccountRef,
    action: &Strk20Action,
) -> Result<Prepared, WalletRpcError> {
    let token = action.token();
    let pool = state.strk20_pool_for(chain, &token).ok_or_else(|| {
        WalletRpcError::PoolNotRegistered(format!(
            "no STRK20 privacy pool registered for token {} on {} — the human operator adds \
             one in Settings (token → Tongo pool contract)",
            felt_hex(&token),
            chain_name(chain)
        ))
    })?;
    let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;
    let node = node.as_ref();

    // Guard against a misconfigured registry entry before proving anything.
    let pool_erc20 = read_pool_erc20(node, &pool).await?;
    if pool_erc20 != token {
        return Err(WalletRpcError::Unknown(format!(
            "pool registry mismatch: pool {} wraps ERC-20 {}, not {}",
            felt_hex(&pool),
            felt_hex(&pool_erc20),
            felt_hex(&token)
        )));
    }

    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
    let kp = {
        let session = state.session.lock().await;
        session.tongo_keypair_for(account)?
    };
    let sk: Felt = *kp.private_key.expose_secret();
    let chain_id = chain.as_felt();

    let pool_acct = read_pool_account(node, &pool, &kp.public_key).await?;
    let balance = decrypt(&sk, &pool_acct.balance_cipher)?;
    let pending = decrypt(&sk, &pool_acct.pending_cipher)?;
    let auditor = read_auditor(node, &pool).await?;

    let mut tongo = TongoAccount::from_private_key(sk, pool)
        .map_err(|_| WalletRpcError::Unknown("tongo account init failed".into()))?;
    tongo.set_balance(balance);
    tongo.set_pending_balance(pending);
    tongo.set_nonce(felt_to_u64(&pool_acct.nonce));

    // Self shared-secret for the balance hints (lets this wallet — and the
    // reference SDK — recover balances without a brute-force decrypt).
    let ss_self = derive_shared_secret(&sk, &kp.public_key)
        .map_err(|_| WalletRpcError::Unknown("hint key derivation failed".into()))?;
    let hint_err = |_| WalletRpcError::Unknown("hint encryption failed".into());

    let calls: Vec<Call> = match action {
        Strk20Action::Deposit { amount, .. } => {
            let rate = read_u128(node, &pool, "get_rate").await?;
            let units = to_tongo_units(*amount, rate, "deposit")?;
            let proof = fund(
                &tongo,
                FundParams {
                    amount: units,
                    nonce: pool_acct.nonce,
                    chain_id,
                    tongo_address: pool,
                    sender_address: sender,
                    auditor_pub_key: auditor,
                    current_balance: pool_acct.balance_cipher.clone(),
                },
            )
            .map_err(|e| WalletRpcError::Unknown(format!("fund proof: {e}")))?;
            let (hint_ct, hint_nonce) =
                encrypt_audit_hint(balance + units, &ss_self).map_err(hint_err)?;
            let (approve, fund_call) =
                build_fund_calls(pool, token, rate, &proof, &hint_ct, &hint_nonce)
                    .map_err(|e| WalletRpcError::Unknown(format!("fund call: {e}")))?;
            vec![to_core_call(approve), to_core_call(fund_call)]
        }
        Strk20Action::Withdraw { amount, recipient, .. } => {
            let rate = read_u128(node, &pool, "get_rate").await?;
            let bit_size = read_u128(node, &pool, "get_bit_size").await? as usize;
            let units = to_tongo_units(*amount, rate, "withdraw")?;
            if units > balance {
                return Err(insufficient(units, balance, pending, rate));
            }
            let proof = withdraw(
                &tongo,
                WithdrawParams {
                    recipient_address: *recipient,
                    amount: units,
                    nonce: pool_acct.nonce,
                    chain_id,
                    tongo_address: pool,
                    sender_address: sender,
                    current_balance: pool_acct.balance_cipher.clone(),
                    bit_size,
                    auditor_key: auditor,
                },
            )
            .map_err(|e| WalletRpcError::Unknown(format!("withdraw proof: {e}")))?;
            let (hint_ct, hint_nonce) =
                encrypt_audit_hint(balance - units, &ss_self).map_err(hint_err)?;
            let call = build_withdraw_call(pool, &proof, &hint_ct, &hint_nonce)
                .map_err(|e| WalletRpcError::Unknown(format!("withdraw call: {e}")))?;
            vec![to_core_call(call)]
        }
        Strk20Action::Transfer { amount, recipient, .. } => {
            let rate = read_u128(node, &pool, "get_rate").await?;
            let bit_size = read_u128(node, &pool, "get_bit_size").await? as usize;
            let units = to_tongo_units(*amount, rate, "transfer")?;
            if units > balance {
                return Err(insufficient(units, balance, pending, rate));
            }
            let proof = transfer(
                &tongo,
                TransferParams {
                    recipient_public_key: recipient.clone(),
                    amount: units,
                    nonce: pool_acct.nonce,
                    chain_id,
                    tongo_address: pool,
                    sender_address: sender,
                    current_balance: pool_acct.balance_cipher.clone(),
                    bit_size,
                    auditor_pub_key: auditor,
                },
            )
            .map_err(|e| WalletRpcError::Unknown(format!("transfer proof: {e}")))?;
            // Hint for the recipient (ECDH: they derive the same secret from
            // their key + our public key), and a leftover hint for ourselves.
            let ss_recipient = derive_shared_secret(&sk, recipient)
                .map_err(|_| WalletRpcError::Unknown("hint key derivation failed".into()))?;
            let (t_ct, t_nonce) = encrypt_audit_hint(units, &ss_recipient).map_err(hint_err)?;
            let (l_ct, l_nonce) =
                encrypt_audit_hint(balance - units, &ss_self).map_err(hint_err)?;
            let call = build_transfer_call(
                pool,
                &kp.public_key,
                recipient,
                &proof,
                &t_ct,
                &t_nonce,
                &l_ct,
                &l_nonce,
            )
            .map_err(|e| WalletRpcError::Unknown(format!("transfer call: {e}")))?;
            vec![to_core_call(call)]
        }
        Strk20Action::Rollover { .. } => {
            if pending == 0 {
                return Err(WalletRpcError::InvalidRequest(
                    "nothing to roll over: pending private balance is 0".into(),
                ));
            }
            let proof = rollover(
                &tongo,
                RolloverParams {
                    nonce: pool_acct.nonce,
                    chain_id,
                    tongo_address: pool,
                    sender_address: sender,
                },
            )
            .map_err(|e| WalletRpcError::Unknown(format!("rollover proof: {e}")))?;
            let (hint_ct, hint_nonce) =
                encrypt_audit_hint(balance + pending, &ss_self).map_err(hint_err)?;
            let call = build_rollover_call(pool, &proof, &hint_ct, &hint_nonce)
                .map_err(|e| WalletRpcError::Unknown(format!("rollover call: {e}")))?;
            vec![to_core_call(call)]
        }
    };

    Ok(Prepared { calls, summary: action.summary() })
}

fn insufficient(units: u128, balance: u128, pending: u128, rate: u128) -> WalletRpcError {
    let mut m = format!(
        "insufficient private balance: have {} pool units, need {units}",
        balance
    );
    if pending > 0 {
        m.push_str(&format!(
            " ({pending} more are pending — add a {{\"type\":\"rollover\"}} action first to \
             make them spendable)"
        ));
    }
    m.push_str(&format!(" (1 pool unit = {rate} smallest token units)"));
    WalletRpcError::InsufficientPrivateBalance(m)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// The empty `STRK20_PROOF` object. Tongo proofs are embedded in calldata, so
/// this is what every prepare/simulate result carries (spec shape, no content).
fn empty_proof() -> Value {
    json!({ "data": "", "output": [], "proof_facts": [] })
}

fn call_json(c: &Call) -> Value {
    json!({
        "contract_address": felt_hex(&c.to),
        "entry_point_selector": felt_hex(&c.selector),
        "calldata": c.calldata.iter().map(felt_hex).collect::<Vec<_>>(),
    })
}

/// `wallet_strk20Balances` — decrypt and report private balances. No prompt
/// (read-only over the caller's own scoped account), needs the unlocked seed.
pub async fn handle_balances(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let tokens: Vec<Felt> = match params.get("tokens") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| felt_param(v, "tokens element"))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(WalletRpcError::InvalidRequest("missing 'tokens' array".into())),
    };
    let account = resolve_account(state, client, params).await?;
    let chain = resolve_chain(state, params).await?;
    let node = state.node_for(chain).ok_or(WalletRpcError::NoNode)?;

    // Empty array = all shielded tokens the wallet knows pools for (spec #391).
    let tokens = if tokens.is_empty() { state.strk20_tokens(chain) } else { tokens };

    let kp = {
        let session = state.session.lock().await;
        session.tongo_keypair_for(&account)?
    };
    let sk: Felt = *kp.private_key.expose_secret();

    let mut entries = Vec::with_capacity(tokens.len());
    for token in tokens {
        let pool = state.strk20_pool_for(chain, &token).ok_or_else(|| {
            WalletRpcError::PoolNotRegistered(format!(
                "no STRK20 privacy pool registered for token {} on {}",
                felt_hex(&token),
                chain_name(chain)
            ))
        })?;
        let acct = read_pool_account(node.as_ref(), &pool, &kp.public_key).await?;
        let rate = read_u128(node.as_ref(), &pool, "get_rate").await?;
        let balance = decrypt(&sk, &acct.balance_cipher)?;
        let pending = decrypt(&sk, &acct.pending_cipher)?;
        entries.push(json!({
            "token": felt_hex(&token),
            // Spec field, in the token's smallest unit.
            "balance": format!("0x{:x}", balance.saturating_mul(rate)),
            // strkd extension: funds received but not yet rolled over (not
            // spendable until a {"type":"rollover"} action lands).
            "pending": format!("0x{:x}", pending.saturating_mul(rate)),
        }));
    }
    Ok(json!(entries))
}

/// `wallet_strk20PrepareInvoke` — build the call(s) + proof without
/// submitting. Approval-gated: proving reveals the account's decrypted private
/// state to the requesting client via the returned call.
pub async fn handle_prepare_invoke(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let actions = parse_actions(params)?;
    let action = single_action(&actions)?;
    let account = resolve_account(state, client, params).await?;
    let chain = resolve_chain(state, params).await?;
    // `simulate` is accepted for spec compatibility but changes nothing:
    // Tongo's sigma proofs are millisecond-scale and live inside the calldata,
    // so there is no expensive step to skip and the call is submittable.
    let _simulate = params.get("simulate").and_then(|v| v.as_bool()).unwrap_or(false);

    let decision = gated_approval(
        state,
        client,
        "wallet_strk20PrepareInvoke",
        format!(
            "Prepare (no submit) STRK20 action from {} on {}: {}",
            account.address,
            chain_name(chain),
            action.summary()
        ),
    )
    .await;
    if decision == crate::approval::Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    let prepared = prepare_action(state, chain, &account, action).await?;
    Ok(json!({
        // Spec shape: the (primary) pool call + empty proof. Tongo deposits
        // also need the ERC-20 approve that precedes the pool call — the full
        // ordered list is in `calls` (strkd extension); submit them together.
        "call": call_json(prepared.calls.last().expect("at least one call")),
        "calls": prepared.calls.iter().map(call_json).collect::<Vec<_>>(),
        "proof": empty_proof(),
    }))
}

/// `wallet_strk20InvokeTransaction` — prove, get approval, sign and broadcast
/// through the caller's account.
pub async fn handle_invoke_transaction(
    state: &ServerState,
    client: &PairedClient,
    params: &Value,
) -> Result<Value, WalletRpcError> {
    let actions = parse_actions(params)?;
    let action = single_action(&actions)?;
    let account = resolve_account(state, client, params).await?;
    let chain = resolve_chain(state, params).await?;
    let sender = Felt::from_hex(&account.address)
        .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;

    let prepared = prepare_action(state, chain, &account, action).await?;

    // Nonce + fee for the wrapping Starknet transaction (the pool-side nonce
    // is already bound inside the proof).
    let encoded = wallet_core::encode_calls(&prepared.calls);
    let (nonce, bounds) = resolve_exec(state, chain, &sender, &encoded, params, false).await?;

    let decision = gated_approval(
        state,
        client,
        "wallet_strk20InvokeTransaction",
        format!(
            "Submit STRK20 action from {} on {}: {}. Max fee: {}",
            account.address,
            chain_name(chain),
            prepared.summary,
            crate::dispatch::bounds_summary(&bounds)
        ),
    )
    .await;
    if decision == crate::approval::Decision::Reject {
        return Err(WalletRpcError::UserRefused);
    }

    sign_and_submit(
        state,
        chain,
        &account,
        &prepared.calls,
        nonce,
        bounds,
        true,
        Vec::new(),
        None,
    )
    .await
}

fn single_action(actions: &[Strk20Action]) -> Result<&Strk20Action, WalletRpcError> {
    match actions {
        [one] => Ok(one),
        _ => Err(WalletRpcError::InvalidRequest(
            "strkd supports exactly one STRK20 action per transaction: every Tongo operation \
             consumes the pool-side account nonce, so a second bundled action would carry a \
             stale proof. Send actions as separate transactions."
                .into(),
        )),
    }
}
