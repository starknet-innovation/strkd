//! Derivation domains.
//!
//! A single BIP-39 seed feeds two non-overlapping branches (see
//! `spec/wallet-companion-spec.md` §6.1):
//!
//! | Domain | Path                     | Rationale                                  |
//! |--------|--------------------------|--------------------------------------------|
//! | User   | `m/44'/9004'/0'/0/i`     | Mirrors Argent's base path → portable.     |
//! | Agent  | `m/44'/9004'/0x41'/0/j`  | Reserved hardened account index → isolated.|
//!
//! Mainstream wallets only ever scan `account' = 0'`, so any account index
//! other than 0 is collision-free against them. We reserve `0x41` ("A") for
//! agent-created accounts and never reuse it for user accounts.

use serde::{Deserialize, Serialize};

/// Starknet SLIP-44 coin type (9004), re-exported from krusty for a single
/// source of truth.
pub const STARKNET_COIN_TYPE: u32 = krusty_kms::STARKNET_COIN_TYPE;

/// Hardened BIP-44 account index for the user (portable) branch.
pub const USER_ACCOUNT_INDEX: u32 = 0;

/// Hardened BIP-44 account index reserved for the agent (segregated) branch.
///
/// Reserved constant — must never be reused for user accounts.
pub const AGENT_ACCOUNT_INDEX: u32 = 0x41;

/// Which derivation branch an account belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Domain {
    /// Human-created, portable accounts (`account' = 0`).
    User,
    /// Agent-created, segregated accounts (`account' = 0x41`).
    Agent,
}

impl Domain {
    /// The hardened BIP-44 `account` index for this domain.
    pub const fn account_index(self) -> u32 {
        match self {
            Domain::User => USER_ACCOUNT_INDEX,
            Domain::Agent => AGENT_ACCOUNT_INDEX,
        }
    }

    /// The BIP-44 coin type for this domain (always Starknet).
    pub const fn coin_type(self) -> u32 {
        STARKNET_COIN_TYPE
    }

    /// Human-readable derivation path for an address index, e.g.
    /// `m/44'/9004'/0'/0/3`.
    pub fn path(self, index: u32) -> String {
        format!(
            "m/44'/{}'/{}'/0/{}",
            self.coin_type(),
            self.account_index(),
            index
        )
    }
}
