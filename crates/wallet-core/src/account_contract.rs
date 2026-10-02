//! The account-contract seam: everything that differs between account classes.
//!
//! A Starknet account is a contract, and wallets that support more than one
//! class have to vary four things together — the class hash, the constructor
//! calldata, the deployment salt, and **how a signature is serialized for
//! `__validate__`**. Getting three right and the fourth wrong produces an
//! account whose address is correct and whose every transaction is rejected.
//!
//! Today [`AccountContract`] has exactly one variant. The seam exists anyway
//! because the signature-serialization split is the expensive one to retrofit:
//! it threads through signing, broadcasting, and the sign-only responses, and
//! every caller that reaches for `(r, s)` directly is a site that would need
//! revisiting. See
//! [issue #15](https://github.com/starknet-innovation/strkd/issues/15).
//!
//! ## Adding a class
//!
//! Add a variant, implement the four methods, and add its vectors to the
//! conformance suite. For Argent 0.4 specifically:
//!
//! - constructor calldata is `[0, public_key, 1]` — signer enum variant `0`
//!   (`Signer::Starknet`) then `1`, which is Cairo's `Option::None` for the
//!   guardian;
//! - the signature is `[1, 0, public_key, r, s]` — a one-element
//!   `SignerSignature` array, not a bare `(r, s)`;
//! - the salt is the public key.
//!
//! Do **not** take these from `krusty-kms`. Its `ArgentAccount` built
//! `[0, public_key, 0]` — an `Option::Some` prefix with no payload — and was
//! removed upstream in v0.11.0. Bramble computes them in TypeScript pinned
//! against the vendored official Argent release; mirror that instead.

use krusty_kms::account_class::{AccountClass, OpenZeppelinAccount, SaltPolicy};
use krusty_kms::StarkSignature;
use krusty_kms_common::ChainId;
use serde::{Deserialize, Serialize};
use starknet_types_core::felt::Felt;

use crate::error::{CoreError, Result};

/// Counterfactual deployment parameters for an account, as needed by
/// `wallet_deploymentData` and by DEPLOY_ACCOUNT.
#[derive(Debug, Clone)]
pub struct DeploymentData {
    pub address: Felt,
    pub class_hash: Felt,
    pub salt: Felt,
    pub constructor_calldata: Vec<Felt>,
}

/// Which contract family an account is.
///
/// Serialized into the vault's registry, so the wire name is part of the vault
/// format. `#[serde(default)]` on the field that holds it means vaults written
/// before this existed load as [`AccountContract::OpenZeppelin`], which is what
/// they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountContract {
    /// OpenZeppelin `AccountUpgradeable`. Constructor `(public_key)`,
    /// signature `(r, s)`.
    #[default]
    OpenZeppelin,
}

impl AccountContract {
    /// How this contract's deployment salt is chosen.
    ///
    /// For a class whose constructor commits to the public key — as OZ's does —
    /// the salt does not affect which key controls the address, so this is a
    /// compatibility choice rather than a security one. It must match whatever
    /// other wallets use for the same class, or the same seed yields different
    /// addresses in each.
    ///
    /// OpenZeppelin uses **zero**, matching bramble, which passes `"0x0"`
    /// explicitly rather than relying on any library default. Bramble has
    /// deployed mainnet accounts at those addresses and cannot cheaply move;
    /// strkd was alpha, so strkd moved (strkd#16).
    ///
    /// This is deliberately **not** krusty's default. krusty defaults both of
    /// its OZ address entry points to the public key (since `4639de5`,
    /// 2026-08-10, inside a broad security-hardening PR), so choosing zero is
    /// an explicit override and the caveat below is load-bearing rather than
    /// incidental.
    ///
    /// Zero is safe **here** because OZ's constructor takes `[public_key]`, so
    /// the key is bound into the address regardless of the salt, and
    /// `DEPLOY_ACCOUNT` (deployer `0`) needs the account's own signature — an
    /// undeployed address cannot be squatted. A class whose constructor does
    /// **not** commit to its owner must never use a zero salt: there, the first
    /// deployer wins. Do not copy this choice to a new variant by default.
    pub fn salt_policy(self) -> SaltPolicy {
        match self {
            AccountContract::OpenZeppelin => SaltPolicy::Zero,
        }
    }

    /// The declared class hash for this contract on `chain`.
    pub fn class_hash(self, chain: ChainId) -> Result<Felt> {
        match self {
            AccountContract::OpenZeppelin => Ok(OpenZeppelinAccount::latest(chain)
                .map_err(|_| CoreError::Address)?
                .class_hash()),
        }
    }

    /// Constructor calldata committing to `public_key`.
    pub fn constructor_calldata(self, public_key: &Felt) -> Result<Vec<Felt>> {
        match self {
            AccountContract::OpenZeppelin => Ok(OpenZeppelinAccount::latest(ChainId::Mainnet)
                .map_err(|_| CoreError::Address)?
                .build_constructor_calldata(public_key)),
        }
    }

    /// Full counterfactual deployment data for `public_key` on `chain`.
    pub fn deployment(self, public_key: &Felt, chain: ChainId) -> Result<DeploymentData> {
        match self {
            AccountContract::OpenZeppelin => {
                let oz = OpenZeppelinAccount::latest(chain).map_err(|_| CoreError::Address)?;
                let d = oz
                    .deployment_descriptor(public_key, self.salt_policy())
                    .map_err(|_| CoreError::Address)?;
                Ok(DeploymentData {
                    address: d.address,
                    class_hash: d.class_hash,
                    salt: d.salt,
                    constructor_calldata: d.constructor_calldata,
                })
            }
        }
    }

    /// Serialize a raw Stark signature the way this account's `__validate__`
    /// (and `is_valid_signature`) expects it.
    ///
    /// This is the method that makes the seam worth having. Every broadcast and
    /// every sign-only response must go through it — reaching for `(r, s)`
    /// directly is correct only for OpenZeppelin and silently wrong elsewhere.
    ///
    /// It takes the whole [`StarkSignature`] rather than the pair because
    /// `public_key` is part of the encoding for some classes (Argent's
    /// `SignerSignature` carries it).
    pub fn serialize_signature(self, sig: &StarkSignature) -> Vec<Felt> {
        match self {
            AccountContract::OpenZeppelin => vec![sig.r, sig.s],
        }
    }

    /// Stable wire name, for logs and RPC responses.
    pub fn as_str(self) -> &'static str {
        match self {
            AccountContract::OpenZeppelin => "openzeppelin",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openzeppelin_is_the_default_so_existing_vaults_load_unchanged() {
        assert_eq!(AccountContract::default(), AccountContract::OpenZeppelin);
        assert_eq!(
            serde_json::from_str::<AccountContract>("\"openzeppelin\"").unwrap(),
            AccountContract::OpenZeppelin
        );
    }

    #[test]
    fn openzeppelin_signs_as_a_bare_r_s_pair() {
        let sig = StarkSignature {
            public_key: Felt::from(0x123u32),
            r: Felt::from(7u32),
            s: Felt::from(9u32),
        };
        assert_eq!(
            AccountContract::OpenZeppelin.serialize_signature(&sig),
            vec![Felt::from(7u32), Felt::from(9u32)]
        );
    }

    #[test]
    fn openzeppelin_constructor_commits_to_the_public_key() {
        let pk = Felt::from(0x123u32);
        assert_eq!(
            AccountContract::OpenZeppelin.constructor_calldata(&pk).unwrap(),
            vec![pk],
            "the key must be in the calldata — it is what binds the address to the owner"
        );
    }
}
