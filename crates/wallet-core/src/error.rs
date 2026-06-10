use thiserror::Error;

/// Errors surfaced by the wallet core.
///
/// Note: error messages must never embed seed/private-key material. krusty
/// `KmsError` values are mapped to a generic string here deliberately.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid mnemonic")]
    InvalidMnemonic,

    #[error("key derivation failed")]
    Derivation,

    #[error("signing failed")]
    Signing,

    #[error("account address computation failed")]
    Address,

    #[error("vault is locked")]
    Locked,

    #[error("incorrect passphrase or corrupt vault")]
    BadPassphraseOrCorrupt,

    #[error("vault serialization error")]
    Serialization,

    #[error("key derivation function failure")]
    Kdf,

    #[error("unsupported vault version: {0}")]
    UnsupportedVaultVersion(u8),

    #[error("crypto subsystem error")]
    Crypto,
}

pub type Result<T> = std::result::Result<T, CoreError>;

impl From<krusty_kms_common::KmsError> for CoreError {
    fn from(_: krusty_kms_common::KmsError) -> Self {
        // Intentionally opaque: never propagate krusty error detail that could
        // conceivably echo key material into logs.
        CoreError::Crypto
    }
}
