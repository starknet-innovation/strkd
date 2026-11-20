//! Find the running wallet service.
//!
//! The desktop app writes a `port.lock` discovery file into its app-data dir
//! (spec §10). We mirror Tauri's `app_data_dir()` — `dirs::data_dir()` joined
//! with the bundle identifier — and read the bound port from it. The port is
//! sticky across app restarts (see `wallet-rpc::server::bind_loopback`), so a
//! URL learned once stays valid until the app moves it.

use std::path::PathBuf;

use serde_json::Value;

/// The desktop app's Tauri bundle identifier (see `tauri.conf.json`).
const APP_IDENTIFIER: &str = "org.starknet.bramble-companion";

/// The identifier the app used while it was called strkd.
const LEGACY_APP_IDENTIFIER: &str = "org.starknet.strkd";

/// The app-data directory the desktop app uses. Matches Tauri's
/// `app_data_dir()`: `<platform data dir>/<identifier>`.
pub fn data_dir() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|d| pick_data_dir(&d))
        .ok_or_else(|| "could not locate this OS's data directory".to_string())
}

/// The current data dir under `base`, or the legacy one when only that exists:
/// the installed app is then a build from before the rename.
fn pick_data_dir(base: &std::path::Path) -> PathBuf {
    let current = base.join(APP_IDENTIFIER);
    let legacy = base.join(LEGACY_APP_IDENTIFIER);
    if !current.exists() && legacy.exists() {
        legacy
    } else {
        current
    }
}

/// Read the bound port from `port.lock` and build the loopback service URL.
///
/// A missing or malformed lock almost always means the menu-bar app isn't
/// running — say so plainly rather than surfacing a raw I/O error.
pub fn service_url() -> Result<String, String> {
    let path = data_dir()?.join("port.lock");
    let bytes = std::fs::read(&path).map_err(|_| {
        format!(
            "Bramble Companion doesn't look like it's running — no port.lock at {}.\n\
             Start the menu-bar app, then try again.",
            path.display()
        )
    })?;
    let doc: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("port.lock is malformed: {e}"))?;
    let port = doc
        .get("port")
        .and_then(Value::as_u64)
        .filter(|p| *p != 0 && *p <= u16::MAX as u64)
        .ok_or("port.lock has no valid \"port\" field")?;
    Ok(format!("http://127.0.0.1:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI finds the app through its data dir, which Tauri names after the
    /// bundle identifier. The two are set in different files, so check they agree.
    #[test]
    fn app_identifier_matches_the_desktop_bundle() {
        let conf: Value =
            serde_json::from_str(include_str!("../../../desktop/src-tauri/tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], APP_IDENTIFIER);
    }

    #[test]
    fn falls_back_to_the_legacy_dir_only_when_it_is_the_only_one() {
        let base = std::env::temp_dir()
            .join(format!("bramble-companion-cli-discovery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (current, legacy) = (base.join(APP_IDENTIFIER), base.join(LEGACY_APP_IDENTIFIER));
        assert_eq!(pick_data_dir(&base), current);
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(pick_data_dir(&base), legacy);
        std::fs::create_dir_all(&current).unwrap();
        assert_eq!(pick_data_dir(&base), current);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
