//! Persist this CLI's pairing so authenticated calls don't re-prompt each run.
//!
//! Stored next to the app's own state, in the same data dir, as
//! `cli-token.json` with `0600` perms (unix). It holds a bearer token — the
//! same secret the desktop app issues to any paired client — so it is treated
//! like one: owner-only, never logged.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::discovery;

/// What `companion_requestPairing` returns and what we cache locally.
#[derive(Serialize, Deserialize)]
pub struct Pairing {
    pub client_id: String,
    pub token: String,
}

fn token_path() -> Result<PathBuf, String> {
    Ok(discovery::data_dir()?.join("cli-token.json"))
}

/// Load the saved pairing, or `None` if we've never paired (or it's unreadable).
pub fn load() -> Option<Pairing> {
    let bytes = std::fs::read(token_path().ok()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist a pairing with owner-only perms.
pub fn save(p: &Pairing) -> Result<(), String> {
    let path = token_path()?;
    let bytes = serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not set perms on {}: {e}", path.display()))?;
    }
    Ok(())
}
