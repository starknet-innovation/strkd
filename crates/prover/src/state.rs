//! Shared prover state, handed to the JSON-RPC `companion_*` handlers and the
//! desktop IPC commands. Cheap to clone (everything behind `Arc`).

use std::sync::Arc;

use crate::jobs::Jobs;
use crate::prover::Prover;
use crate::settings::SettingsStore;
use crate::storage::Storage;

#[derive(Clone)]
pub struct ProverState {
    /// The proving seam — the native on-device backend, or a configured remote
    /// prover. No mock backend.
    pub prover: Arc<dyn Prover>,
    /// Async proving jobs + the UI activity feed.
    pub jobs: Arc<Jobs>,
    /// User-editable per-network settings (RPC + remote prover + keys).
    /// IPC-only — never exposed over the JSON-RPC service.
    pub settings: Arc<SettingsStore>,
    /// Local store of payloads, proofs, and proof metadata.
    pub storage: Arc<Storage>,
}
