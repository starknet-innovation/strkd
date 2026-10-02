//! Encrypted vault: passphrase-protected storage for the seed + account
//! registry.
//!
//! Format (see `spec/wallet-companion-spec.md` §5.1):
//!   passphrase --Argon2id--> 32-byte key --AES-256-GCM--> ciphertext
//!
//! The vault is content-agnostic: it seals/opens arbitrary plaintext bytes.
//! Higher layers serialize the seed + registry into those bytes. Plaintext is
//! always returned in a `Zeroizing` buffer so it is wiped on drop.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{CoreError, Result};

// v2: the account-derivation change (strkd#16) moved every address — the OZ
// salt became zero and user accounts moved onto the BIP-44 account axis. A v1
// vault's cached addresses no longer match what its seed derives, so it must be
// refused rather than silently opened against the wrong accounts. There is no
// migration by decision: assets were swept out beforehand (strkd#14).
const VAULT_VERSION: u8 = 2;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

// Argon2id parameters. Tune for the target hardware before production; these
// are reasonable interactive defaults (~64 MiB, 3 passes).
const ARGON2_M_COST: u32 = 64 * 1024; // KiB
const ARGON2_T_COST: u32 = 3;
const ARGON2_P_COST: u32 = 1;

/// KDF parameters persisted with the vault so it can be reopened even if the
/// defaults change later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    #[serde(with = "hex::serde")]
    pub salt: Vec<u8>,
}

/// A sealed vault, safe to persist to disk. Contains no plaintext secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedVault {
    pub version: u8,
    pub kdf: KdfParams,
    #[serde(with = "hex::serde")]
    pub nonce: Vec<u8>,
    #[serde(with = "hex::serde")]
    pub ciphertext: Vec<u8>,
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).map_err(|_| CoreError::Crypto)?;
    Ok(buf)
}

fn derive_key(passphrase: &str, params: &KdfParams) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let argon_params = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN))
        .map_err(|_| CoreError::Kdf)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(passphrase.as_bytes(), &params.salt, key.as_mut())
        .map_err(|_| CoreError::Kdf)?;
    Ok(key)
}

impl EncryptedVault {
    /// Encrypt `plaintext` under `passphrase`, producing a persistable vault.
    pub fn seal(passphrase: &str, plaintext: &[u8]) -> Result<Self> {
        let salt = random_bytes::<SALT_LEN>()?;
        let kdf = KdfParams {
            m_cost: ARGON2_M_COST,
            t_cost: ARGON2_T_COST,
            p_cost: ARGON2_P_COST,
            salt: salt.to_vec(),
        };
        let key = derive_key(passphrase, &kdf)?;

        let nonce_bytes = random_bytes::<NONCE_LEN>()?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
            .map_err(|_| CoreError::Crypto)?;

        Ok(EncryptedVault {
            version: VAULT_VERSION,
            kdf,
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        })
    }

    /// Decrypt the vault with `passphrase`. Returns the plaintext in a wiped-on-
    /// drop buffer. A wrong passphrase or any tampering fails the AEAD tag and
    /// yields [`CoreError::BadPassphraseOrCorrupt`].
    pub fn open(&self, passphrase: &str) -> Result<Zeroizing<Vec<u8>>> {
        if self.version != VAULT_VERSION {
            return Err(CoreError::UnsupportedVaultVersion(self.version));
        }
        if self.nonce.len() != NONCE_LEN {
            return Err(CoreError::BadPassphraseOrCorrupt);
        }
        let key = derive_key(passphrase, &self.kdf)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&self.nonce), self.ciphertext.as_ref())
            .map_err(|_| CoreError::BadPassphraseOrCorrupt)?;
        Ok(Zeroizing::new(plaintext))
    }

    /// Whether this build can open a vault of this version.
    ///
    /// Lets a caller distinguish "your passphrase is wrong" from "this build
    /// refuses this vault" *before* asking for a passphrase. The difference
    /// matters: one is a typo, the other means the on-disk vault is fine and the
    /// way forward is the recovery phrase. Telling a user their vault may be
    /// corrupt when it is not invites them to delete it.
    pub fn is_supported_version(&self) -> bool {
        self.version == VAULT_VERSION
    }

    /// The vault format this build writes and can open.
    pub const fn supported_version() -> u8 {
        VAULT_VERSION
    }

    /// Serialize the sealed vault to JSON for on-disk storage.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(|_| CoreError::Serialization)
    }

    /// Parse a sealed vault from JSON.
    pub fn from_json(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|_| CoreError::Serialization)
    }
}
