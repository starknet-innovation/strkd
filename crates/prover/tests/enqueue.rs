//! End-to-end of the proving orchestration against the mock backend (no remote
//! prover configured, no native binary needed): enqueue → job succeeds → a full
//! `ProofRecord` lands in storage and the job carries the proof.

use std::time::{Duration, Instant};

use prover::{build_prover_state, enqueue_prove, JobStatus, ProverConfig};
use serde_json::json;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("strkd-prover-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn mock_prove_succeeds_and_persists() {
    let data_dir = temp_dir("mock");
    let cfg = ProverConfig { prover_backend: "remote".into(), mock_prove_ms: 5 };
    let state = build_prover_state(data_dir.clone(), &cfg);

    // No remote prover configured for any network → CompanionProver mock path.
    let payload = json!({ "transaction": { "x": 1 } });
    let job_id = enqueue_prove(&state, payload, Some("test".into()), "testnet".into()).await;
    assert!(job_id.starts_with('p'));

    // Poll the in-memory job until terminal (mock delay is 5ms; give it room).
    let deadline = Instant::now() + Duration::from_secs(5);
    let job = loop {
        let j = state.jobs.get(&job_id).await.expect("job exists");
        if matches!(j.status, JobStatus::Succeeded | JobStatus::Failed) {
            break j;
        }
        assert!(Instant::now() < deadline, "job did not finish in time");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    assert_eq!(job.status, JobStatus::Succeeded, "mock proof should succeed");
    let proof = job.result.expect("succeeded job has a proof");
    assert_eq!(proof.get("mock").and_then(|m| m.as_bool()), Some(true));

    // A full record was persisted to storage/<job_id>.json.
    let rec = state.storage.get_record(&job_id).expect("record persisted");
    assert_eq!(rec.status, "succeeded");
    assert!(rec.proof.is_some());
    assert_eq!(rec.network, "testnet");
    assert_eq!(state.storage.stats().records, 1);

    let _ = std::fs::remove_dir_all(&data_dir);
}

#[tokio::test]
async fn job_counter_seeds_past_existing_records() {
    let data_dir = temp_dir("seed");
    let cfg = ProverConfig { prover_backend: "remote".into(), mock_prove_ms: 1 };

    // First state proves one job → storage now has p0.json.
    {
        let state = build_prover_state(data_dir.clone(), &cfg);
        let id = enqueue_prove(&state, json!({"a": 1}), None, "mainnet".into()).await;
        assert_eq!(id, "p0");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let j = state.jobs.get(&id).await.unwrap();
            if matches!(j.status, JobStatus::Succeeded | JobStatus::Failed) {
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // A fresh state must not reuse p0 (which would overwrite the stored record).
    let state2 = build_prover_state(data_dir.clone(), &cfg);
    let id2 = enqueue_prove(&state2, json!({"b": 2}), None, "mainnet".into()).await;
    assert_eq!(id2, "p1", "counter should seed past existing p0");

    let _ = std::fs::remove_dir_all(&data_dir);
}
