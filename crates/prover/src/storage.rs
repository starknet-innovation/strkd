//! Local storage of every proving job: the received payload, the generated
//! proof, and metadata (timestamp, proof-generation time, network, status).
//!
//! One JSON file per job under `data_dir/storage/`. Not explorable via the
//! JSON-RPC service — kept for audit/replay and surfaced only as aggregate stats
//! (and a desktop-IPC detail view) the user can clear from Settings.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A persisted record of one proving job.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProofRecord {
    pub job_id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub network: String,
    /// Unix-epoch milliseconds the job was created.
    pub created_at_ms: u128,
    /// Wall-clock proof-generation time, milliseconds.
    pub prove_ms: u128,
    pub status: String,
    /// The received payload (kept verbatim).
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregate storage usage.
#[derive(Clone, Debug, Serialize)]
pub struct StorageStats {
    pub records: u64,
    pub bytes: u64,
}

/// Lightweight metadata for one stored proof — for the Activity list (no heavy
/// payload/proof blobs).
#[derive(Clone, Debug, Serialize)]
pub struct ProofSummary {
    pub job_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub network: String,
    pub created_at_ms: u128,
    pub prove_ms: u128,
    pub status: String,
    /// Size of the proof blob in bytes (0 if none).
    pub proof_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Storage { dir }
    }

    /// Persist a record (best-effort; storage failures never block proving).
    pub fn save(&self, rec: &ProofRecord) {
        let path = self.dir.join(format!("{}.json", rec.job_id));
        if let Ok(s) = serde_json::to_string_pretty(rec) {
            let _ = std::fs::write(path, s);
        }
    }

    pub fn stats(&self) -> StorageStats {
        let (mut records, mut bytes) = (0u64, 0u64);
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.path().extension().map(|x| x == "json").unwrap_or(false) {
                    records += 1;
                    if let Ok(m) = e.metadata() {
                        bytes += m.len();
                    }
                }
            }
        }
        StorageStats { records, bytes }
    }

    /// Metadata for every stored proof, newest first (for the Activity list).
    pub fn list(&self) -> Vec<ProofSummary> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if !e.path().extension().map(|x| x == "json").unwrap_or(false) {
                    continue;
                }
                if let Some(rec) = std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|s| serde_json::from_str::<ProofRecord>(&s).ok())
                {
                    let proof_bytes = rec
                        .proof
                        .as_ref()
                        .and_then(|p| p.get("proof"))
                        .and_then(|p| p.as_str())
                        .map(|s| s.len())
                        .unwrap_or(0);
                    out.push(ProofSummary {
                        job_id: rec.job_id,
                        label: rec.label,
                        network: rec.network,
                        created_at_ms: rec.created_at_ms,
                        prove_ms: rec.prove_ms,
                        status: rec.status,
                        proof_bytes,
                        error: rec.error,
                    });
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.created_at_ms));
        out
    }

    /// Highest `p{n}` record number on disk, if any. Used to seed the live job
    /// counter at startup so ids — and the `<job_id>.json` filenames — don't
    /// collide with (and overwrite) existing records after a restart.
    pub fn max_seq(&self) -> Option<u64> {
        let mut max: Option<u64> = None;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name();
                let n = name
                    .to_str()
                    .and_then(|s| s.strip_suffix(".json"))
                    .and_then(|s| s.strip_prefix('p'))
                    .and_then(|s| s.parse::<u64>().ok());
                if let Some(n) = n {
                    max = Some(max.map_or(n, |m| m.max(n)));
                }
            }
        }
        max
    }

    /// The full record for one job (payload + proof + metadata), for a detail view.
    pub fn get_record(&self, job_id: &str) -> Option<ProofRecord> {
        // Guard against path traversal — job ids are simple tokens.
        if job_id.is_empty()
            || !job_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        std::fs::read_to_string(self.dir.join(format!("{job_id}.json")))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Delete all stored records; returns how many were removed.
    pub fn clear(&self) -> u64 {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.path().extension().map(|x| x == "json").unwrap_or(false)
                    && std::fs::remove_file(e.path()).is_ok()
                {
                    n += 1;
                }
            }
        }
        n
    }
}
