//! Loopback HTTP transport (spec §7.1).
//!
//! Binds `127.0.0.1` only, accepts JSON-RPC over `POST /`, and writes a
//! `port.lock` discovery file so local callers can find the bound port. The
//! port is *sticky*: on restart we re-bind whatever `port.lock` last recorded,
//! so the service URL stays stable across reboots. We fall back to an
//! OS-assigned ephemeral port only on first run or if that port is taken.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;

/// Max request body. SNIP-36 proof-carrying invokes embed a multi-MB STWO proof,
/// so we raise this well above axum's 2 MB default.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

use crate::dispatch::{dispatch, ServerState};
use crate::error::WalletRpcError;
use crate::jsonrpc::{Request, Response};

fn bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("authorization")?.to_str().ok()?;
    Some(raw.strip_prefix("Bearer ").unwrap_or(raw).to_string())
}

/// True if `host` (a `Host` header value, possibly with `:port`) names the
/// loopback interface.
fn is_loopback_host(host: &str) -> bool {
    let h = if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6: [::1] or [::1]:port → take the part before ']'.
        rest.split(']').next().unwrap_or(rest)
    } else if host.matches(':').count() == 1 {
        // Exactly one colon: host:port (IPv4 or hostname) → drop the port.
        host.split(':').next().unwrap_or(host)
    } else {
        // No colon, or a bare IPv6 literal (many colons) → use as-is.
        host
    };
    matches!(h, "127.0.0.1" | "localhost" | "::1")
}

/// Transport hardening (spec §5.4): reject browser-shaped / cross-origin
/// requests so a malicious web page can't reach the loopback service via CSRF
/// or DNS-rebinding. Returns `Err(reason)` to reject.
///
/// Callers are native local processes; none of these conditions occur in a
/// legitimate request. The `X-Companion-Client` requirement forces any browser
/// caller into a CORS preflight, which the server never answers.
pub fn transport_guard(headers: &HeaderMap) -> Result<(), &'static str> {
    if headers.contains_key("origin") || headers.contains_key("referer") {
        return Err("Origin/Referer header not allowed");
    }
    match headers.get("host").and_then(|v| v.to_str().ok()) {
        Some(h) if is_loopback_host(h) => {}
        _ => return Err("Host must be loopback"),
    }
    match headers.get("x-companion-client").and_then(|v| v.to_str().ok()) {
        Some(v) if !v.is_empty() => {}
        _ => return Err("missing X-Companion-Client header"),
    }
    Ok(())
}

async fn rpc_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Response>) {
    if let Err(reason) = transport_guard(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(Response::err(
                Value::Null,
                WalletRpcError::TransportRejected(reason.to_string()).to_object(),
            )),
        );
    }
    let token = bearer(&headers);
    let req: Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::OK,
                Json(Response::err(Value::Null, WalletRpcError::Parse.to_object())),
            )
        }
    };
    (
        StatusCode::OK,
        Json(dispatch(&state, token.as_deref(), req).await),
    )
}

/// Self-describing usage doc (spec §7). Open discovery — no auth or transport
/// guard, since it returns only public usage info and no state changes.
async fn usage_handler(State(state): State<Arc<ServerState>>) -> Json<Value> {
    Json(crate::usage::usage_doc(&state.api_version, &state.spec_versions))
}

/// Build the axum router for the service. `GET /` (and `/usage`) serve the usage
/// doc; `POST /` is the JSON-RPC endpoint.
pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/", get(usage_handler).post(rpc_handler))
        .route("/usage", get(usage_handler))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Bind a loopback listener, optionally write the `port.lock` discovery file,
/// and spawn the server. Returns the bound address and the server task handle.
///
/// The port is sticky: if `port.lock` already records a port (from a prior
/// run), we try to re-bind it so the service URL is stable across restarts. If
/// no lock exists or that port is unavailable, the OS assigns an ephemeral one.
pub async fn bind_loopback(
    state: Arc<ServerState>,
    port_lock_path: Option<&Path>,
) -> io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = bind_sticky(port_lock_path.and_then(read_locked_port)).await?;
    let addr = listener.local_addr()?;
    if let Some(path) = port_lock_path {
        write_port_lock(path, addr.port())?;
    }
    let app = router(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((addr, handle))
}

/// Try to bind the previously-used `port` on loopback; on failure (no prior
/// port, or it's in use) fall back to an OS-assigned ephemeral port.
async fn bind_sticky(port: Option<u16>) -> io::Result<tokio::net::TcpListener> {
    if let Some(p) = port.filter(|p| *p != 0) {
        if let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            return Ok(listener);
        }
    }
    tokio::net::TcpListener::bind("127.0.0.1:0").await
}

/// Read the `port` field from an existing `port.lock`, if it parses. A missing,
/// unreadable, or malformed file yields `None` — we just fall back to ephemeral.
fn read_locked_port(path: &Path) -> Option<u16> {
    let bytes = std::fs::read(path).ok()?;
    let doc: Value = serde_json::from_slice(&bytes).ok()?;
    doc.get("port")?.as_u64()?.try_into().ok()
}

/// Write `{ "port": <port>, "nonce": <hex> }` with `0600` perms (unix).
pub fn write_port_lock(path: &Path, port: u16) -> io::Result<()> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| io::Error::other("OS RNG unavailable"))?;
    let doc = serde_json::json!({ "port": port, "nonce": hex::encode(nonce) });
    std::fs::write(path, serde_json::to_vec_pretty(&doc).unwrap())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn loopback_host_detection() {
        for ok in ["127.0.0.1", "127.0.0.1:8080", "localhost", "localhost:3000", "::1", "[::1]", "[::1]:9000"] {
            assert!(is_loopback_host(ok), "{ok} should be loopback");
        }
        for bad in ["evil.com", "evil.com:80", "192.168.1.5", "example.localhost"] {
            assert!(!is_loopback_host(bad), "{bad} should NOT be loopback");
        }
    }

    fn good_headers() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:5000".parse().unwrap());
        h.insert("x-companion-client", "agent-1".parse().unwrap());
        h
    }

    #[test]
    fn guard_accepts_a_well_formed_native_request() {
        assert!(transport_guard(&good_headers()).is_ok());
    }

    #[test]
    fn guard_rejects_origin_and_referer() {
        let mut h = good_headers();
        h.insert("origin", "http://evil.com".parse().unwrap());
        assert!(transport_guard(&h).is_err());

        let mut h = good_headers();
        h.insert("referer", "http://evil.com/x".parse().unwrap());
        assert!(transport_guard(&h).is_err());
    }

    #[test]
    fn guard_rejects_non_loopback_host() {
        let mut h = good_headers();
        h.insert("host", "evil.com".parse().unwrap());
        assert!(transport_guard(&h).is_err());
    }

    #[test]
    fn guard_requires_custom_header() {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:5000".parse().unwrap());
        assert!(transport_guard(&h).is_err());
    }

    #[test]
    fn locked_port_round_trips_through_port_lock() {
        let path = std::env::temp_dir().join(format!("strkd-portlock-{}.json", std::process::id()));
        write_port_lock(&path, 54321).unwrap();
        assert_eq!(read_locked_port(&path), Some(54321));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn locked_port_is_none_when_absent_or_malformed() {
        let missing = std::env::temp_dir().join("strkd-portlock-does-not-exist.json");
        assert_eq!(read_locked_port(&missing), None);

        let bad = std::env::temp_dir().join(format!("strkd-portlock-bad-{}.json", std::process::id()));
        std::fs::write(&bad, b"not json").unwrap();
        assert_eq!(read_locked_port(&bad), None);
        let _ = std::fs::remove_file(&bad);
    }

    #[tokio::test]
    async fn bind_sticky_reuses_a_free_port_and_falls_back_when_taken() {
        // First bind grabs an ephemeral port; the sticky path should re-bind it.
        let first = bind_sticky(None).await.unwrap();
        let port = first.local_addr().unwrap().port();
        drop(first);

        let again = bind_sticky(Some(port)).await.unwrap();
        assert_eq!(again.local_addr().unwrap().port(), port, "should reuse the freed port");

        // With that port still held, a second sticky bind must fall back, not error.
        let fallback = bind_sticky(Some(port)).await.unwrap();
        assert_ne!(fallback.local_addr().unwrap().port(), port, "should fall back to a free port");
    }
}
