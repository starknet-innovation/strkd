//! Account registry: the set of derived accounts, partitioned by domain and
//! tagged with the caller that owns agent accounts (for scoping, spec §6.3).

use serde::{Deserialize, Serialize};

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

/// The full account registry persisted inside the vault.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registry {
    pub accounts: Vec<AccountRef>,
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
}
