//! Caller authentication: pairing store + token verification (spec §5.5).
//!
//! Each paired client gets a high-entropy bearer token. Only the SHA-256 hash
//! of the token is stored, never the token itself. Callers present the token on
//! every request; we hash and compare.
//!
//! The store is **persisted** to a JSON file (`clients.json`, `0600`) so
//! pairings and grants survive restarts. Only token *hashes* are written — an
//! attacker with the file cannot recover the tokens.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Whether a client is an autonomous agent (scoped to its own agent-domain
/// accounts) or a user-facing app (sees user accounts). See spec §6.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Agent,
    App,
}

/// A paired client. The token is not stored — only its hash (hex).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedClient {
    pub id: String,
    pub label: String,
    pub kind: ClientKind,
    /// Unix-ms expiry of an auto-approval grant, if any. While active, the
    /// client's own-account operations skip the menu-bar prompt (funding still
    /// always prompts). `None` = no grant.
    #[serde(default)]
    pub granted_until: Option<u64>,
    /// SHA-256 of the bearer token, hex-encoded.
    token_hash: String,
}

impl PairedClient {
    /// Whether an auto-approval grant is currently active at `now_ms`.
    pub fn grant_active(&self, now_ms: u64) -> bool {
        self.granted_until.map(|t| t > now_ms).unwrap_or(false)
    }
}

/// Non-secret view of a paired client for the UI (no token material).
#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub id: String,
    pub label: String,
    pub kind: ClientKind,
    pub granted_until: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ClientStore {
    clients: Vec<PairedClient>,
    /// Backing file; mutations are persisted here. `None` = in-memory (tests).
    path: Option<PathBuf>,
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    // getrandom is the OS CSPRNG; failure here is unrecoverable.
    getrandom::getrandom(&mut buf).expect("OS RNG unavailable");
    hex::encode(buf)
}

impl ClientStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a file-backed store, loading existing clients (or empty if absent /
    /// unreadable). Subsequent mutations persist to `path`.
    pub fn open(path: PathBuf) -> Self {
        let clients = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<PairedClient>>(&s).ok())
            .unwrap_or_default();
        Self {
            clients,
            path: Some(path),
        }
    }

    /// Persist the current clients to the backing file (atomic; `0600`). No-op
    /// for an in-memory store. Errors are swallowed so auth never breaks on a
    /// write failure.
    fn persist(&self) {
        let Some(path) = &self.path else { return };
        let Ok(json) = serde_json::to_string_pretty(&self.clients) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_err() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        let _ = std::fs::rename(&tmp, path);
    }

    /// Register a new client. Returns `(client_id, token)`. The token is shown
    /// to the caller exactly once; only its hash is retained.
    pub fn pair(&mut self, label: impl Into<String>, kind: ClientKind) -> (String, String) {
        let id = format!("client-{}", random_hex(8));
        let token = random_hex(32);
        self.clients.push(PairedClient {
            id: id.clone(),
            label: label.into(),
            kind,
            granted_until: None,
            token_hash: hash_token(&token),
        });
        self.persist();
        (id, token)
    }

    /// Grant a client an auto-approval window ending at `until_ms`. Returns false
    /// if the id is unknown.
    pub fn grant(&mut self, id: &str, until_ms: u64) -> bool {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == id) {
            c.granted_until = Some(until_ms);
            self.persist();
            true
        } else {
            false
        }
    }

    /// Revoke a client's grant. Returns false if the id is unknown.
    pub fn revoke_grant(&mut self, id: &str) -> bool {
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == id) {
            c.granted_until = None;
            self.persist();
            true
        } else {
            false
        }
    }

    /// The id of an existing client with this (name, kind), if any.
    pub fn find_id(&self, name: &str, kind: ClientKind) -> Option<String> {
        self.clients
            .iter()
            .find(|c| c.label == name && c.kind == kind)
            .map(|c| c.id.clone())
    }

    /// Re-attach: issue a fresh token to the existing client with this
    /// (name, kind), keeping its id, grant, and owned accounts. The old token is
    /// invalidated (only one hash is kept). Returns `(client_id, new_token)`, or
    /// `None` if no such client exists.
    pub fn reattach(&mut self, name: &str, kind: ClientKind) -> Option<(String, String)> {
        let token = random_hex(32);
        let new_hash = hash_token(&token);
        let entry = self
            .clients
            .iter_mut()
            .find(|c| c.label == name && c.kind == kind)?;
        entry.token_hash = new_hash;
        let id = entry.id.clone();
        self.persist();
        Some((id, token))
    }

    /// Non-secret listing for the UI.
    pub fn list(&self) -> Vec<ClientInfo> {
        self.clients
            .iter()
            .map(|c| ClientInfo {
                id: c.id.clone(),
                label: c.label.clone(),
                kind: c.kind,
                granted_until: c.granted_until,
            })
            .collect()
    }

    /// Resolve a bearer token to its client, if valid.
    pub fn verify(&self, token: &str) -> Option<PairedClient> {
        let h = hash_token(token);
        self.clients.iter().find(|c| c.token_hash == h).cloned()
    }

    /// Revoke a client by id (removes it entirely). Returns true if one was removed.
    pub fn revoke(&mut self, id: &str) -> bool {
        let before = self.clients.len();
        self.clients.retain(|c| c.id != id);
        let removed = self.clients.len() != before;
        if removed {
            self.persist();
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}
