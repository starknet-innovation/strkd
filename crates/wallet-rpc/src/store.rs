//! On-disk vault file lifecycle (spec §5.1, §10).
//!
//! `VaultStore` owns the path to `vault.bin` and reads/writes the
//! passphrase-encrypted [`EncryptedVault`]. Writes are **atomic** (temp file +
//! rename) so a crash mid-write can't corrupt the vault, and the file is
//! restricted to the owner (`0600` on unix).
//!
//! The store only ever sees ciphertext — encryption/decryption happens in
//! `wallet-core`. It holds no key material.

use std::io;
use std::path::{Path, PathBuf};

use wallet_core::EncryptedVault;

/// Handle to an on-disk encrypted vault file. Cheap to clone.
#[derive(Debug, Clone)]
pub struct VaultStore {
    path: PathBuf,
}

impl VaultStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        VaultStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Load and parse the vault, or `Ok(None)` if the file does not exist yet.
    pub fn load(&self) -> io::Result<Option<EncryptedVault>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let s = String::from_utf8(bytes)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "vault not utf-8"))?;
                let vault = EncryptedVault::from_json(&s)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "vault parse failed"))?;
                Ok(Some(vault))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Atomically write the vault: serialize → temp file (`0600`) → rename.
    pub fn save(&self, vault: &EncryptedVault) -> io::Result<()> {
        let json = vault
            .to_json()
            .map_err(|_| io::Error::other("vault serialize failed"))?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, json.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
