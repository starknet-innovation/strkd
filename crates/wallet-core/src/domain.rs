//! Derivation domains.
//!
//! A single BIP-39 seed feeds two non-overlapping branches (see
//! `spec/wallet-companion-spec.md` §6.1):
//!
//! | Domain | Account *n* is         | Rationale                                    |
//! |--------|------------------------|----------------------------------------------|
//! | User   | `m/44'/9004'/n'/0/0`   | Matches bramble's recovery scan → portable.  |
//! | Agent  | `m/44'/9004'/AGNT'/0/n`| Reserved account index → cannot collide.     |
//!
//! ## Why the two branches use different axes
//!
//! BIP-44 gives two indices below the coin type: a hardened *account* index and
//! an *address* index. Wallets differ in which one they walk for "add another
//! account", and the choice is a compatibility decision, not a free one.
//!
//! **User accounts walk the account index**, because that is what bramble's
//! recovery scan enumerates (`accountIndex` 0–19, address index fixed at 0). A
//! seed imported into either wallet therefore surfaces the same accounts. Before
//! [#16](https://github.com/starknet-innovation/strkd/issues/16) strkd walked
//! the address index instead, so only account 0 agreed.
//!
//! **Agent accounts sit under one reserved account index and walk the address
//! index**, which keeps every agent account off the axis user accounts occupy.
//! One reserved constant covers the whole branch.
//!
//! ## The reserved index
//!
//! [`AGENT_ACCOUNT_INDEX`] is `0x41474E54` — `"AGNT"` in ASCII, 1,095,192,148,
//! comfortably inside BIP-32's hardened ceiling of `0x7FFFFFFF`. It was `0x41`
//! (`"A"`, 65) when user accounts walked the address index and nothing traversed
//! the account axis. Once user accounts march `0, 1, 2, …` up that axis, 65 is
//! reachable by ordinary use — the 66th account — so the constant moved somewhere
//! no realistic enumeration reaches.
//!
//! The reservation is a convention, not something the chain enforces: another
//! wallet given a manual index could still derive here. Bramble is being asked
//! to exclude it explicitly (`mc-wallet#336`).

use serde::{Deserialize, Serialize};

/// Starknet SLIP-44 coin type (9004), re-exported from krusty for a single
/// source of truth.
pub const STARKNET_COIN_TYPE: u32 = krusty_kms::STARKNET_COIN_TYPE;

/// Hardened BIP-44 account index reserved for the agent branch — `"AGNT"`.
///
/// Must never be used for a user account, and no wallet sharing this seed
/// should derive here. See the module docs.
pub const AGENT_ACCOUNT_INDEX: u32 = 0x4147_4E54;

/// The account index of the first user account. User account *n* is at account
/// index *n*, so this is also the base of the user branch.
pub const USER_ACCOUNT_INDEX: u32 = 0;

/// BIP-32's ceiling on a hardened index. Anything reserved must be below it.
pub const MAX_HARDENED_INDEX: u32 = 0x7FFF_FFFF;

/// Which derivation branch an account belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Domain {
    /// Human-created, portable accounts.
    User,
    /// Agent-created, segregated accounts.
    Agent,
}

impl Domain {
    /// The BIP-44 `(account_index, address_index)` for the *n*-th account in
    /// this branch.
    ///
    /// The two branches deliberately walk different axes — see the module docs.
    pub const fn path_indices(self, n: u32) -> (u32, u32) {
        match self {
            Domain::User => (n, 0),
            Domain::Agent => (AGENT_ACCOUNT_INDEX, n),
        }
    }

    /// The BIP-44 coin type for this domain (always Starknet).
    pub const fn coin_type(self) -> u32 {
        STARKNET_COIN_TYPE
    }

    /// Human-readable derivation path for the *n*-th account in this branch,
    /// e.g. `m/44'/9004'/3'/0/0` for user account 3.
    pub fn path(self, n: u32) -> String {
        let (account, address) = self.path_indices(n);
        format!("m/44'/{}'/{}'/0/{}", self.coin_type(), account, address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_agent_index_is_a_valid_hardened_index() {
        // Compile-time: a reserved index above the hardened ceiling would derive
        // the wrong path rather than fail, so this must never build.
        const _: () = assert!(AGENT_ACCOUNT_INDEX < MAX_HARDENED_INDEX);
        assert_eq!(AGENT_ACCOUNT_INDEX, u32::from_be_bytes(*b"AGNT"));
    }

    #[test]
    fn the_branches_never_share_a_derivation_path() {
        // The agent branch is unreachable by user enumeration: a user account
        // would have to be the 1,095,192,148th.
        for n in [0u32, 1, 65, 19, 1000] {
            assert_ne!(Domain::User.path_indices(n), Domain::Agent.path_indices(n));
            assert_ne!(Domain::User.path_indices(n).0, AGENT_ACCOUNT_INDEX);
        }
    }
}
