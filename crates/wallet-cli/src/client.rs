//! HTTP client for the loopback wallet service.
//!
//! Every call carries the `X-Companion-Client` header the service's transport
//! guard requires (its absence forces any browser into a CORS preflight the
//! server never answers — spec §5.4). reqwest sends no `Origin`/`Referer` and a
//! loopback `Host`, so well-formed native requests pass the guard.

use serde_json::{json, Value};

/// Identifies this caller in the `X-Companion-Client` header and request log.
const CLIENT_HEADER: &str = "strkd-cli";

pub struct Client {
    http: reqwest::Client,
    base_url: String,
}

impl Client {
    /// Connect by discovering the running app's port. Does no network I/O yet.
    pub fn connect() -> Result<Self, String> {
        Ok(Client {
            http: reqwest::Client::new(),
            base_url: crate::discovery::service_url()?,
        })
    }

    /// `GET /` — the open, self-describing usage doc (no auth, no transport
    /// guard).
    pub async fn usage(&self) -> Result<Value, String> {
        self.http
            .get(&self.base_url)
            .send()
            .await
            .map_err(|e| unreachable_err(&e))?
            .json()
            .await
            .map_err(|e| format!("could not decode usage doc: {e}"))
    }

    /// `POST /` — a JSON-RPC 2.0 call. Attaches the bearer token when present.
    /// Maps a JSON-RPC `error` object to `Err(..)` and returns the bare
    /// `result` on success.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        token: Option<&str>,
    ) -> Result<Value, String> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let mut req = self
            .http
            .post(&self.base_url)
            .header("x-companion-client", CLIENT_HEADER)
            .json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp: Value = req
            .send()
            .await
            .map_err(|e| unreachable_err(&e))?
            .json()
            .await
            .map_err(|e| format!("could not decode response: {e}"))?;

        if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(format!("wallet returned error {code}: {msg}"));
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| "response had neither result nor error".to_string())
    }
}

fn unreachable_err(e: &reqwest::Error) -> String {
    format!("could not reach the wallet service ({e}). Is the strkd app still running?")
}
