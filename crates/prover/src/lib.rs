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
//!
//! Payloads that carry a secret (a STRK20 proof invocation holds the viewing
//! key in its calldata) go through [`prove_unrecorded`] or
//! [`enqueue_prove_unrecorded`] instead, which write nothing to [`storage`].
//! The native backend still hands the payload to the `snip36` CLI through a
//! private temp file while it runs (see [`native_prover`]).

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
pub use jobs::{Activity, Job, JobFailure, JobOptions, JobStatus, Jobs};
pub use prover::{Prover, ProveRequest, ProveResult, RemoteProver};
pub use settings::{NetworkConfig, Settings, SettingsStore, Strk20Config};
pub use state::ProverState;
pub use storage::{ProofRecord, ProofSummary, Storage, StorageStats};

/// Build shared prover state: load settings + open storage under `data_dir`, and
/// wire the configured backend. `cfg` supplies the backend choice (the persisted
/// Settings toggle wins over the env/default, chosen once at startup so a
/// settings change applies on restart). Both backends are real — there is no
/// mock; an unconfigured backend fails a prove with a clear error.
pub fn build_prover_state(data_dir: PathBuf, cfg: &ProverConfig) -> ProverState {
    // A crash or kill mid-prove leaves the native prover's temp dir (which
    // holds the transaction, and for STRK20 its viewing key) behind.
    native_prover::remove_stale_workdirs(&std::env::temp_dir());
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
    enqueue_prove_as(state, payload, label, network, None).await
}

/// [`enqueue_prove`], with the job owned by `client_id`: [`Jobs::get_for`]
/// shows it to that client only. The JSON-RPC handlers tag every job with the
/// paired client that queued it.
pub async fn enqueue_prove_as(
    state: &ProverState,
    payload: Value,
    label: Option<String>,
    network: String,
    client_id: Option<String>,
) -> String {
    let job_id = state.jobs.create_with(label.clone(), JobOptions { client_id, result_ttl_ms: None }).await;
    let st = state.clone();
    let jid = job_id.clone();
    tokio::spawn(async move {
        st.jobs.set_status(&jid, JobStatus::Proving).await;
        let created_at_ms = unix_ms();
        let started = std::time::Instant::now();
        let res = st
            .prover
            .prove(ProveRequest { payload: payload.clone(), label: label.clone(), network: network.clone(), allow_empty_signature: false })
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

/// Prove and wait, **without persisting anything** to [`Storage`]. For payloads
/// that carry a secret — a STRK20 proof invocation has the account's viewing
/// key in its calldata. The job still shows in the activity feed (label,
/// status, timing); its result is the proof only. If the wallet locks while it
/// runs ([`Jobs::wipe_ephemeral`]), the prover is stopped and this returns an
/// error.
pub async fn prove_unrecorded(
    state: &ProverState,
    payload: Value,
    label: Option<String>,
    network: String,
) -> Result<Value, String> {
    // Untagged (no client can poll it) and ephemeral: the caller already gets
    // the proof as this function's return value.
    let opts = JobOptions { client_id: None, result_ttl_ms: Some(UNRECORDED_RESULT_TTL_MS) };
    let job_id = state.jobs.create_with(label.clone(), opts).await;
    state.jobs.set_status(&job_id, JobStatus::Proving).await;
    let Some(res) = prove_or_cancel(state, &job_id, ProveRequest { payload, label, network, allow_empty_signature: true }).await else {
        state.jobs.fail_with(&job_id, wiped_failure()).await;
        return Err(WIPED_MESSAGE.into());
    };
    match &res {
        Ok(out) => state.jobs.succeed(&job_id, out.proof.clone()).await,
        Err(e) => state.jobs.fail(&job_id, e.clone()).await,
    }
    res.map(|out| out.proof)
}

/// Why a stopped job has no result (also the error [`prove_unrecorded`]
/// returns then).
pub const WIPED_MESSAGE: &str = "the wallet locked while this job ran, so proving was stopped";

fn wiped_failure() -> JobFailure {
    JobFailure { reason: Some("WIPED_ON_LOCK".into()), message: format!("WIPED_ON_LOCK: {WIPED_MESSAGE}") }
}

/// Run the prover, unless the job's cancel signal fires first (the wallet
/// locked): then the prover's future is dropped — the native backend kills its
/// subprocess group and removes its temp dir — and this returns `None`.
async fn prove_or_cancel(
    state: &ProverState,
    job_id: &str,
    req: ProveRequest,
) -> Option<Result<ProveResult, String>> {
    let prove = state.prover.prove(req);
    match state.jobs.cancel_signal(job_id) {
        Some(signal) => tokio::select! {
            res = prove => Some(res),
            _ = signal.notified() => None,
        },
        None => Some(prove.await),
    }
}

/// How long an unrecorded job keeps its result after it finishes: 1 h
/// (design-48 §5.7). [`Jobs::wipe_ephemeral`] drops it sooner.
pub const UNRECORDED_RESULT_TTL_MS: u128 = 3_600_000;

/// Queue a proof in the background **without persisting anything**: the
/// payload and the result never reach [`Storage`], and the job is ephemeral
/// (its result is dropped [`UNRECORDED_RESULT_TTL_MS`] after it finishes, or
/// when the wallet locks) and owned by `client_id`. The background counterpart
/// of [`prove_unrecorded`].
///
/// `finish` runs in the background task with the job id and the prover's
/// outcome (the proof, or the backend's error text, unscrubbed); what it
/// returns becomes the job's result or error. It is where the caller parses
/// the proof, turns errors into safe text, and (optionally) submits. The
/// payload is dropped as soon as the prover returns.
///
/// If the wallet locks while the prover runs, the prover is stopped (its
/// future dropped: the native backend kills the `snip36` process group and
/// removes its temp dir), `finish` never runs, and the job fails with
/// `WIPED_ON_LOCK`. What stays exposed: while the native prover runs, the
/// payload sits in a `0600` file inside a `0700` temp dir (removed when the
/// prover returns or is stopped, and at the next start after a crash), and the
/// `snip36` process holds it in memory.
pub async fn enqueue_prove_unrecorded<F, Fut>(
    state: &ProverState,
    payload: Value,
    label: Option<String>,
    network: String,
    client_id: Option<String>,
    finish: F,
) -> String
where
    F: FnOnce(String, Result<Value, String>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Value, JobFailure>> + Send + 'static,
{
    let opts = JobOptions { client_id, result_ttl_ms: Some(UNRECORDED_RESULT_TTL_MS) };
    let job_id = state.jobs.create_with(label.clone(), opts).await;
    let st = state.clone();
    let jid = job_id.clone();
    tokio::spawn(async move {
        st.jobs.set_status(&jid, JobStatus::Proving).await;
        let Some(res) = prove_or_cancel(&st, &jid, ProveRequest { payload, label, network, allow_empty_signature: true }).await else {
            drop(finish);
            st.jobs.fail_with(&jid, wiped_failure()).await;
            return;
        };
        let outcome = finish(jid.clone(), res.map(|out| out.proof)).await;
        match outcome {
            Ok(result) => st.jobs.succeed(&jid, result).await,
            Err(f) => st.jobs.fail_with(&jid, f).await,
        }
    });
    job_id
}
