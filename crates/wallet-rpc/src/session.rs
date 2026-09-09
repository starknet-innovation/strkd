//! The wallet session: the unlocked seed + account registry, and the operations
//! that need them. Keeps the mnemonic inside this type so it does not spread
//! through the dispatch layer.
//!
//! The unlocked state holds the **vault passphrase** as well as the mnemonic so
//! the session can re-seal itself (e.g. after a registry change) without the
//! passphrase being supplied again. Both are kept in `Zeroizing` buffers and
//! wiped on lock.
//!
//! Note: the optional BIP-39 passphrase (distinct from the vault passphrase) is
//! assumed empty in this phase. If/when supported it must also be stored here
//! and threaded into every derivation call.

use serde::{Deserialize, Serialize};
use wallet_core::{
    address_hex, deployment_data, sign_declare_v3, sign_deploy_account_v3, sign_invoke_v3,
    sign_typed_data, AccountRef, Call, ChainId, CoreError, DeploymentData, Domain, EncryptedVault,
    Felt, InvokeV3Params, Registry, SignedDeclare, SignedDeployAccount, SignedInvoke, StarkSignature,
};
use zeroize::Zeroizing;

use crate::error::WalletRpcError;

/// The plaintext payload stored (encrypted) in the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultContents {
    pub mnemonic: String,
    #[serde(default)]
    pub registry: Registry,
}

struct Unlocked {
    mnemonic: Zeroizing<String>,
    vault_passphrase: Zeroizing<String>,
    registry: Registry,
}

/// A wallet session, locked or unlocked, bound to a network.
pub struct WalletSession {
    chain: ChainId,
    unlocked: Option<Unlocked>,
}

impl WalletSession {
    pub fn new_locked(chain: ChainId) -> Self {
        WalletSession {
            chain,
            unlocked: None,
        }
    }

    /// Construct an already-unlocked session (used by tests and by callers that
    /// already hold the decrypted contents). `vault_passphrase` is the
    /// passphrase the session will re-seal under.
    pub fn new_unlocked(
        chain: ChainId,
        mnemonic: impl Into<String>,
        vault_passphrase: impl Into<String>,
        registry: Registry,
    ) -> Self {
        WalletSession {
            chain,
            unlocked: Some(Unlocked {
                mnemonic: Zeroizing::new(mnemonic.into()),
                vault_passphrase: Zeroizing::new(vault_passphrase.into()),
                registry,
            }),
        }
    }

    /// Decrypt the vault and enter the unlocked state, retaining the passphrase
    /// for later re-sealing.
    pub fn unlock(&mut self, vault: &EncryptedVault, vault_passphrase: &str) -> Result<(), CoreError> {
        let plaintext = vault.open(vault_passphrase)?;
        let contents: VaultContents =
            serde_json::from_slice(&plaintext).map_err(|_| CoreError::Serialization)?;
        self.unlocked = Some(Unlocked {
            mnemonic: Zeroizing::new(contents.mnemonic),
            vault_passphrase: Zeroizing::new(vault_passphrase.to_string()),
            registry: contents.registry,
        });
        Ok(())
    }

    /// Drop the unlocked state, wiping the mnemonic.
    pub fn lock(&mut self) {
        self.unlocked = None;
    }

    pub fn is_locked(&self) -> bool {
        self.unlocked.is_none()
    }

    pub fn chain(&self) -> ChainId {
        self.chain
    }

    pub fn set_chain(&mut self, chain: ChainId) {
        self.chain = chain;
    }

    /// The current registry, if unlocked.
    pub fn registry(&self) -> Result<&Registry, WalletRpcError> {
        self.unlocked
            .as_ref()
            .map(|u| &u.registry)
            .ok_or(WalletRpcError::Locked)
    }

    fn require_unlocked(&self) -> Result<&Unlocked, WalletRpcError> {
        self.unlocked.as_ref().ok_or(WalletRpcError::Locked)
    }

    /// Re-seal the current contents into a vault under the **stored** vault
    /// passphrase (for persisting registry changes such as a new agent account).
    pub fn reseal(&self) -> Result<EncryptedVault, WalletRpcError> {
        let u = self.require_unlocked()?;
        let contents = VaultContents {
            mnemonic: u.mnemonic.to_string(),
            registry: u.registry.clone(),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&contents).map_err(|_| WalletRpcError::Unknown("serialize".into()))?,
        );
        EncryptedVault::seal(&u.vault_passphrase, &plaintext).map_err(WalletRpcError::from)
    }

    /// Remove an account from the in-memory registry (used to roll back an add
    /// when persistence fails).
    pub fn rollback_account(&mut self, address: &str) {
        if let Some(u) = self.unlocked.as_mut() {
            u.registry.remove_by_address(address);
        }
    }

    /// Compute counterfactual deployment data for an account.
    pub fn deployment_data_for(
        &self,
        account: &AccountRef,
    ) -> Result<DeploymentData, WalletRpcError> {
        let u = self.require_unlocked()?;
        deployment_data(&u.mnemonic, account.domain, account.index, None, self.chain)
            .map_err(WalletRpcError::from)
    }

    /// Derive the Tongo (STRK20 privacy pool) keypair for `account`. Needs the
    /// unlocked seed; the private half zeroizes on drop (krusty `SecretFelt`).
    pub fn tongo_keypair_for(
        &self,
        account: &AccountRef,
    ) -> Result<wallet_core::TongoKeyPair, WalletRpcError> {
        let u = self.require_unlocked()?;
        wallet_core::tongo_keypair(&u.mnemonic, account.domain, account.index, None)
            .map_err(WalletRpcError::from)
    }

    /// Sign SNIP-12 typed data with the given account's key.
    pub fn sign_typed_data_for(
        &self,
        account: &AccountRef,
        typed_data_json: &str,
    ) -> Result<StarkSignature, WalletRpcError> {
        let u = self.require_unlocked()?;
        let address = Felt::from_hex(&account.address)
            .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
        sign_typed_data(
            &u.mnemonic,
            account.domain,
            account.index,
            None,
            typed_data_json,
            &address,
        )
        .map_err(WalletRpcError::from)
    }

    /// Sign a V3 invoke (sign-only) with the given account's key. Returns the
    /// tx hash, encoded calldata, and signature. Does not broadcast.
    pub fn sign_invoke_for(
        &self,
        account: &AccountRef,
        calls: &[Call],
        chain: ChainId,
        params: &InvokeV3Params,
    ) -> Result<SignedInvoke, WalletRpcError> {
        let u = self.require_unlocked()?;
        let sender = Felt::from_hex(&account.address)
            .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
        sign_invoke_v3(
            &u.mnemonic,
            account.domain,
            account.index,
            None,
            &sender,
            calls,
            chain,
            params,
        )
        .map_err(WalletRpcError::from)
    }

    /// Sign a DECLARE v3 with the given account's key. `class_hash` is the Sierra
    /// class hash, `compiled_class_hash` the CASM hash. Sign-only.
    pub fn sign_declare_for(
        &self,
        account: &AccountRef,
        class_hash: &Felt,
        compiled_class_hash: &Felt,
        chain: ChainId,
        params: &InvokeV3Params,
    ) -> Result<SignedDeclare, WalletRpcError> {
        let u = self.require_unlocked()?;
        let sender = Felt::from_hex(&account.address)
            .map_err(|_| WalletRpcError::Unknown("bad stored address".into()))?;
        sign_declare_v3(
            &u.mnemonic,
            account.domain,
            account.index,
            None,
            &sender,
            class_hash,
            compiled_class_hash,
            chain,
            params,
        )
        .map_err(WalletRpcError::from)
    }

    /// Sign a DEPLOY_ACCOUNT v3 for the given account (nonce 0). Returns the
    /// hash + deployment fields + signature for broadcasting. Sign-only.
    pub fn sign_deploy_account_for(
        &self,
        account: &AccountRef,
        chain: ChainId,
        params: &InvokeV3Params,
    ) -> Result<SignedDeployAccount, WalletRpcError> {
        let u = self.require_unlocked()?;
        sign_deploy_account_v3(
            &u.mnemonic,
            account.domain,
            account.index,
            None,
            chain,
            params,
        )
        .map_err(WalletRpcError::from)
    }

    /// Derive and register the next agent-domain account owned by `client_id`.
    pub fn create_agent_account(
        &mut self,
        client_id: &str,
        label: impl Into<String>,
    ) -> Result<AccountRef, WalletRpcError> {
        let chain = self.chain;
        let u = self.unlocked.as_mut().ok_or(WalletRpcError::Locked)?;
        let index = u.registry.next_index(Domain::Agent);
        let addr = wallet_core::oz_address(&u.mnemonic, Domain::Agent, index, None, chain)
            .map_err(WalletRpcError::from)?;
        let account = AccountRef {
            domain: Domain::Agent,
            index,
            address: address_hex(&addr),
            label: label.into(),
            owner_client_id: Some(client_id.to_string()),
        };
        u.registry.add(account.clone());
        Ok(account)
    }

    /// The "agent manager" / funding-source account: a user-domain account
    /// (default the root, index 0) used as the sender when funding agents. It is
    /// derived on demand — it need not be in the registry. Never chosen by a
    /// caller; the wallet resolves it so agents can only pull funds *into* their
    /// own accounts, never spend from an arbitrary one.
    pub fn manager_account(&self, index: u32) -> Result<AccountRef, WalletRpcError> {
        let u = self.require_unlocked()?;
        let addr = wallet_core::oz_address(&u.mnemonic, Domain::User, index, None, self.chain)
            .map_err(WalletRpcError::from)?;
        Ok(AccountRef {
            domain: Domain::User,
            index,
            address: address_hex(&addr),
            label: "manager".into(),
            owner_client_id: None,
        })
    }

    /// Derive and register the next user-domain account (created in-app, not via
    /// RPC — see spec §6.3). Portable branch, no owning client.
    pub fn create_user_account(
        &mut self,
        label: impl Into<String>,
    ) -> Result<AccountRef, WalletRpcError> {
        let chain = self.chain;
        let u = self.unlocked.as_mut().ok_or(WalletRpcError::Locked)?;
        let index = u.registry.next_index(Domain::User);
        let addr = wallet_core::oz_address(&u.mnemonic, Domain::User, index, None, chain)
            .map_err(WalletRpcError::from)?;
        let account = AccountRef {
            domain: Domain::User,
            index,
            address: address_hex(&addr),
            label: label.into(),
            owner_client_id: None,
        };
        u.registry.add(account.clone());
        Ok(account)
    }
}
