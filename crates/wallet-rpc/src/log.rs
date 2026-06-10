//! Persistent request log (spec §9).
//!
//! One row per request, in SQLite. Supports a file-backed store (production)
//! and an in-memory store (tests exercise the same SQL path).
//!
//! ## Redaction (non-negotiable)
//! Key material is **never** logged: no seed, mnemonic, private key, passphrase,
//! or raw bearer token. Tokens arrive in the `Authorization` header (never in
//! params) and the dispatch layer logs only the resolved client *label/id*. The
//! request params and results we do store contain only public data (calldata,
//! signatures, addresses).
//!
//! A `full_payloads` toggle (default on, for debugging — spec §9) controls
//! whether the full `params_json` / `result_json` are persisted; when off, only
//! a redacted summary (method, client, decision, outcome, codes, timing) is
//! kept. Redaction is centralized here so there is one place to audit.
//!
//! Inserts are synchronous SQLite calls made under the caller's mutex. At
//! wallet request rates (human/agent paced) this is negligible; if throughput
//! ever matters, move `record` onto a dedicated writer task or `spawn_blocking`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde::Serialize;

/// Milliseconds since the Unix epoch (best-effort; 0 if the clock is before it).
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One logged request.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub ts_unix_ms: u64,
    pub method: String,
    /// Caller identity (`label (id)`), or `None` for unauthenticated calls.
    pub client: Option<String>,
    pub network: Option<String>,
    /// "approved" | "rejected" | "n/a".
    pub decision: String,
    /// "ok" or an error summary (never includes secrets).
    pub outcome: String,
    /// Application/JSON-RPC error code, if the request errored.
    pub error_code: Option<i64>,
    pub latency_ms: u64,
    /// Full request params JSON — present only when `full_payloads` is on.
    pub params_json: Option<String>,
    /// Full result JSON — present only when `full_payloads` is on.
    pub result_json: Option<String>,
}

/// SQLite-backed request log.
pub struct RequestLog {
    conn: Connection,
    full_payloads: bool,
}

impl RequestLog {
    /// Open (or create) a file-backed log.
    pub fn open(path: &Path, full_payloads: bool) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?, full_payloads)
    }

    /// Open an in-memory log (used by tests and ephemeral runs).
    pub fn in_memory(full_payloads: bool) -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?, full_payloads)
    }

    fn init(conn: Connection, full_payloads: bool) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS requests (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_unix_ms  INTEGER NOT NULL,
                method      TEXT    NOT NULL,
                client      TEXT,
                network     TEXT,
                decision    TEXT    NOT NULL,
                outcome     TEXT    NOT NULL,
                error_code  INTEGER,
                latency_ms  INTEGER NOT NULL,
                params_json TEXT,
                result_json TEXT
            );",
        )?;
        Ok(Self {
            conn,
            full_payloads,
        })
    }

    /// Toggle full-payload capture. When off, future records store only the
    /// redacted summary.
    pub fn set_full_payloads(&mut self, on: bool) {
        self.full_payloads = on;
    }

    pub fn full_payloads(&self) -> bool {
        self.full_payloads
    }

    /// Persist one entry. Applies the full-payload redaction. Insert errors are
    /// swallowed so logging can never break request handling.
    pub fn record(&self, mut entry: LogEntry) {
        if !self.full_payloads {
            entry.params_json = None;
            entry.result_json = None;
        }
        let _ = self.conn.execute(
            "INSERT INTO requests
                (ts_unix_ms, method, client, network, decision, outcome,
                 error_code, latency_ms, params_json, result_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                entry.ts_unix_ms as i64,
                entry.method,
                entry.client,
                entry.network,
                entry.decision,
                entry.outcome,
                entry.error_code,
                entry.latency_ms as i64,
                entry.params_json,
                entry.result_json,
            ],
        );
    }

    /// Total number of logged requests.
    pub fn count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM requests", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize
    }

    /// The most recent `limit` entries, newest first.
    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let mut stmt = match self.conn.prepare(
            "SELECT ts_unix_ms, method, client, network, decision, outcome,
                    error_code, latency_ms, params_json, result_json
             FROM requests ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(LogEntry {
                ts_unix_ms: r.get::<_, i64>(0)? as u64,
                method: r.get(1)?,
                client: r.get(2)?,
                network: r.get(3)?,
                decision: r.get(4)?,
                outcome: r.get(5)?,
                error_code: r.get(6)?,
                latency_ms: r.get::<_, i64>(7)? as u64,
                params_json: r.get(8)?,
                result_json: r.get(9)?,
            })
        });
        match rows {
            Ok(it) => it.filter_map(Result::ok).collect(),
            Err(_) => Vec::new(),
        }
    }
}
