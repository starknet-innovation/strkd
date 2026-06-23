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
const APP_IDENTIFIER: &str = "org.starknet.strkd";

/// The app-data directory the desktop app uses. Matches Tauri's
/// `app_data_dir()`: `<platform data dir>/<identifier>`.
pub fn data_dir() -> Result<PathBuf, String> {
    dirs::data_dir()
        .map(|d| d.join(APP_IDENTIFIER))
        .ok_or_else(|| "could not locate this OS's data directory".to_string())
}

/// Read the bound port from `port.lock` and build the loopback service URL.
///
/// A missing or malformed lock almost always means the menu-bar app isn't
/// running — say so plainly rather than surfacing a raw I/O error.
pub fn service_url() -> Result<String, String> {
    let path = data_dir()?.join("port.lock");
    let bytes = std::fs::read(&path).map_err(|_| {
        format!(
            "strkd doesn't look like it's running — no port.lock at {}.\n\
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
