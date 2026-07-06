//! Orchestration of `enqueue_prove` against the REAL backends (there is no mock).
//! A prove that can't produce a real proof FAILS with a clear error and still
//! persists a `ProofRecord`, and the job counter seeds past existing records so
//! ids/files don't collide across restarts.
//!
//! We drive this via the `remote` backend with no prover URL configured: it
//! fails immediately (no network call) with a clear message — the de-mocked
//! behavior — which exercises the same enqueue → job-lifecycle → storage path a
//! real backend uses.

use std::time::{Duration, Instant};

use prover::{build_prover_state, enqueue_prove, Job, JobStatus, ProverConfig, ProverState};
use serde_json::json;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("strkd-prover-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Poll a job until it reaches a terminal state (or time out).
async fn run_to_terminal(state: &ProverState, job_id: &str) -> Job {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let j = state.jobs.get(job_id).await.expect("job exists");
        if matches!(j.status, JobStatus::Succeeded | JobStatus::Failed) {
            return j;
        }
        assert!(Instant::now() < deadline, "job did not finish in time");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn unconfigured_backend_fails_with_clear_error_and_persists() {
    let data_dir = temp_dir("remote-nourl");
    // `remote` backend, no prover URL set for any network → no real proof possible.
    let cfg = ProverConfig { prover_backend: "remote".into() };
    let state = build_prover_state(data_dir.clone(), &cfg);

    let job_id =
        enqueue_prove(&state, json!({ "transaction": { "x": 1 } }), Some("t".into()), "testnet".into())
            .await;
    let job = run_to_terminal(&state, &job_id).await;

    // No fake proof — it fails honestly with an actionable message.
    assert_eq!(job.status, JobStatus::Failed, "an unconfigured backend must not fake a proof");
    assert!(job.result.is_none());
    assert!(job.error.as_deref().unwrap_or_default().contains("no remote prover configured"));

    // The failure is still recorded to storage (audit trail), as a failed record.
    let rec = state.storage.get_record(&job_id).expect("record persisted");
    assert_eq!(rec.status, "failed");
    assert!(rec.proof.is_none());
    assert!(rec.error.is_some());
    assert_eq!(rec.network, "testnet");
    assert_eq!(state.storage.stats().records, 1);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn job_counter_seeds_past_existing_records() {
    let data_dir = temp_dir("seed");
    let cfg = ProverConfig { prover_backend: "remote".into() };

    // First state proves one job (it fails, but a p0.json record is written).
    {
        let state = build_prover_state(data_dir.clone(), &cfg);
        let id = enqueue_prove(&state, json!({ "a": 1 }), None, "mainnet".into()).await;
        assert_eq!(id, "p0");
        run_to_terminal(&state, &id).await;
    }

    // A fresh state must not reuse p0 (which would overwrite the stored record).
    let state2 = build_prover_state(data_dir.clone(), &cfg);
    let id2 = enqueue_prove(&state2, json!({ "b": 2 }), None, "mainnet".into()).await;
    assert_eq!(id2, "p1", "counter should seed past existing p0");

    let _ = std::fs::remove_dir_all(&data_dir);
}
