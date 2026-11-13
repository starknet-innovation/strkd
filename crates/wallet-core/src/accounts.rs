//! Account registry: the set of derived accounts, partitioned by domain and
//! tagged with the caller that owns agent accounts (for scoping, spec §6.3).

use serde::{Deserialize, Serialize};

use krusty_kms_common::ChainId;

use crate::account_contract::AccountContract;
use crate::domain::Domain;

/// A single derived account's metadata. The address is cached so the registry
/// can be displayed/served without re-deriving (and without the seed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountRef {
    pub domain: Domain,
    /// BIP-44 address index within the domain branch.
    pub index: u32,
    /// Cached counterfactual address, hex (`0x…`).
    pub address: String,
    /// User-facing label.
    pub label: String,
    /// Which account contract this address is. Defaults to OpenZeppelin so
    /// registries written before the seam existed load as what they are.
    #[serde(default)]
    pub contract: AccountContract,
    /// For agent accounts: the client id that created/owns it. `None` for user
    /// accounts. Used to scope `wallet_requestAccounts` per caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_client_id: Option<String>,
}

/// The strkd account whose key an owner's STRK20 viewing key is derived from.
/// Recorded so the key can be derived again (seed + owner + this).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnerController {
    pub domain: Domain,
    pub index: u32,
    /// The controller account's address, hex (`0x…`).
    pub address: String,
}

/// An external contract enrolled as a STRK20 owner
/// (`docs/project/design-48-strk20-external-owner.md` §3.1). Holds no secrets:
/// the owner's viewing key is re-derived from the controller per call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Strk20Owner {
    /// The owner contract's address, hex (`0x…`, as [`crate::address_hex`]).
    pub owner: String,
    pub chain: ChainId,
    /// The pool the key is scoped to, hex.
    pub pool: String,
    pub controller: OwnerController,
    /// The agent client that enrolled it; `None` for the user's own (enrolled
    /// by an App client or the desktop). Scoped like [`AccountRef::owner_client_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub label: String,
}

impl Strk20Owner {
    /// Whether this record is the one for `owner` on `(chain, pool)` within the
    /// scope `client_id` (see [`Registry::scoped_for`]).
    pub fn is(&self, owner: &str, chain: ChainId, pool: &str, client_id: Option<&str>) -> bool {
        self.owner == owner && self.chain == chain && self.pool == pool && self.client_id.as_deref() == client_id
    }
}

/// The full account registry persisted inside the vault.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registry {
    pub accounts: Vec<AccountRef>,
    /// Enrolled STRK20 owner contracts. Absent in vaults written before #48.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strk20_owners: Vec<Strk20Owner>,
}

impl Registry {
    /// Next free address index for a domain (max existing index + 1, or 0).
    pub fn next_index(&self, domain: Domain) -> u32 {
        self.accounts
            .iter()
            .filter(|a| a.domain == domain)
            .map(|a| a.index)
            .max()
            .map_or(0, |m| m + 1)
    }

    /// Append an account. Callers are responsible for using `next_index`.
    pub fn add(&mut self, account: AccountRef) {
        self.accounts.push(account);
    }

    /// Remove the account(s) with the given address. Returns true if any were
    /// removed. Used to roll back an in-memory add when persistence fails.
    pub fn remove_by_address(&mut self, address: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.address != address);
        self.accounts.len() != before
    }

    /// All user-domain accounts.
    pub fn user_accounts(&self) -> impl Iterator<Item = &AccountRef> {
        self.accounts.iter().filter(|a| a.domain == Domain::User)
    }

    /// Accounts visible to a given caller (spec §6.3 scoping):
    /// * an `agent` client sees only the agent accounts it owns;
    /// * a user/app client (`client_id == None`) sees user accounts.
    pub fn scoped_for<'a>(
        &'a self,
        client_id: Option<&'a str>,
    ) -> Box<dyn Iterator<Item = &'a AccountRef> + 'a> {
        match client_id {
            None => Box::new(self.user_accounts()),
            Some(cid) => Box::new(
                self.accounts
                    .iter()
                    .filter(move |a| a.owner_client_id.as_deref() == Some(cid)),
            ),
        }
    }

    /// STRK20 owners visible to a caller, by the same rule as [`Self::scoped_for`]:
    /// an agent sees the owners it enrolled, a user/app caller the user's.
    pub fn strk20_owners_for<'a>(&'a self, client_id: Option<&'a str>) -> impl Iterator<Item = &'a Strk20Owner> + 'a {
        self.strk20_owners.iter().filter(move |o| o.client_id.as_deref() == client_id)
    }

    /// Add an owner record. Returns false (and adds nothing) if the same owner
    /// is already enrolled on that chain and pool in the same scope.
    pub fn add_strk20_owner(&mut self, owner: Strk20Owner) -> bool {
        let dup = self
            .strk20_owners
            .iter()
            .any(|o| o.is(&owner.owner, owner.chain, &owner.pool, owner.client_id.as_deref()));
        if !dup {
            self.strk20_owners.push(owner);
        }
        !dup
    }

    /// Remove and return the owner record matching [`Strk20Owner::is`].
    pub fn remove_strk20_owner(
        &mut self,
        owner: &str,
        chain: ChainId,
        pool: &str,
        client_id: Option<&str>,
    ) -> Option<Strk20Owner> {
        let i = self.strk20_owners.iter().position(|o| o.is(owner, chain, pool, client_id))?;
        Some(self.strk20_owners.remove(i))
    }
}
