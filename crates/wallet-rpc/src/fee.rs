//! Fee bounds (#61): turning a node fee estimate into V3 resource bounds,
//! fitting them to the paying account's STRK balance, and naming the node's
//! "bounds exceed balance" refusal.
//!
//! A V3 transaction is charged `amount used × the block's price` per resource;
//! its bounds are only maxima. The node refuses a transaction whose **max fee**
//! (`Σ max_amount × max_price_per_unit`) exceeds the payer's balance, so padding
//! both factors by 1.5× (Bramble Companion ≤ 0.4.0) asked for ~2.25× the estimate in balance.
//! Now the amount and the price are padded separately:
//!
//! - **amount** × [`AMOUNT_MARGIN_PCT`] (1.5×): estimates run with SKIP_VALIDATE,
//!   so they omit `__validate__`, and running out of gas reverts the transaction
//!   *and charges for it*. This is the margin that protects funds.
//! - **price** × a smaller per-resource headroom: the transaction pays the
//!   block's price, so the bound only has to cover the price moving between the
//!   estimate and inclusion. A block price above the bound keeps the
//!   transaction out of the block, which costs nothing.
//!
//! The price headrooms come from block headers (`l1_gas_price`, `l2_gas_price`,
//! `l1_data_gas_price` in fri), sampled 2026-11-15 on Sepolia and mainnet: 8
//! windows of 300 consecutive blocks per network, spread over ~9 days (2 s
//! blocks). Largest rise of a price at block `n + k` over block `n`:
//!
//! | resource    | k ≤ 20 (~40 s) Sepolia / mainnet | k ≤ 299 (~10 min, a slow approval) | headroom |
//! |-------------|----------------------------------|------------------------------------|----------|
//! | l1_gas      | 0.00 % / 0.49 %                  | 0.00 % / 1.78 %                    | 1.5×     |
//! | l2_gas      | 0.36 % / 0.60 %                  | 0.36 % / 0.72 %                    | 1.2×     |
//! | l1_data_gas | 0.04 % / 0.99 %                  | 0.04 % / 4.35 %                    | 2×       |
//!
//! Within minutes prices barely move, but between windows they jumped: mainnet
//! L1 gas 5.8e13–7.9e13 fri and L1 data gas 1.9e10–2.2e11 fri (12×), Sepolia
//! L1 data gas from 5e4 to 5.2e12 fri. L2 gas, which dominates almost every fee
//! (82M of it for a STRK20 batch), gets 1.2×; the L1 resources, which cost
//! little in balance (L1 gas is usually 0 with blob DA, L1 data gas a few
//! thousand units), keep wide headroom for those spikes.

use serde_json::Value;
use wallet_core::{Felt, ResourceBounds};

use crate::error::WalletRpcError;
use crate::node::{FeeBounds, NodeError, StarknetRpc};

/// Headroom on the estimated gas amounts (×100).
pub const AMOUNT_MARGIN_PCT: u128 = 150;
/// Headroom on the estimate's L1 gas price (×100).
pub const L1_GAS_PRICE_HEADROOM_PCT: u128 = 150;
/// Headroom on the estimate's L2 gas price (×100).
pub const L2_GAS_PRICE_HEADROOM_PCT: u128 = 120;
/// Headroom on the estimate's L1 data gas price (×100).
pub const L1_DATA_GAS_PRICE_HEADROOM_PCT: u128 = 200;

/// The least amount margin Bramble Companion tightens to when the balance can't cover the
/// padded bounds (×100). Below it a transaction risks reverting out of gas,
/// which is charged.
pub const MIN_AMOUNT_MARGIN_PCT: u128 = 110;
/// The least price headroom Bramble Companion tightens to (×100), per resource: above the
/// largest 10-minute rise measured (4.35 %).
pub const MIN_PRICE_HEADROOM_PCT: u128 = 105;

/// `data.reason` when the payer's balance can't cover even the tightened
/// bounds, or when the node refuses the bounds as exceeding the balance.
pub const INSUFFICIENT_BALANCE_FOR_BOUNDS: &str = "INSUFFICIENT_BALANCE_FOR_BOUNDS";

/// STRK, the V3 fee token (same address on mainnet and Sepolia).
const FEE_TOKEN: &str = "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";

/// One resource of a fee estimate: the amount consumed and the block's price,
/// unpadded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceEstimate {
    pub amount: u128,
    pub price: u128,
}

/// A node fee estimate, unpadded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FeeEstimate {
    pub l1_gas: ResourceEstimate,
    pub l2_gas: ResourceEstimate,
    pub l1_data_gas: ResourceEstimate,
}

/// Per-resource price headrooms, in the order l1_gas, l2_gas, l1_data_gas.
const PRICE_HEADROOM_PCT: [u128; 3] = [
    L1_GAS_PRICE_HEADROOM_PCT,
    L2_GAS_PRICE_HEADROOM_PCT,
    L1_DATA_GAS_PRICE_HEADROOM_PCT,
];

fn pct(v: u128, pct: u128) -> u128 {
    // `v × pct / 100` without overflowing for any u128 `v`.
    (v / 100).saturating_mul(pct).saturating_add((v % 100) * pct / 100)
}

/// Inverse of [`pct`] for `pct ≥ 100`: the least `v` with `pct(v) ≥ padded`
/// (exact for values [`pct`] produced).
fn unpct(padded: u128, pct: u128) -> u128 {
    // ceil(padded × 100 / pct), overflow-free.
    let q = padded / pct;
    let r = padded % pct;
    q.saturating_mul(100).saturating_add((r * 100).div_ceil(pct))
}

/// A u128 gas amount as the u64 `max_amount` field, saturating instead of
/// truncating.
fn amount_u64(v: u128) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// `Σ max_amount × max_price_per_unit`, saturating: what the node checks
/// against the payer's balance.
pub fn max_fee(b: &FeeBounds) -> u128 {
    [b.l1_gas, b.l2_gas, b.l1_data_gas]
        .iter()
        .map(|r| u128::from(r.max_amount).saturating_mul(r.max_price_per_unit))
        .fold(0u128, u128::saturating_add)
}

impl FeeEstimate {
    fn resources(&self) -> [ResourceEstimate; 3] {
        [self.l1_gas, self.l2_gas, self.l1_data_gas]
    }

    /// Parse a spec 0.8+ `FEE_ESTIMATE` object (`*_consumed`, `*_price`, hex).
    pub fn from_rpc(est: &Value) -> Result<Self, NodeError> {
        let num = |k: &str| -> Result<u128, NodeError> {
            let s = est[k]
                .as_str()
                .ok_or_else(|| NodeError::Decode(format!("{k}: expected hex string")))?;
            u128::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16)
                .map_err(|_| NodeError::Decode(format!("{k}: bad u128")))
        };
        let res = |a: &str, p: &str| -> Result<ResourceEstimate, NodeError> {
            Ok(ResourceEstimate {
                amount: num(a)?,
                price: num(p)?,
            })
        };
        Ok(FeeEstimate {
            l1_gas: res("l1_gas_consumed", "l1_gas_price")?,
            l2_gas: res("l2_gas_consumed", "l2_gas_price")?,
            l1_data_gas: res("l1_data_gas_consumed", "l1_data_gas_price")?,
        })
    }

    /// The estimated fee, `Σ amount × price` (saturating).
    pub fn fee(&self) -> u128 {
        self.resources()
            .iter()
            .map(|r| r.amount.saturating_mul(r.price))
            .fold(0u128, u128::saturating_add)
    }

    fn bounds_with(&self, amount_pct: [u128; 3], price_pct: [u128; 3]) -> FeeBounds {
        let r = self.resources();
        let b = |i: usize| ResourceBounds {
            max_amount: amount_u64(pct(r[i].amount, amount_pct[i])),
            max_price_per_unit: pct(r[i].price, price_pct[i]),
        };
        FeeBounds {
            l1_gas: b(0),
            l2_gas: b(1),
            l1_data_gas: b(2),
        }
    }

    /// The padded bounds Bramble Companion submits by default: amounts × 1.5, prices ×
    /// their per-resource headroom.
    pub fn bounds(&self) -> FeeBounds {
        self.bounds_with([AMOUNT_MARGIN_PCT; 3], PRICE_HEADROOM_PCT)
    }

    /// The tightest bounds Bramble Companion will submit.
    pub fn min_bounds(&self) -> FeeBounds {
        self.bounds_with([MIN_AMOUNT_MARGIN_PCT; 3], [MIN_PRICE_HEADROOM_PCT; 3])
    }

    /// Recover the estimate from bounds [`FeeEstimate::bounds`] produced (exact
    /// for those; for other bounds, the estimate they would correspond to).
    pub fn from_bounds(padded: &FeeBounds) -> Self {
        let r = |b: &ResourceBounds, i: usize| ResourceEstimate {
            amount: unpct(u128::from(b.max_amount), AMOUNT_MARGIN_PCT),
            price: unpct(b.max_price_per_unit, PRICE_HEADROOM_PCT[i]),
        };
        FeeEstimate {
            l1_gas: r(&padded.l1_gas, 0),
            l2_gas: r(&padded.l2_gas, 1),
            l1_data_gas: r(&padded.l1_data_gas, 2),
        }
    }
}

/// How the padded bounds fit a balance.
#[derive(Debug, Clone, Copy)]
pub enum Fit {
    /// The padded bounds fit as they are.
    Padded(FeeBounds),
    /// Tightened toward the estimate (price headroom first, then the amount
    /// margin) so that the max fee is within the balance.
    Tightened(FeeBounds),
    /// Even the tightest bounds exceed the balance (all in fri).
    Insufficient {
        estimated: u128,
        padded: u128,
        minimum: u128,
    },
}

const PPM: u128 = 1_000_000;

/// Fit the padded bounds of `est` to `budget` (fri the payer can spend on the
/// fee). Prices give way first: a price bound that turns out too low only fails
/// validation, while a too-low amount reverts the transaction and is charged.
pub fn fit_to_balance(est: &FeeEstimate, budget: u128) -> Fit {
    let padded = est.bounds();
    let padded_fee = max_fee(&padded);
    if padded_fee <= budget {
        return Fit::Padded(padded);
    }
    let floor = est.min_bounds();
    let pick = |b: &FeeBounds, i: usize| match i {
        0 => b.l1_gas,
        1 => b.l2_gas,
        _ => b.l1_data_gas,
    };
    // Interpolate one field from `floor` toward `padded` by `s_ppm` / 10^6.
    let lerp = |lo: u128, hi: u128, s_ppm: u128| {
        lo + (hi.saturating_sub(lo) / PPM).saturating_mul(s_ppm) + (hi.saturating_sub(lo) % PPM) * s_ppm / PPM
    };
    let tightened = |s_ppm: u128, shrink_amounts: bool| -> FeeBounds {
        let r = |i: usize| {
            let (p, f) = (pick(&padded, i), pick(&floor, i));
            if shrink_amounts {
                ResourceBounds {
                    max_amount: amount_u64(lerp(f.max_amount.into(), p.max_amount.into(), s_ppm)),
                    max_price_per_unit: f.max_price_per_unit,
                }
            } else {
                ResourceBounds {
                    max_amount: p.max_amount,
                    max_price_per_unit: lerp(f.max_price_per_unit, p.max_price_per_unit, s_ppm),
                }
            }
        };
        FeeBounds {
            l1_gas: r(0),
            l2_gas: r(1),
            l1_data_gas: r(2),
        }
    };
    // Stage 1: padded amounts, prices between floor and padded.
    // Stage 2: floor prices, amounts between floor and padded.
    for shrink_amounts in [false, true] {
        let lo = max_fee(&tightened(0, shrink_amounts));
        if lo > budget {
            continue;
        }
        let hi = max_fee(&tightened(PPM, shrink_amounts));
        let span = hi.saturating_sub(lo).max(1);
        // Fee is linear in s; rounding down keeps it within the budget.
        let s_ppm = ((budget - lo) / span)
            .saturating_mul(PPM)
            .saturating_add((budget - lo) % span * PPM / span)
            .min(PPM);
        let b = tightened(s_ppm, shrink_amounts);
        if max_fee(&b) <= budget {
            return Fit::Tightened(b);
        }
    }
    Fit::Insufficient {
        estimated: est.fee(),
        padded: padded_fee,
        minimum: max_fee(&floor),
    }
}

/// The field-wise minimum of two sets of bounds.
fn min_bounds(a: &FeeBounds, b: &FeeBounds) -> FeeBounds {
    let m = |x: ResourceBounds, y: ResourceBounds| ResourceBounds {
        max_amount: x.max_amount.min(y.max_amount),
        max_price_per_unit: x.max_price_per_unit.min(y.max_price_per_unit),
    };
    FeeBounds {
        l1_gas: m(a.l1_gas, b.l1_gas),
        l2_gas: m(a.l2_gas, b.l2_gas),
        l1_data_gas: m(a.l1_data_gas, b.l1_data_gas),
    }
}

/// fri as a decimal STRK amount (6 decimals, rounded down).
pub fn strk(fri: u128) -> String {
    const WEI: u128 = 1_000_000_000_000_000_000;
    format!("{}.{:06}", fri / WEI, (fri % WEI) / 1_000_000_000_000)
}

/// Fit auto-estimated `padded` bounds (from [`FeeEstimate::bounds`]) to the
/// STRK balance of `payer`, less `reserve` fri the transaction itself spends
/// (pool fee, STRK deposits). One `balanceOf` read. A failed read keeps the
/// padded bounds: the node's own check still applies and is named by
/// [`broadcast_error`].
pub async fn fit_bounds_to_balance(
    node: &dyn StarknetRpc,
    payer: &Felt,
    padded: FeeBounds,
    reserve: u128,
) -> Result<FeeBounds, WalletRpcError> {
    let token = Felt::from_hex(FEE_TOKEN).expect("constant felt");
    let Ok(balance) = node.balance_of(&token, payer).await else {
        return Ok(padded);
    };
    let budget = balance.saturating_sub(reserve);
    if max_fee(&padded) <= budget {
        return Ok(padded);
    }
    let est = FeeEstimate::from_bounds(&padded);
    match fit_to_balance(&est, budget) {
        Fit::Padded(_) => Ok(padded),
        // Never above the node's bounds, whatever rounding `from_bounds` did.
        Fit::Tightened(b) => Ok(min_bounds(&b, &padded)),
        Fit::Insufficient {
            estimated,
            padded: padded_fee,
            minimum,
        } => {
            let spent = if reserve > 0 {
                format!(
                    ", of which this transaction itself spends {} STRK (pool fee / deposits)",
                    strk(reserve)
                )
            } else {
                String::new()
            };
            Err(WalletRpcError::Precondition(format!(
                "the paying account's STRK balance can't cover this transaction's fee bounds: estimated fee {} STRK, \
                 padded max fee {} STRK, tightest bounds Bramble Companion will submit {} STRK, balance {} STRK{spent}. Fund the \
                 account with at least {} STRK more, or pass explicit resource_bounds.",
                strk(estimated),
                strk(padded_fee),
                strk(minimum),
                strk(balance),
                strk(minimum.saturating_add(reserve).saturating_sub(balance)),
            ))
            .with_reason(INSUFFICIENT_BALANCE_FOR_BOUNDS))
        }
    }
}

/// Whether a node error is the "resource bounds exceed balance" refusal
/// (`{"code":55,"data":"Resources bounds (…) exceed balance (…)."}`).
pub fn is_exceed_balance(text: &str) -> bool {
    text.to_ascii_lowercase().contains("exceed balance")
}

/// The balance in a node's "exceed balance (N)" refusal, if it can be read.
fn refused_balance(text: &str) -> Option<u128> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("exceed balance (")? + "exceed balance (".len();
    let digits: String = lower[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A broadcast failure: the node's "bounds exceed balance" refusal becomes a
/// precondition with [`INSUFFICIENT_BALANCE_FOR_BOUNDS`]; anything else stays
/// a node error (-32004).
pub fn broadcast_error(e: impl std::fmt::Display, bounds: &FeeBounds) -> WalletRpcError {
    let text = e.to_string();
    if !is_exceed_balance(&text) {
        return WalletRpcError::Node(text);
    }
    let balance = refused_balance(&text)
        .map(|b| format!(", balance {} STRK", strk(b)))
        .unwrap_or_default();
    WalletRpcError::Precondition(format!(
        "the node refused the transaction because its max fee exceeds the paying account's STRK balance (max fee {} \
         STRK{balance}). Fund the account, or pass lower resource_bounds.",
        strk(max_fee(bounds)),
    ))
    .with_reason(INSUFFICIENT_BALANCE_FOR_BOUNDS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The STRK20 `apply_actions` from #61: ~90.5M L2 gas at ~25.3 gwei-fri.
    fn issue_estimate() -> FeeEstimate {
        FeeEstimate {
            l1_gas: ResourceEstimate {
                amount: 0,
                price: 52_736_062_563_358,
            },
            l2_gas: ResourceEstimate {
                amount: 90_491_541,
                price: 25_341_972_642,
            },
            l1_data_gas: ResourceEstimate {
                amount: 1_248,
                price: 5_586_658_467_335,
            },
        }
    }

    #[test]
    fn amount_and_price_are_padded_separately() {
        let est = issue_estimate();
        let b = est.bounds();
        assert_eq!(b.l2_gas.max_amount, 135_737_311);
        assert_eq!(
            b.l2_gas.max_price_per_unit,
            25_341_972_642 * L2_GAS_PRICE_HEADROOM_PCT / 100
        );
        assert_eq!(b.l1_data_gas.max_amount, 1_872);
        assert_eq!(
            b.l1_data_gas.max_price_per_unit,
            5_586_658_467_335 * L1_DATA_GAS_PRICE_HEADROOM_PCT / 100
        );
        assert_eq!(
            b.l1_gas.max_price_per_unit,
            52_736_062_563_358 * L1_GAS_PRICE_HEADROOM_PCT / 100
        );
        // Was ~2.25× the estimate (5.16 STRK for a 2.29 STRK estimate); now the
        // amount margin times the L2 price headroom, as L2 gas dominates.
        let ratio = max_fee(&b) as f64 / est.fee() as f64;
        let want = (AMOUNT_MARGIN_PCT * L2_GAS_PRICE_HEADROOM_PCT) as f64 / 10_000.0;
        assert!((ratio - want).abs() < 0.01, "ratio {ratio}");
        assert!(ratio < 2.0);
    }

    #[test]
    fn parses_a_fee_estimate() {
        let v = json!({
            "l1_gas_consumed": "0x0", "l1_gas_price": "0x10",
            "l2_gas_consumed": "0x64", "l2_gas_price": "0x3",
            "l1_data_gas_consumed": "0x80", "l1_data_gas_price": "0x2",
            "overall_fee": "0x22c", "unit": "FRI",
        });
        let est = FeeEstimate::from_rpc(&v).unwrap();
        assert_eq!(est.l2_gas, ResourceEstimate { amount: 100, price: 3 });
        assert_eq!(est.fee(), 300 + 256);
        assert!(FeeEstimate::from_rpc(&json!({ "l1_gas_consumed": 5 })).is_err());
    }

    #[test]
    fn amounts_saturate_instead_of_truncating() {
        let est = FeeEstimate {
            l2_gas: ResourceEstimate {
                amount: u128::from(u64::MAX),
                price: u128::MAX,
            },
            l1_gas: ResourceEstimate {
                amount: u128::MAX,
                price: 1,
            },
            ..FeeEstimate::default()
        };
        let b = est.bounds();
        // 1.5 × u64::MAX would wrap to a small number with `as u64`.
        assert_eq!(b.l2_gas.max_amount, u64::MAX);
        assert_eq!(b.l1_gas.max_amount, u64::MAX);
        assert_eq!(b.l2_gas.max_price_per_unit, u128::MAX);
        assert_eq!(max_fee(&b), u128::MAX);
        assert_eq!(est.fee(), u128::MAX);
        assert_eq!(pct(u128::MAX, 150), u128::MAX);
        assert_eq!(pct(199, 150), 298);
    }

    #[test]
    fn padding_round_trips() {
        for v in [
            0u128,
            1,
            2,
            3,
            7,
            99,
            100,
            101,
            12_345,
            90_491_541,
            25_341_972_642,
            5_586_658_467_335,
        ] {
            let est = FeeEstimate {
                l1_gas: ResourceEstimate {
                    amount: v,
                    price: v + 1,
                },
                l2_gas: ResourceEstimate {
                    amount: v,
                    price: v * 3,
                },
                l1_data_gas: ResourceEstimate {
                    amount: v,
                    price: v + 17,
                },
            };
            assert_eq!(FeeEstimate::from_bounds(&est.bounds()), est);
        }
    }

    #[test]
    fn padded_bounds_are_kept_when_the_balance_covers_them() {
        let est = issue_estimate();
        let Fit::Padded(b) = fit_to_balance(&est, max_fee(&est.bounds())) else {
            panic!("expected padded")
        };
        assert_eq!(max_fee(&b), max_fee(&est.bounds()));
    }

    #[test]
    fn prices_give_way_before_amounts() {
        let est = issue_estimate();
        let padded = est.bounds();
        // Between the padded fee and the fee with floor prices.
        let budget = max_fee(&padded) - est.fee() / 20;
        let Fit::Tightened(b) = fit_to_balance(&est, budget) else {
            panic!("expected tightened")
        };
        assert!(max_fee(&b) <= budget);
        assert!(max_fee(&b) > budget - budget / 1000, "uses the budget");
        assert_eq!(b.l2_gas.max_amount, padded.l2_gas.max_amount);
        assert!(b.l2_gas.max_price_per_unit < padded.l2_gas.max_price_per_unit);
        assert!(b.l2_gas.max_price_per_unit >= est.min_bounds().l2_gas.max_price_per_unit);
    }

    #[test]
    fn amounts_shrink_once_prices_are_at_their_floor() {
        let est = issue_estimate();
        let floor = est.min_bounds();
        let budget = max_fee(&floor) + est.fee() / 20;
        let Fit::Tightened(b) = fit_to_balance(&est, budget) else {
            panic!("expected tightened")
        };
        assert!(max_fee(&b) <= budget && max_fee(&b) >= max_fee(&floor));
        assert_eq!(b.l2_gas.max_price_per_unit, floor.l2_gas.max_price_per_unit);
        assert!(b.l2_gas.max_amount > floor.l2_gas.max_amount);
        assert!(b.l2_gas.max_amount < est.bounds().l2_gas.max_amount);
    }

    #[test]
    fn a_balance_below_the_tightest_bounds_is_insufficient() {
        let est = issue_estimate();
        let Fit::Insufficient {
            estimated,
            padded,
            minimum,
        } = fit_to_balance(&est, est.fee())
        else {
            panic!("expected insufficient")
        };
        assert_eq!(estimated, est.fee());
        assert_eq!(padded, max_fee(&est.bounds()));
        assert_eq!(minimum, max_fee(&est.min_bounds()));
        assert!(minimum > estimated);
        // Exactly the tightest bounds is enough.
        assert!(matches!(fit_to_balance(&est, minimum), Fit::Tightened(_)));
        assert!(matches!(fit_to_balance(&FeeEstimate::default(), 0), Fit::Padded(_)));
    }

    #[test]
    fn the_nodes_exceed_balance_refusal_gets_a_reason() {
        let node = r#"rpc: {"code":55,"data":"Resources bounds ({ l1_gas: { max_amount: 0, max_price_per_unit: 79104093845037 }, l2_gas: { max_amount: 135737311, max_price_per_unit: 38012958963 }, l1_data_gas: { max_amount: 1872, max_price_per_unit: 8379987701002 } }) exceed balance (1922662352869740595).","message":"Account validation failed"}"#;
        let bounds = issue_estimate().bounds();
        let e = broadcast_error(node, &bounds);
        assert_eq!(e.reason(), Some(INSUFFICIENT_BALANCE_FOR_BOUNDS));
        assert_eq!(e.code(), -32006);
        let m = e.message();
        assert!(m.contains("balance 1.922662 STRK"));
        assert!(m.contains(&format!("max fee {} STRK", strk(max_fee(&bounds)))));
        // Other node errors stay -32004 with no reason.
        let other = broadcast_error(r#"rpc: {"code":55,"message":"Account validation failed"}"#, &bounds);
        assert_eq!(other.code(), -32004);
        assert_eq!(other.reason(), None);
    }

    #[test]
    fn strk_formats_fri() {
        assert_eq!(strk(0), "0.000000");
        assert_eq!(strk(1_922_662_352_869_740_595), "1.922662");
        assert_eq!(strk(2_500_000_000_000_000_000), "2.500000");
    }
}
