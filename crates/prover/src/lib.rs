//! `prover` — strkd's on-device proving companion (ported from `../dinner`).
//!
//! Hand it an opaque, **already-signed** payload + a network; it proves
//! on-device (native SNIP-36 / stwo) or via a network's configured remote
//! prover, and returns the proof. Every job's payload, proof, and metadata are
//! persisted to local [`storage`]. The crate holds **no key material** — it
//! receives signed transactions and proves them; signing happens upstream in
//! `wallet-core`.
//!
//! Unlike `../dinner`, this crate has **no standalone HTTP server**: proving is
//! exposed over strkd's authenticated loopback JSON-RPC service (`wallet-rpc`)
//! and the desktop app's IPC. The shared orchestration ([`enqueue_prove`]) and
//! state ([`ProverState`]) live here so both callers run one code path.

pub mod config;
pub mod jobs;
pub mod native_prover;
pub mod prover;
pub mod settings;
pub mod snip36;
pub mod state;
pub mod storage;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub use config::ProverConfig;
pub use jobs::{Activity, Job, JobStatus, Jobs};
pub use prover::{Prover, ProveRequest, ProveResult, RemoteProver};
pub use settings::{NetworkConfig, Settings, SettingsStore};
pub use state::ProverState;
pub use storage::{ProofRecord, ProofSummary, Storage, StorageStats};

/// Build shared prover state: load settings + open storage under `data_dir`, and
/// wire the configured backend. `cfg` supplies the backend choice (the persisted
/// Settings toggle wins over the env/default, chosen once at startup so a
/// settings change applies on restart). Both backends are real — there is no
/// mock; an unconfigured backend fails a prove with a clear error.
pub fn build_prover_state(data_dir: PathBuf, cfg: &ProverConfig) -> ProverState {
    let settings = Arc::new(SettingsStore::load(data_dir.join("settings.json")));
    let storage = Arc::new(Storage::new(data_dir.join("storage")));
    // Seed the job counter past existing records so ids (and `<job_id>.json`
    // filenames) don't collide with — and overwrite — prior proofs after a restart.
    let next_seq = storage.max_seq().map_or(0, |m| m + 1);
    let backend = {
        let s = settings.prover_backend_blocking();
        if s.is_empty() { cfg.prover_backend.clone() } else { s }
    };
    let prover: Arc<dyn Prover> = match backend.as_str() {
        // "remote" / "companion" → forward to the user's configured remote prover.
        "remote" | "companion" => Arc::new(RemoteProver::new(settings.clone())),
        // Default (incl. "native"): prove on-device.
        _ => Arc::new(native_prover::NativeProver::new(settings.clone())),
    };
    ProverState { prover, jobs: Arc::new(Jobs::new(next_seq)), settings, storage }
}

fn unix_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Enqueue a payload and prove it in the background. Times the proof, persists a
/// full [`ProofRecord`] (payload + proof + metadata), and updates the job/feed.
/// Shared by the JSON-RPC `companion_prove` handler and the desktop "test prove"
/// command. Returns the new job id immediately; poll [`Jobs::get`] for the result.
pub async fn enqueue_prove(
    state: &ProverState,
    payload: Value,
    label: Option<String>,
    network: String,
) -> String {
    let job_id = state.jobs.create(label.clone()).await;
    let st = state.clone();
    let jid = job_id.clone();
    tokio::spawn(async move {
        st.jobs.set_status(&jid, JobStatus::Proving).await;
        let created_at_ms = unix_ms();
        let started = std::time::Instant::now();
        let res = st
            .prover
            .prove(ProveRequest { payload: payload.clone(), label: label.clone(), network: network.clone() })
            .await;
        let prove_ms = started.elapsed().as_millis();

        let mut rec = ProofRecord {
            job_id: jid.clone(),
            label,
            network,
            created_at_ms,
            prove_ms,
            status: String::new(),
            payload,
            proof: None,
            error: None,
        };
        match res {
            Ok(out) => {
                st.jobs.succeed(&jid, out.proof.clone()).await;
                rec.status = "succeeded".into();
                rec.proof = Some(out.proof);
            }
            Err(e) => {
                st.jobs.fail(&jid, e.clone()).await;
                rec.status = "failed".into();
                rec.error = Some(e);
            }
        }
        st.storage.save(&rec);
    });
    job_id
}
