//! Tests for the temporary ERC-20 sweep (issue #14).
//!
//! Public BIP-39 test vector only — never a real seed.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use wallet_core::{
    oz_address, AccountRef, ChainId, Domain, Felt, Registry, ResourceBounds,
};
use wallet_rpc::node::{FeeBounds, NodeError, StarknetRpc, TxState};
use wallet_rpc::sweep::{self, Confirmation, SweepAccount, SweepEvent, SweepPlan, SweepToken};
use wallet_rpc::WalletSession;

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

const STRK: &str = "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";
const ETH: &str = "0x049d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7";

const ONE: u128 = 1_000_000_000_000_000_000;

/// A plausible full-width destination that is not one of the test accounts.
const DEST: &str = "0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn canon(f: &Felt) -> String {
    format!("0x{:064x}", f)
}

fn tokens() -> Vec<SweepToken> {
    sweep::default_tokens()
}

/// Deterministic node double that records what was broadcast.
struct MockNode {
    /// `(token, holder)` → amount, both canonical hex.
    balances: StdMutex<HashMap<(String, String), u128>>,
    deployed: StdMutex<HashSet<String>>,
    /// Recorded `add_invoke` calls: `(sender, calldata)`.
    invokes: StdMutex<Vec<(String, Vec<Felt>)>>,
    /// Recorded `add_deploy_account` salts (one per deploy).
    deploys: StdMutex<Vec<Felt>>,
    /// Transaction hashes that report as failed rather than accepted.
    fail: HashSet<u64>,
    next_hash: AtomicU64,
    /// Multiplies every fee estimate, to simulate fees moving after planning.
    fee_multiplier: AtomicU64,
}

impl MockNode {
    fn new() -> Self {
        MockNode {
            balances: StdMutex::new(HashMap::new()),
            deployed: StdMutex::new(HashSet::new()),
            invokes: StdMutex::new(Vec::new()),
            deploys: StdMutex::new(Vec::new()),
            fail: HashSet::new(),
            next_hash: AtomicU64::new(1),
            fee_multiplier: AtomicU64::new(1),
        }
    }
    fn failing(mut self, hash: u64) -> Self {
        self.fail.insert(hash);
        self
    }
    fn with_balance(self, token: &str, holder: &str, amount: u128) -> Self {
        let t = canon(&Felt::from_hex(token).unwrap());
        let h = canon(&Felt::from_hex(holder).unwrap());
        self.balances.lock().unwrap().insert((t, h), amount);
        self
    }
    fn with_deployed(self, holder: &str) -> Self {
        self.deployed.lock().unwrap().insert(canon(&Felt::from_hex(holder).unwrap()));
        self
    }
    fn invoke_calldata(&self) -> Vec<Vec<Felt>> {
        self.invokes.lock().unwrap().iter().map(|(_, c)| c.clone()).collect()
    }
    fn bounds(&self) -> FeeBounds {
        // 100×7 + 200_000×11 + 100×3 = 2_201_000 base units of the fee token (×multiplier).
        let m = self.fee_multiplier.load(Ordering::SeqCst) as u128;
        FeeBounds {
            l1_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 7 * m },
            l2_gas: ResourceBounds { max_amount: 200_000, max_price_per_unit: 11 * m },
            l1_data_gas: ResourceBounds { max_amount: 100, max_price_per_unit: 3 * m },
        }
    }
}

#[async_trait]
impl StarknetRpc for MockNode {
    async fn get_nonce(&self, _address: &Felt) -> Result<Felt, NodeError> {
        Ok(Felt::ZERO)
    }
    async fn estimate_invoke(
        &self,
        _sender: &Felt,
        _calldata: &[Felt],
        _nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(self.bounds())
    }
    async fn add_invoke(
        &self,
        sender: &Felt,
        calldata: &[Felt],
        _signature: &[Felt],
        _nonce: &Felt,
        _bounds: &FeeBounds,
        _proof_facts: &[Felt],
        _proof: Option<&str>,
    ) -> Result<Felt, NodeError> {
        self.invokes.lock().unwrap().push((canon(sender), calldata.to_vec()));
        Ok(Felt::from(self.next_hash.fetch_add(1, Ordering::SeqCst)))
    }
    async fn is_deployed(&self, address: &Felt) -> Result<bool, NodeError> {
        Ok(self.deployed.lock().unwrap().contains(&canon(address)))
    }
    async fn balance_of(&self, token: &Felt, holder: &Felt) -> Result<u128, NodeError> {
        Ok(*self
            .balances
            .lock()
            .unwrap()
            .get(&(canon(token), canon(holder)))
            .unwrap_or(&0))
    }
    async fn estimate_deploy_account(
        &self,
        _address: &Felt,
        _class_hash: &Felt,
        _constructor_calldata: &[Felt],
        _salt: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(self.bounds())
    }
    async fn add_deploy_account(
        &self,
        _class_hash: &Felt,
        _constructor_calldata: &[Felt],
        salt: &Felt,
        _signature: &[Felt],
        _bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        self.deploys.lock().unwrap().push(*salt);
        Ok(Felt::from(self.next_hash.fetch_add(1, Ordering::SeqCst)))
    }
    async fn estimate_declare(
        &self,
        _sender: &Felt,
        _compiled_class_hash: &Felt,
        _contract_class: &Value,
        _nonce: &Felt,
    ) -> Result<FeeBounds, NodeError> {
        Ok(self.bounds())
    }
    async fn add_declare(
        &self,
        _sender: &Felt,
        _compiled_class_hash: &Felt,
        _contract_class: &Value,
        _signature: &[Felt],
        _nonce: &Felt,
        _bounds: &FeeBounds,
    ) -> Result<Felt, NodeError> {
        Ok(Felt::ZERO)
    }
    async fn tx_state(&self, tx_hash: &Felt) -> Result<TxState, NodeError> {
        let n: u64 = tx_hash.to_string().parse().unwrap_or(0);
        if self.fail.contains(&n) {
            return Ok(TxState::Failed("reverted in test".into()));
        }
        Ok(TxState::Accepted)
    }
}

/// A session with `n` user accounts derived from the public test vector, plus
/// the sweep inputs (account + deployment data) for each.
fn session_with(n: u32) -> (Arc<Mutex<WalletSession>>, Vec<SweepAccount>) {
    let mut s =
        WalletSession::new_unlocked(ChainId::Sepolia, TEST_MNEMONIC, "pass", Registry::default());
    for i in 0..n {
        s.create_user_account(format!("Account {i}")).unwrap();
    }
    let accounts: Vec<AccountRef> = s.registry().unwrap().accounts.clone();
    let inputs: Vec<SweepAccount> = accounts
        .iter()
        .map(|a| SweepAccount { acct: a.clone(), deployment: s.deployment_data_for(a).unwrap() })
        .collect();
    (Arc::new(Mutex::new(s)), inputs)
}

fn addr_of(i: u32) -> String {
    canon(&oz_address(TEST_MNEMONIC, Domain::User, i, None, ChainId::Sepolia).unwrap())
}

fn noop(_: SweepEvent) {}

/// The confirmation a user gives by accepting exactly this plan, on Sepolia.
fn confirm(plan: &SweepPlan) -> Confirmation {
    Confirmation { fingerprint: plan.fingerprint.clone(), allow_mainnet: false }
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_reports_balances_and_leaves_the_chain_untouched() {
    let (_session, inputs) = session_with(2);
    let a0 = addr_of(0);
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_balance(STRK, &a0, 5 * ONE)
        .with_balance(ETH, &a0, 2 * ONE);

    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();

    let p0 = plan.accounts.iter().find(|a| a.address == a0).unwrap();
    assert!(p0.deployed);
    assert_eq!(p0.balances.len(), 2, "both STRK and ETH are reported");
    assert_eq!(p0.gas, (5 * ONE).to_string());
    assert!(p0.is_funding_source);

    // Totals aggregate across accounts.
    let strk_total = plan.totals.iter().find(|t| t.symbol == "STRK").unwrap();
    assert_eq!(strk_total.amount, (5 * ONE).to_string());

    // Planning signs and broadcasts nothing.
    assert!(node.invokes.lock().unwrap().is_empty());
    assert!(node.deploys.lock().unwrap().is_empty());
}

#[tokio::test]
async fn plan_excludes_the_destination_from_the_sweep() {
    let (_session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_deployed(&a1)
        .with_balance(STRK, &a0, 5 * ONE)
        .with_balance(STRK, &a1, 3 * ONE);

    // Sweep into a1, one of our own accounts.
    let plan = sweep::plan(&node, &inputs, &tokens(), &a1, &a0, "sepolia").await.unwrap();

    let p1 = plan.accounts.iter().find(|a| a.address == a1).unwrap();
    assert!(p1.is_destination);
    assert!(!p1.will_sweep(), "the destination is never swept into itself");
    // Its balance is excluded from the totals.
    let strk_total = plan.totals.iter().find(|t| t.symbol == "STRK").unwrap();
    assert_eq!(strk_total.amount, (5 * ONE).to_string());
    assert!(plan.warnings.iter().any(|w| w.contains("own accounts")));
}

#[tokio::test]
async fn plan_warns_when_the_funding_source_cannot_cover_the_topups() {
    let (_session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    // a1 holds ETH but no gas at all, and a0 (the funding source) is nearly empty.
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_deployed(&a1)
        .with_balance(STRK, &a0, 10)
        .with_balance(ETH, &a1, 2 * ONE);

    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();

    let p1 = plan.accounts.iter().find(|a| a.address == a1).unwrap();
    assert_ne!(p1.needs_gas, "0", "an account with no gas needs a top-up");
    assert!(
        plan.warnings.iter().any(|w| w.contains("funding source holds")),
        "warnings: {:?}",
        plan.warnings
    );
}

#[tokio::test]
async fn execution_order_puts_the_funding_source_last() {
    let (_session, inputs) = session_with(3);
    let (a0, a1, a2) = (addr_of(0), addr_of(1), addr_of(2));
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_deployed(&a1)
        .with_deployed(&a2)
        .with_balance(STRK, &a0, 5 * ONE)
        .with_balance(STRK, &a1, 5 * ONE)
        .with_balance(STRK, &a2, 5 * ONE);

    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    let order = sweep::execution_order(&plan);

    assert_eq!(order.len(), 3);
    assert!(
        order.last().unwrap().is_funding_source,
        "the funding source pays for the others, so it drains last"
    );
}

#[tokio::test]
async fn sweeping_keeps_back_a_fee_reserve_from_the_fee_token() {
    let (session, inputs) = session_with(1);
    let a0 = addr_of(0);
    let held = 5 * ONE;
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, held);

    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    let report =
        sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
            .await
            .unwrap();

    assert_eq!(report.swept, 1, "{:?}", report.outcomes);
    let calldata = node.invoke_calldata();
    assert_eq!(calldata.len(), 1, "one drain, no top-up needed");
    assert!(
        !calldata[0].contains(&Felt::from(held)),
        "the full fee-token balance must never be transferred — the account could not pay the fee"
    );
    // Something substantial did move: within 1% of the balance.
    let moved: u128 = report.outcomes[0].moved[0].amount.parse().unwrap();
    assert!(moved > held / 100 * 99 && moved < held, "moved {moved} of {held}");
}

#[tokio::test]
async fn an_undeployed_account_holding_value_is_funded_and_deployed_first() {
    let (session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    // a1 is undeployed and holds ETH plus a little gas; a0 funds it.
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_balance(STRK, &a0, 100 * ONE)
        .with_balance(STRK, &a1, ONE)
        .with_balance(ETH, &a1, 2 * ONE);

    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    let p1 = plan.accounts.iter().find(|a| a.address == a1).unwrap();
    assert!(p1.needs_deploy, "an undeployed account holding value must be deployed");

    let report =
        sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
            .await
            .unwrap();

    assert_eq!(node.deploys.lock().unwrap().len(), 1, "exactly one deploy");
    assert_eq!(report.failed, 0, "{:?}", report.outcomes);
    let o1 = report.outcomes.iter().find(|o| o.address == a1).unwrap();
    assert_eq!(o1.status, "swept");
    assert!(o1.transactions.len() >= 2, "deploy plus drain: {:?}", o1.transactions);
}

#[tokio::test]
async fn a_blocked_account_is_skipped_without_stopping_the_run() {
    let (session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_deployed(&a1)
        .with_balance(STRK, &a0, 50 * ONE)
        .with_balance(STRK, &a1, 5 * ONE);

    // Sweeping into a1 blocks a1 (it is the destination) but must not stop a0.
    let plan = sweep::plan(&node, &inputs, &tokens(), &a1, &a0, "sepolia").await.unwrap();
    let report =
        sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
            .await
            .unwrap();

    assert_eq!(report.swept, 1, "a0 still swept: {:?}", report.outcomes);
    assert!(report.outcomes.iter().all(|o| o.status != "failed"));
}

#[tokio::test]
async fn accounts_with_nothing_to_move_are_not_touched() {
    let (session, inputs) = session_with(2);
    let a0 = addr_of(0);
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, 5 * ONE);

    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    let report =
        sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
            .await
            .unwrap();

    // Only the funded account is in the report at all.
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(node.deploys.lock().unwrap().len(), 0, "an empty account is never deployed");
}

#[test]
fn amounts_render_at_token_precision() {
    assert_eq!(sweep::format_amount(&(5 * ONE).to_string(), 18), "5");
    assert_eq!(sweep::format_amount(&(ONE / 2).to_string(), 18), "0.5");
    assert_eq!(sweep::format_amount("0", 18), "0");
}

#[test]
fn the_default_token_list_marks_exactly_one_fee_token() {
    let t = sweep::default_tokens();
    assert_eq!(t.iter().filter(|t| t.is_fee_token).count(), 1);
    let fee = t.iter().find(|t| t.is_fee_token).unwrap();
    assert_eq!(fee.symbol, "STRK");
    // The sweep must price fees in the same token the wallet funds agents with.
    assert_eq!(fee.address, wallet_rpc::dispatch::STRK_TOKEN_ADDRESS);
}

// ---------------------------------------------------------------------------
// Review follow-ups (PR #22)
// ---------------------------------------------------------------------------

#[test]
fn implausible_destinations_are_rejected() {
    for bad in [
        "0xdead",                                                              // truncated / typo
        "dead",                                                                // no 0x
        "0xzz",                                                                // not hex
        "0x0800000000000000000000000000000000000000000000000000000000000000", // 2^251
        "0x01234567890123456789012345678901234567890123456789012345678901234", // 65 digits
    ] {
        assert!(sweep::validate_destination(bad).is_err(), "{bad} must be rejected");
    }
    assert!(sweep::validate_destination(DEST).is_ok());
    assert!(sweep::validate_destination(&addr_of(0)).is_ok());
}

#[test]
fn the_token_list_must_have_exactly_one_fee_token_and_it_must_be_strk() {
    let mut two_fee = tokens();
    two_fee[1].is_fee_token = true;
    assert!(sweep::validate_tokens(&two_fee).is_err());

    let mut eth_fee = tokens();
    eth_fee[0].is_fee_token = false;
    eth_fee[1].is_fee_token = true;
    assert!(sweep::validate_tokens(&eth_fee).is_err());

    let mut dup = tokens();
    dup.push(dup[1].clone());
    assert!(sweep::validate_tokens(&dup).is_err());

    assert!(sweep::validate_tokens(&tokens()).is_ok());
}

#[tokio::test]
async fn a_plan_that_changed_since_confirmation_is_refused_before_anything_is_sent() {
    let (session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, 50 * ONE);
    let confirmed = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();

    // a1 receives funds after the user confirmed: the fresh plan would now also
    // deploy and drain a1, which the user never saw.
    node.balances.lock().unwrap().insert((canon(&Felt::from_hex(ETH).unwrap()), a1.clone()), ONE);
    let fresh = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    assert_ne!(fresh.fingerprint, confirmed.fingerprint);

    let err = sweep::execute(&node, &session, &inputs, &tokens(), &fresh, ChainId::Sepolia, &confirm(&confirmed), &noop)
        .await
        .unwrap_err();
    assert!(err.contains("changed since you reviewed"), "{err}");
    assert!(node.invokes.lock().unwrap().is_empty() && node.deploys.lock().unwrap().is_empty());
}

#[tokio::test]
async fn balance_changes_alone_do_not_invalidate_a_confirmation() {
    let (_session, inputs) = session_with(1);
    let a0 = addr_of(0);
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, 50 * ONE);
    let before = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    node.balances.lock().unwrap().insert((canon(&Felt::from_hex(STRK).unwrap()), a0.clone()), 51 * ONE);
    let after = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    assert_eq!(before.fingerprint, after.fingerprint, "a sweep moves whatever is there");
}

#[tokio::test]
async fn mainnet_is_refused_without_explicit_permission() {
    let (session, inputs) = session_with(1);
    let a0 = addr_of(0);
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, 5 * ONE);
    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "mainnet").await.unwrap();
    let err = sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Mainnet, &confirm(&plan), &noop)
        .await
        .unwrap_err();
    assert!(err.contains("not enabled on mainnet"), "{err}");
    assert!(node.invokes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failure_part_way_still_reports_the_transactions_already_sent() {
    let (session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    // a1 is undeployed, holds ETH and no gas: a0 tops it up (tx 1), then the
    // deploy (tx 2) reverts.
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_balance(STRK, &a0, 100 * ONE)
        .with_balance(ETH, &a1, 2 * ONE)
        .failing(2);
    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    let report = sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
        .await
        .unwrap();
    let o1 = report.outcomes.iter().find(|o| o.address == a1).unwrap();
    assert_eq!(o1.status, "failed");
    assert_eq!(o1.transactions.len(), 2, "top-up and deploy were both sent: {o1:?}");
    assert!(o1.detail.as_deref().unwrap().contains("already sent"));
}

#[tokio::test]
async fn top_ups_are_capped_when_fees_jump_after_planning() {
    let (session, inputs) = session_with(2);
    let (a0, a1) = (addr_of(0), addr_of(1));
    let node = MockNode::new()
        .with_deployed(&a0)
        .with_deployed(&a1)
        .with_balance(STRK, &a0, 100 * ONE)
        .with_balance(ETH, &a1, 2 * ONE);
    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    // The node now claims fees cost 10× what it said a moment ago.
    node.fee_multiplier.store(10, Ordering::SeqCst);
    let report = sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
        .await
        .unwrap();
    let o1 = report.outcomes.iter().find(|o| o.address == a1).unwrap();
    assert_eq!(o1.status, "failed", "{o1:?}");
    assert!(o1.detail.as_deref().unwrap().contains("refusing to fund"));
    // No top-up to a1 was sent; only a0's own drain went out.
    let a1_felt = Felt::from_hex(&a1).unwrap();
    assert!(
        node.invoke_calldata().iter().all(|c| !c.contains(&a1_felt)),
        "no transfer may target a1"
    );
}

#[tokio::test]
async fn the_plan_reports_what_stays_behind_as_fee_reserve() {
    let (_session, inputs) = session_with(1);
    let a0 = addr_of(0);
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, 5 * ONE);
    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    assert_ne!(plan.left_behind, "0");
    assert_eq!(plan.accounts[0].fee_reserve, plan.left_behind);
    assert!(plan.warnings.iter().any(|w| w.contains("stays behind")), "{:?}", plan.warnings);
    // The destination is undeployed in this mock: the user is warned.
    assert!(plan.warnings.iter().any(|w| w.contains("no contract deployed")));
}

#[tokio::test]
async fn a_swept_account_reports_its_left_behind_reserve() {
    let (session, inputs) = session_with(1);
    let a0 = addr_of(0);
    let held = 5 * ONE;
    let node = MockNode::new().with_deployed(&a0).with_balance(STRK, &a0, held);
    let plan = sweep::plan(&node, &inputs, &tokens(), DEST, &a0, "sepolia").await.unwrap();
    let report = sweep::execute(&node, &session, &inputs, &tokens(), &plan, ChainId::Sepolia, &confirm(&plan), &noop)
        .await
        .unwrap();
    let o = &report.outcomes[0];
    let moved: u128 = o.moved[0].amount.parse().unwrap();
    let left: u128 = o.left_behind[0].amount.parse().unwrap();
    assert_eq!(moved + left, held, "everything is either moved or reported as left behind");
}
