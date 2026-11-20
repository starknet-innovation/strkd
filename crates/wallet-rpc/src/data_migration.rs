//! One-time copy of the app's data from the directory an earlier product name
//! used.
//!
//! The data directory is named after the bundle identifier, so renaming the app
//! (strkd → Bramble Companion) moves it. On the first launch under the new
//! identifier the desktop app calls [`migrate_legacy_data_dir`], which copies the
//! old directory across: vault, pairings, settings, request log, prover data and
//! `port.lock` (so the service keeps its port and paired clients keep their URL).
//!
//! The old directory is never modified or removed. It stays as the backup, and
//! an older build can still open it.

use std::io;
use std::path::Path;

/// The bundle identifier the app used while it was called strkd.
pub const LEGACY_APP_IDENTIFIER: &str = "org.starknet.strkd";

/// Written into a migrated directory, holding the path it was copied from.
pub const MARKER_FILE: &str = "migrated-from.txt";

const VAULT_FILE: &str = "vault.bin";

#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Nothing was copied: `current` already holds data, or `legacy` has no vault.
    NotNeeded,
    /// `legacy` was copied into `current`; `files` regular files were carried.
    Migrated { files: usize },
}

/// Copy `legacy` into `current` if `current` is missing or empty and `legacy`
/// holds a vault.
///
/// The copy is built in a sibling staging directory and renamed into place, so
/// `current` either does not exist or is complete: a crash part-way leaves the
/// staging directory, which the next call discards and rebuilds. Only regular
/// files and directories are copied; symlinks and other entries are skipped.
///
/// An error means nothing was put at `current`. The caller should stop instead
/// of starting with an empty data directory, which would show onboarding to a
/// user who already has a wallet.
pub fn migrate_legacy_data_dir(legacy: &Path, current: &Path) -> io::Result<Migration> {
    if !is_missing_or_empty(current)? || !legacy.join(VAULT_FILE).is_file() {
        return Ok(Migration::NotNeeded);
    }
    let name = current
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "data dir has no name"))?;
    let mut staging_name = name.to_os_string();
    staging_name.push(".migrating");
    let staging = current.with_file_name(staging_name);
    if staging.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(&staging)?;
    }

    let files = copy_tree(legacy, &staging)?;
    std::fs::write(staging.join(MARKER_FILE), legacy.display().to_string())?;
    if current.exists() {
        // Empty (checked above); a rename onto an existing directory is not
        // portable.
        std::fs::remove_dir(current)?;
    }
    std::fs::rename(&staging, current)?;
    Ok(Migration::Migrated { files })
}

fn is_missing_or_empty(dir: &Path) -> io::Result<bool> {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => Ok(entries.next().is_none()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

/// Copy `from` to a new directory `to`, keeping permissions. Returns the number
/// of files copied.
fn copy_tree(from: &Path, to: &Path) -> io::Result<usize> {
    std::fs::create_dir_all(to)?;
    std::fs::set_permissions(to, std::fs::metadata(from)?.permissions())?;
    let mut files = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            files += copy_tree(&entry.path(), &dest)?;
        } else if kind.is_file() {
            // `fs::copy` carries the permission bits (the vault and token files
            // are owner-only).
            std::fs::copy(entry.path(), &dest)?;
            files += 1;
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fresh base dir holding `legacy/` (with a vault) and no `current/`.
    fn setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir()
            .join(format!("bramble-companion-migrate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let legacy = base.join(LEGACY_APP_IDENTIFIER);
        std::fs::create_dir_all(legacy.join("prover/storage")).unwrap();
        std::fs::write(legacy.join("vault.bin"), b"vault").unwrap();
        std::fs::write(legacy.join("clients.json"), b"{}").unwrap();
        std::fs::write(legacy.join("port.lock"), br#"{"port":4242}"#).unwrap();
        std::fs::write(legacy.join("prover/storage/proof-1"), b"proof").unwrap();
        let current = base.join("org.starknet.bramble-companion");
        (base, legacy, current)
    }

    #[test]
    fn copies_everything_and_leaves_the_old_dir_alone() {
        let (base, legacy, current) = setup("copy");
        assert_eq!(
            migrate_legacy_data_dir(&legacy, &current).unwrap(),
            Migration::Migrated { files: 4 }
        );
        assert_eq!(std::fs::read(current.join("vault.bin")).unwrap(), b"vault");
        assert_eq!(std::fs::read(current.join("port.lock")).unwrap(), br#"{"port":4242}"#);
        assert_eq!(std::fs::read(current.join("prover/storage/proof-1")).unwrap(), b"proof");
        assert_eq!(
            std::fs::read_to_string(current.join(MARKER_FILE)).unwrap(),
            legacy.display().to_string()
        );
        // The source is untouched and no staging dir is left behind.
        assert_eq!(std::fs::read(legacy.join("vault.bin")).unwrap(), b"vault");
        assert!(!legacy.join(MARKER_FILE).exists());
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 2);
        // A second launch does nothing.
        assert_eq!(migrate_legacy_data_dir(&legacy, &current).unwrap(), Migration::NotNeeded);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn never_touches_a_data_dir_that_already_holds_something() {
        let (base, legacy, current) = setup("occupied");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("config.json"), b"mine").unwrap();
        assert_eq!(migrate_legacy_data_dir(&legacy, &current).unwrap(), Migration::NotNeeded);
        assert!(!current.join("vault.bin").exists());
        assert_eq!(std::fs::read(current.join("config.json")).unwrap(), b"mine");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn fills_an_existing_empty_data_dir() {
        let (base, legacy, current) = setup("empty");
        std::fs::create_dir_all(&current).unwrap();
        assert!(matches!(
            migrate_legacy_data_dir(&legacy, &current).unwrap(),
            Migration::Migrated { .. }
        ));
        assert!(current.join("vault.bin").is_file());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn needs_a_vault_in_the_old_dir() {
        let (base, legacy, current) = setup("novault");
        std::fs::remove_file(legacy.join("vault.bin")).unwrap();
        assert_eq!(migrate_legacy_data_dir(&legacy, &current).unwrap(), Migration::NotNeeded);
        assert!(!current.exists());
        // So does a missing old dir.
        assert_eq!(
            migrate_legacy_data_dir(&base.join("absent"), &current).unwrap(),
            Migration::NotNeeded
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn rebuilds_a_staging_dir_left_by_a_crash() {
        let (base, legacy, current) = setup("stale");
        let staging = base.join("org.starknet.bramble-companion.migrating");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("half-written"), b"x").unwrap();
        assert!(matches!(
            migrate_legacy_data_dir(&legacy, &current).unwrap(),
            Migration::Migrated { .. }
        ));
        assert!(!current.join("half-written").exists());
        assert!(!staging.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn keeps_owner_only_permissions_and_skips_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let (base, legacy, current) = setup("perms");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        std::fs::set_permissions(legacy.join("vault.bin"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", legacy.join("link")).unwrap();
        assert_eq!(
            migrate_legacy_data_dir(&legacy, &current).unwrap(),
            Migration::Migrated { files: 4 }
        );
        assert_eq!(mode(&current.join("vault.bin")), 0o600);
        assert_eq!(mode(&current), 0o700);
        assert!(current.join("link").symlink_metadata().is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
