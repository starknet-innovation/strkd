//! Temporary asset recovery — consolidate ERC-20 balances into one address.
//!
//! **This module is temporary by construction.** It exists so assets can be
//! recovered before the account-derivation change in
//! [issue #16](https://github.com/starknet-innovation/strkd/issues/16) moves
//! every address. Delete it after the cutover; see
//! `docs/project/bramble-convergence.md` §5.1.
//!
//! ## Why this is the riskiest code in the wallet
//!
//! It moves every ERC-20 balance the wallet can reach to an address the user
//! types in, it deploys accounts, and it spends from the funding source. There
//! is no undo. The consent model is therefore *plan first*: [`plan`] is strictly
//! read-only and shows exactly what would move and what it will cost, and
//! [`execute`] runs only against a plan the user has confirmed.
//!
//! ## Ordering
//!
//! The fee token is also a swept asset, so order matters. Within an account,
//! non-fee tokens move at their full balance and the fee token moves last, net
//! of a reserve that covers the transfer itself. Across accounts the funding
//! source goes last, because it pays for everyone else's deploys and top-ups.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use wallet_core::{AccountRef, Call, ChainId, DeploymentData, Felt, InvokeV3Params};

use crate::node::{FeeBounds, StarknetRpc, TxState};
use crate::session::WalletSession;

/// How long to wait for one transaction to reach a block before giving up.
const TX_WAIT_SECS: u64 = 180;
/// Poll interval while waiting.
const TX_POLL_MS: u64 = 3_000;
/// Head-room over an estimated fee, in percent. The node client's estimate
/// already carries its own margin; this covers drift between estimating and
/// landing, and a fee-token transfer that reserves too little simply reverts.
const FEE_MARGIN_PCT: u128 = 200;

// ---------------------------------------------------------------------------
// Inputs and outputs
// ---------------------------------------------------------------------------

/// An ERC-20 the sweep looks at.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SweepToken {
    /// Contract address, hex.
    pub address: String,
    pub symbol: String,
    pub decimals: u8,
    /// Whether this is the fee token (STRK). The fee token is swept last and
    /// keeps back enough to pay for its own transfer.
    #[serde(default)]
    pub is_fee_token: bool,
}

/// One account plus the deployment data needed to deploy it. Deployment data
/// comes from the unlocked session, so the caller supplies it rather than this
/// module reaching for the seed.
#[derive(Debug, Clone)]
pub struct SweepAccount {
    pub acct: AccountRef,
    pub deployment: DeploymentData,
}

/// The tokens swept by default.
///
/// Addresses come from bramble's `DEFAULT_TOKENS`
/// (`packages/foundation/wallet-model/src/networks/networks.ts`), the same
/// values on mainnet and Sepolia, rather than being written from memory. The
/// STRK entry matches [`crate::dispatch::STRK_TOKEN_ADDRESS`] exactly.
///
/// Deliberately short: anything else the user holds is added through the UI,
/// because guessing a token address is how funds reach the wrong contract.
pub fn default_tokens() -> Vec<SweepToken> {
    vec![
        SweepToken {
            address: crate::dispatch::STRK_TOKEN_ADDRESS.to_string(),
            symbol: "STRK".into(),
            decimals: 18,
            is_fee_token: true,
        },
        SweepToken {
            address: "0x049d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7".into(),
            symbol: "ETH".into(),
            decimals: 18,
            is_fee_token: false,
        },
    ]
}

/// A non-zero balance held by one account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    pub symbol: String,
    pub token: String,
    /// Raw amount in the token's smallest unit, as a string — u128 exceeds
    /// JavaScript's integer precision.
    pub amount: String,
}

/// What the sweep found for one account, and what stands between it and sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountPlan {
    pub address: String,
    pub label: String,
    pub domain: String,
    pub index: u32,
    pub deployed: bool,
    /// True for the account that pays for everyone else's deploys and top-ups.
    pub is_funding_source: bool,
    /// True when this account *is* the destination, so there is nothing to move.
    pub is_destination: bool,
    pub balances: Vec<TokenBalance>,
    /// Fee-token balance, in the fee token's smallest unit, as a string.
    pub gas: String,
    /// Deploy required before this account can send.
    pub needs_deploy: bool,
    /// Fee-token top-up needed before this account can send, as a string.
    /// `"0"` when it can already pay its own way.
    pub needs_gas: String,
    /// Reasons this account cannot be swept as things stand.
    pub blockers: Vec<String>,
}

impl AccountPlan {
    /// Whether this account holds anything worth moving.
    pub fn has_value(&self) -> bool {
        !self.balances.is_empty()
    }
    /// Whether the sweep will attempt this account at all.
    pub fn will_sweep(&self) -> bool {
        self.has_value() && !self.is_destination && self.blockers.is_empty()
    }
}

/// The read-only survey shown to the user before anything is signed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepPlan {
    pub destination: String,
    pub network: String,
    pub accounts: Vec<AccountPlan>,
    /// Per-token totals across every account that will be swept.
    pub totals: Vec<TokenBalance>,
    pub funding_source: String,
    /// Fee-token balance of the funding source, as a string.
    pub funding_available: String,
    /// Total top-ups the funding source is expected to pay, as a string.
    pub funding_required: String,
    /// What the user needs to know before confirming.
    pub warnings: Vec<String>,
}

/// Progress emitted while [`execute`] runs, so a long sweep is not a silent spinner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SweepEvent {
    Started { accounts: usize },
    Step { address: String, label: String, step: String },
    Sent { address: String, what: String, tx: String },
    Waiting { tx: String },
    Skipped { address: String, why: String },
    Failed { address: String, why: String },
    Done { swept: usize, skipped: usize, failed: usize },
}

/// What actually happened to one account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountOutcome {
    pub address: String,
    pub label: String,
    /// `"swept"`, `"skipped"` or `"failed"`.
    pub status: String,
    /// Transaction hashes produced for this account, in order.
    pub transactions: Vec<String>,
    pub moved: Vec<TokenBalance>,
    pub detail: Option<String>,
}

/// The result of a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepReport {
    pub destination: String,
    pub network: String,
    pub outcomes: Vec<AccountOutcome>,
    pub swept: usize,
    pub skipped: usize,
    pub failed: usize,
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Ceiling on what a set of resource bounds can cost.
///
/// `max_amount × max_price_per_unit` summed over the three resources — the most
/// the transaction can spend. Saturating throughout: a bogus bound should read
/// as a huge number rather than wrap to a small one and under-reserve the fee.
pub fn max_fee(b: &FeeBounds) -> u128 {
    let one =
        |r: &wallet_core::ResourceBounds| (r.max_amount as u128).saturating_mul(r.max_price_per_unit);
    one(&b.l1_gas)
        .saturating_add(one(&b.l2_gas))
        .saturating_add(one(&b.l1_data_gas))
}

fn with_margin(fee: u128) -> u128 {
    fee.saturating_mul(FEE_MARGIN_PCT).saturating_div(100)
}

fn felt_of(hex: &str, what: &str) -> Result<Felt, String> {
    Felt::from_hex(hex).map_err(|_| format!("bad {what}: {hex}"))
}

/// Canonical zero-padded form, so addresses compare as strings.
fn canon(f: &Felt) -> String {
    format!("0x{:064x}", f)
}

/// Human-readable amount for a raw token quantity, for prompts and reports.
pub fn format_amount(raw: &str, decimals: u8) -> String {
    let v: u128 = raw.parse().unwrap_or(0);
    let scale = 10u128.checked_pow(decimals as u32).unwrap_or(1);
    let (whole, frac) = (v / scale, v % scale);
    if frac == 0 {
        return whole.to_string();
    }
    let frac_str = format!("{:0width$}", frac, width = decimals as usize);
    format!("{whole}.{}", frac_str.trim_end_matches('0'))
}

/// Chain label used in plans and reports.
pub fn chain_label(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Mainnet => "mainnet",
        ChainId::Sepolia => "sepolia",
    }
}

/// Read every token balance for one holder. Returns non-zero balances plus the
/// fee-token balance (which is reported even when zero, since it gates sending).
async fn read_balances(
    node: &dyn StarknetRpc,
    holder: &Felt,
    tokens: &[SweepToken],
) -> Result<(Vec<TokenBalance>, u128), String> {
    let mut balances = Vec::new();
    let mut gas = 0u128;
    for t in tokens {
        let token = felt_of(&t.address, &format!("token {} address", t.symbol))?;
        let amount = node
            .balance_of(&token, holder)
            .await
            .map_err(|e| format!("{}: {e}", t.symbol))?;
        if t.is_fee_token {
            gas = amount;
        }
        if amount > 0 {
            balances.push(TokenBalance {
                symbol: t.symbol.clone(),
                token: t.address.clone(),
                amount: amount.to_string(),
            });
        }
    }
    Ok((balances, gas))
}

/// Build `transfer(destination, amount)` calls for the given balances.
///
/// Non-fee tokens go first at their full balance, then the fee token at
/// `fee_token_amount`. `None` omits the fee token entirely — used when pricing
/// a call before the reserve is known.
fn transfer_calls(
    to: &Felt,
    tokens: &[SweepToken],
    balances: &[TokenBalance],
    fee_token_amount: Option<u128>,
) -> Result<Vec<Call>, String> {
    let selector = wallet_core::get_selector_from_name("transfer");
    let mut calls = Vec::new();
    for fee_pass in [false, true] {
        for b in balances {
            let t = tokens
                .iter()
                .find(|t| t.address == b.token)
                .ok_or_else(|| format!("token {} is not in the token list", b.token))?;
            if t.is_fee_token != fee_pass {
                continue;
            }
            let held: u128 = b.amount.parse().map_err(|_| "unreadable balance".to_string())?;
            let amount = if t.is_fee_token {
                match fee_token_amount {
                    None => continue,
                    Some(a) => a.min(held),
                }
            } else {
                held
            };
            if amount == 0 {
                continue;
            }
            calls.push(Call {
                to: felt_of(&t.address, "token address")?,
                selector,
                calldata: vec![*to, Felt::from(amount), Felt::ZERO],
            });
        }
    }
    Ok(calls)
}

/// Invoke params for a plain (non-proof-carrying) V3 transaction.
fn invoke_params(nonce: Felt, bounds: &FeeBounds) -> InvokeV3Params {
    InvokeV3Params {
        nonce,
        tip: 0,
        l1_gas: bounds.l1_gas,
        l2_gas: bounds.l2_gas,
        l1_data_gas: bounds.l1_data_gas,
        ..Default::default()
    }
}

/// Poll until `hash` is in a block, or time out.
async fn wait_accepted(
    node: &dyn StarknetRpc,
    hash: &Felt,
    on_event: &(dyn Fn(SweepEvent) + Send + Sync),
) -> Result<(), String> {
    let tx = format!("0x{:x}", hash);
    on_event(SweepEvent::Waiting { tx: tx.clone() });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TX_WAIT_SECS);
    loop {
        match node.tx_state(hash).await {
            Ok(TxState::Accepted) => return Ok(()),
            Ok(TxState::Failed(why)) => return Err(format!("{tx} failed on-chain: {why}")),
            // A transient lookup error must not abandon a broadcast transaction.
            Ok(TxState::Pending) | Err(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "{tx} did not reach a block within {TX_WAIT_SECS}s. It may still land — check the \
                 explorer before re-running, or the retry may spend the fee twice."
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(TX_POLL_MS)).await;
    }
}

// ---------------------------------------------------------------------------
// Plan — read-only
// ---------------------------------------------------------------------------

/// Survey every account: what it holds, whether it can send, and what the
/// funding source will have to cover. Makes **no** state changes and signs
/// nothing.
pub async fn plan(
    node: &dyn StarknetRpc,
    accounts: &[SweepAccount],
    tokens: &[SweepToken],
    destination: &str,
    funding_source: &str,
    network: &str,
) -> Result<SweepPlan, String> {
    let dest = felt_of(destination, "destination address")?;
    if dest == Felt::ZERO {
        return Err("destination address must not be zero".into());
    }
    let dest_c = canon(&dest);
    let funding_c = canon(&felt_of(funding_source, "funding source address")?);

    let mut warnings = Vec::new();
    if !tokens.iter().any(|t| t.is_fee_token) {
        warnings.push(
            "no fee token is marked in the token list, so gas top-ups and deploys cannot be priced"
                .into(),
        );
    }

    let mut plans = Vec::new();
    let mut totals: BTreeMap<String, (String, u128)> = BTreeMap::new();
    let mut funding_required = 0u128;
    let mut funding_available = 0u128;

    for input in accounts {
        let acct = &input.acct;
        let addr = felt_of(&acct.address, "account address")?;
        let addr_c = canon(&addr);
        let is_dest = addr_c == dest_c;
        let is_funding = addr_c == funding_c;

        let deployed = node
            .is_deployed(&addr)
            .await
            .map_err(|e| format!("{}: {e}", acct.address))?;
        let (balances, gas) = read_balances(node, &addr, tokens)
            .await
            .map_err(|e| format!("{}: {e}", acct.address))?;

        if is_funding {
            funding_available = gas;
        }

        let mut blockers = Vec::new();
        let mut needs_gas = 0u128;
        let needs_deploy = !deployed && !balances.is_empty() && !is_dest;

        if !balances.is_empty() && !is_dest {
            let mut required = 0u128;

            if !deployed {
                let d = &input.deployment;
                match node
                    .estimate_deploy_account(&d.address, &d.class_hash, &d.constructor_calldata, &d.salt)
                    .await
                {
                    Ok(b) => required = required.saturating_add(max_fee(&b)),
                    Err(e) => blockers.push(format!("cannot price the deploy: {e}")),
                }
            }

            // Price the transfer. An undeployed account cannot be simulated, so
            // its transfer is approximated by a second deploy-sized fee — this
            // only sizes the top-up, which carries its own margin, and the real
            // fee is estimated again after the deploy lands.
            if deployed {
                match price_transfer(node, &addr, &dest, tokens, &balances, gas).await {
                    Ok(f) => required = required.saturating_add(f),
                    Err(e) => blockers.push(format!("cannot price the transfer: {e}")),
                }
            } else {
                required = required.saturating_mul(2);
            }

            let required = with_margin(required);
            if required > gas {
                needs_gas = required - gas;
                if !is_funding {
                    funding_required = funding_required.saturating_add(needs_gas);
                }
            }
        }

        if is_dest {
            blockers.push("this is the destination — nothing to move".into());
        }
        if is_funding && needs_gas > 0 {
            blockers
                .push("the funding source cannot top itself up — send it more of the fee token".into());
        }

        if !is_dest {
            for b in &balances {
                let e = totals.entry(b.token.clone()).or_insert_with(|| (b.symbol.clone(), 0));
                e.1 = e.1.saturating_add(b.amount.parse::<u128>().unwrap_or(0));
            }
        }

        plans.push(AccountPlan {
            address: acct.address.clone(),
            label: acct.label.clone(),
            domain: format!("{:?}", acct.domain).to_lowercase(),
            index: acct.index,
            deployed,
            is_funding_source: is_funding,
            is_destination: is_dest,
            balances,
            gas: gas.to_string(),
            needs_deploy,
            needs_gas: needs_gas.to_string(),
            blockers,
        });
    }

    if funding_required > funding_available {
        warnings.push(format!(
            "the funding source holds {funding_available} but needs about {funding_required} \
             (fee-token base units) to cover deploys and top-ups. Top it up first or some accounts \
             will fail."
        ));
    }
    if plans.iter().any(|p| p.needs_deploy) {
        warnings.push(
            "some accounts holding value are not deployed. Deploying them is irreversible and is \
             paid for by the funding source."
                .into(),
        );
    }
    if plans.iter().any(|p| p.has_value() && p.is_destination) {
        warnings
            .push("the destination is one of this wallet's own accounts; it will not be swept".into());
    }

    Ok(SweepPlan {
        destination: dest_c,
        network: network.to_string(),
        accounts: plans,
        totals: totals
            .into_iter()
            .map(|(token, (symbol, amount))| TokenBalance {
                symbol,
                token,
                amount: amount.to_string(),
            })
            .collect(),
        funding_source: funding_c,
        funding_available: funding_available.to_string(),
        funding_required: funding_required.to_string(),
        warnings,
    })
}

/// Estimated cost of moving `balances` out of a deployed account.
///
/// Prices the call with the fee token at half its balance — an amount the
/// account can certainly afford, so the simulation does not fail for want of
/// funds while it is being measured. The amount barely affects the gas, so this
/// is a good estimate of the real transfer.
async fn price_transfer(
    node: &dyn StarknetRpc,
    from: &Felt,
    to: &Felt,
    tokens: &[SweepToken],
    balances: &[TokenBalance],
    gas: u128,
) -> Result<u128, String> {
    let calls = transfer_calls(to, tokens, balances, Some(gas / 2))?;
    if calls.is_empty() {
        return Ok(0);
    }
    let encoded = wallet_core::encode_calls(&calls);
    let nonce = node.get_nonce(from).await.map_err(|e| e.to_string())?;
    let bounds = node
        .estimate_invoke(from, &encoded, &nonce)
        .await
        .map_err(|e| e.to_string())?;
    Ok(max_fee(&bounds))
}

/// Order accounts for execution: everything else first, the funding source last.
pub fn execution_order(plan: &SweepPlan) -> Vec<&AccountPlan> {
    let mut v: Vec<&AccountPlan> =
        plan.accounts.iter().filter(|a| a.has_value() && !a.is_destination).collect();
    v.sort_by_key(|a| a.is_funding_source);
    v
}

// ---------------------------------------------------------------------------
// Execute
// ---------------------------------------------------------------------------

/// Run the sweep described by `plan`.
///
/// Irreversible. Every account that holds value is topped up and deployed as
/// needed, then drained to `plan.destination`. Accounts with blockers are
/// skipped rather than attempted. One account failing does not stop the rest —
/// each is independent, and stopping early would strand accounts already funded.
pub async fn execute(
    node: &dyn StarknetRpc,
    session: &Mutex<WalletSession>,
    accounts: &[SweepAccount],
    tokens: &[SweepToken],
    plan: &SweepPlan,
    chain: ChainId,
    on_event: &(dyn Fn(SweepEvent) + Send + Sync),
) -> Result<SweepReport, String> {
    let dest = felt_of(&plan.destination, "destination address")?;
    let funding = accounts
        .iter()
        .find(|a| canon(&felt_of(&a.acct.address, "account").unwrap_or(Felt::ZERO)) == plan.funding_source)
        .cloned();

    let targets = execution_order(plan);
    on_event(SweepEvent::Started { accounts: targets.len() });

    let mut outcomes = Vec::new();
    let (mut swept, mut skipped, mut failed) = (0usize, 0usize, 0usize);

    for ap in targets {
        if !ap.blockers.is_empty() {
            let why = ap.blockers.join("; ");
            on_event(SweepEvent::Skipped { address: ap.address.clone(), why: why.clone() });
            skipped += 1;
            outcomes.push(AccountOutcome {
                address: ap.address.clone(),
                label: ap.label.clone(),
                status: "skipped".into(),
                transactions: Vec::new(),
                moved: Vec::new(),
                detail: Some(why),
            });
            continue;
        }

        let input = match accounts.iter().find(|a| a.acct.address == ap.address) {
            Some(i) => i,
            None => continue,
        };

        match sweep_one(node, session, input, ap, funding.as_ref(), tokens, &dest, chain, on_event)
            .await
        {
            Ok(outcome) => {
                swept += 1;
                outcomes.push(outcome);
            }
            Err(why) => {
                on_event(SweepEvent::Failed { address: ap.address.clone(), why: why.clone() });
                failed += 1;
                outcomes.push(AccountOutcome {
                    address: ap.address.clone(),
                    label: ap.label.clone(),
                    status: "failed".into(),
                    transactions: Vec::new(),
                    moved: Vec::new(),
                    detail: Some(why),
                });
            }
        }
    }

    on_event(SweepEvent::Done { swept, skipped, failed });
    Ok(SweepReport {
        destination: plan.destination.clone(),
        network: plan.network.clone(),
        outcomes,
        swept,
        skipped,
        failed,
    })
}

/// Deploy if needed, top up if needed, then drain one account.
#[allow(clippy::too_many_arguments)]
async fn sweep_one(
    node: &dyn StarknetRpc,
    session: &Mutex<WalletSession>,
    input: &SweepAccount,
    ap: &AccountPlan,
    funding: Option<&SweepAccount>,
    tokens: &[SweepToken],
    dest: &Felt,
    chain: ChainId,
    on_event: &(dyn Fn(SweepEvent) + Send + Sync),
) -> Result<AccountOutcome, String> {
    let acct = &input.acct;
    let addr = felt_of(&acct.address, "account address")?;
    let mut txs: Vec<String> = Vec::new();
    let step = |s: &str| {
        on_event(SweepEvent::Step {
            address: acct.address.clone(),
            label: acct.label.clone(),
            step: s.to_string(),
        })
    };

    // ---- 1. Deploy, if the account has never been deployed. ----------------
    if !ap.deployed {
        step("pricing the deploy");
        let d = &input.deployment;
        let bounds = node
            .estimate_deploy_account(&d.address, &d.class_hash, &d.constructor_calldata, &d.salt)
            .await
            .map_err(|e| format!("deploy estimate failed: {e}"))?;
        let need = with_margin(max_fee(&bounds));

        let (_, gas) = read_balances(node, &addr, tokens).await?;
        if gas < need {
            let top_up = need - gas;
            step("funding the deploy");
            let tx =
                fund(node, session, funding, &addr, top_up, tokens, chain, on_event).await?;
            txs.push(tx);
        }

        step("deploying");
        let signed = {
            let s = session.lock().await;
            s.sign_deploy_account_for(acct, chain, &invoke_params(Felt::ZERO, &bounds))
                .map_err(|e| format!("signing the deploy failed: {e}"))?
        };
        let hash = node
            .add_deploy_account(
                &signed.class_hash,
                &signed.constructor_calldata,
                &signed.salt,
                &signed.signature,
                &bounds,
            )
            .await
            .map_err(|e| format!("broadcasting the deploy failed: {e}"))?;
        let tx = format!("0x{:x}", hash);
        on_event(SweepEvent::Sent {
            address: acct.address.clone(),
            what: "deploy".into(),
            tx: tx.clone(),
        });
        txs.push(tx);
        wait_accepted(node, &hash, on_event).await?;
    }

    // ---- 2. Re-read balances; funding and deploying both moved them. -------
    step("reading balances");
    let (balances, gas) = read_balances(node, &addr, tokens).await?;
    if balances.is_empty() {
        return Ok(AccountOutcome {
            address: acct.address.clone(),
            label: acct.label.clone(),
            status: "skipped".into(),
            transactions: txs,
            moved: Vec::new(),
            detail: Some("nothing left to move".into()),
        });
    }

    // ---- 3. Price the drain, topping up if it cannot pay for itself. -------
    step("pricing the transfer");
    let mut fee = price_transfer(node, &addr, dest, tokens, &balances, gas).await?;
    let mut reserve = with_margin(fee);
    let mut gas = gas;

    if gas < reserve {
        let top_up = reserve - gas;
        step("funding the transfer");
        let tx = fund(node, session, funding, &addr, top_up, tokens, chain, on_event).await?;
        txs.push(tx);
        let (_, g) = read_balances(node, &addr, tokens).await?;
        gas = g;
        fee = price_transfer(node, &addr, dest, tokens, &balances, gas).await?;
        reserve = with_margin(fee);
    }

    // Balances changed if we just topped up, so re-read before building the
    // final calls — otherwise the fee-token transfer would use a stale amount.
    let (balances, gas_now) = read_balances(node, &addr, tokens).await?;
    let fee_amount = gas_now.saturating_sub(reserve);

    let calls = transfer_calls(dest, tokens, &balances, Some(fee_amount))?;
    if calls.is_empty() {
        return Ok(AccountOutcome {
            address: acct.address.clone(),
            label: acct.label.clone(),
            status: "skipped".into(),
            transactions: txs,
            moved: Vec::new(),
            detail: Some(format!(
                "balance {gas_now} does not cover its own transfer fee of about {reserve}"
            )),
        });
    }

    // ---- 4. Sign and broadcast the drain. ----------------------------------
    step("sweeping");
    let encoded = wallet_core::encode_calls(&calls);
    let nonce = node.get_nonce(&addr).await.map_err(|e| e.to_string())?;
    let bounds = node
        .estimate_invoke(&addr, &encoded, &nonce)
        .await
        .map_err(|e| format!("transfer estimate failed: {e}"))?;
    let signed = {
        let s = session.lock().await;
        s.sign_invoke_for(acct, &calls, chain, &invoke_params(nonce, &bounds))
            .map_err(|e| format!("signing the transfer failed: {e}"))?
    };
    let hash = node
        .add_invoke(&addr, &signed.calldata, &signed.signature, &nonce, &bounds, &[], None)
        .await
        .map_err(|e| format!("broadcasting the transfer failed: {e}"))?;
    let tx = format!("0x{:x}", hash);
    on_event(SweepEvent::Sent {
        address: acct.address.clone(),
        what: "sweep".into(),
        tx: tx.clone(),
    });
    txs.push(tx);
    wait_accepted(node, &hash, on_event).await?;

    // What actually left the account: the calls we sent.
    let moved = balances
        .iter()
        .filter_map(|b| {
            let t = tokens.iter().find(|t| t.address == b.token)?;
            let amount = if t.is_fee_token { fee_amount } else { b.amount.parse().ok()? };
            (amount > 0).then(|| TokenBalance {
                symbol: b.symbol.clone(),
                token: b.token.clone(),
                amount: amount.to_string(),
            })
        })
        .collect();

    Ok(AccountOutcome {
        address: acct.address.clone(),
        label: acct.label.clone(),
        status: "swept".into(),
        transactions: txs,
        moved,
        detail: None,
    })
}

/// Send `amount` of the fee token from the funding source to `to`, and wait for
/// it to land. The recipient cannot act until it does.
#[allow(clippy::too_many_arguments)]
async fn fund(
    node: &dyn StarknetRpc,
    session: &Mutex<WalletSession>,
    funding: Option<&SweepAccount>,
    to: &Felt,
    amount: u128,
    tokens: &[SweepToken],
    chain: ChainId,
    on_event: &(dyn Fn(SweepEvent) + Send + Sync),
) -> Result<String, String> {
    let funding = funding.ok_or("no funding source is available to pay for this")?;
    let fee_token = tokens
        .iter()
        .find(|t| t.is_fee_token)
        .ok_or("no fee token is marked in the token list")?;
    let from = felt_of(&funding.acct.address, "funding source address")?;

    if from == *to {
        return Err("the funding source cannot fund itself".into());
    }

    let calls = vec![Call {
        to: felt_of(&fee_token.address, "fee token address")?,
        selector: wallet_core::get_selector_from_name("transfer"),
        calldata: vec![*to, Felt::from(amount), Felt::ZERO],
    }];
    let encoded = wallet_core::encode_calls(&calls);
    let nonce = node.get_nonce(&from).await.map_err(|e| e.to_string())?;
    let bounds = node
        .estimate_invoke(&from, &encoded, &nonce)
        .await
        .map_err(|e| format!("funding estimate failed: {e}"))?;
    let signed = {
        let s = session.lock().await;
        s.sign_invoke_for(&funding.acct, &calls, chain, &invoke_params(nonce, &bounds))
            .map_err(|e| format!("signing the top-up failed: {e}"))?
    };
    let hash = node
        .add_invoke(&from, &signed.calldata, &signed.signature, &nonce, &bounds, &[], None)
        .await
        .map_err(|e| format!("broadcasting the top-up failed: {e}"))?;
    let tx = format!("0x{:x}", hash);
    on_event(SweepEvent::Sent {
        address: funding.acct.address.clone(),
        what: format!("top up {}", canon(to)),
        tx: tx.clone(),
    });
    wait_accepted(node, &hash, on_event).await?;
    Ok(tx)
}
