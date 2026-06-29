//! Native prover backend — runs the local `snip36` CLI directly (no Docker).
//!
//! This is the **preferred** (and, in strkd, the only real) backend:
//! self-contained, native speed, no container runtime. It runs `snip36 prove
//! virtual-os` from a working directory that contains the prover `deps/` (the
//! snip-36-prover-backend checkout, or the bundled `resources/prover/`). As long
//! as that stack is current (matches the on-chain verifier), proofs verify.
//!
//! Config (env, with sibling-checkout defaults — the desktop app overrides these
//! to point at its bundled prover):
//!   STRKD_SNIP36_BIN       path to the `snip36` binary
//!   STRKD_SNIP36_WORK_DIR  dir to run it from (must contain `deps/`)
//!
//! Backend-independent logic lives in [`crate::snip36`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::prover::{Prover, ProveRequest, ProveResult};
use crate::settings::SettingsStore;
use crate::snip36;
use crate::snip36::run_and_parse;

/// An OS-assigned free TCP port (bind :0, read it back). The window between
/// release and the runner re-binding is tiny; the CLI also re-checks the port.
fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

pub struct NativeProver {
    settings: Arc<SettingsStore>,
    bin: PathBuf,
    work_dir: PathBuf,
    http: reqwest::Client,
    seq: AtomicU64,
}

impl NativeProver {
    pub fn new(settings: Arc<SettingsStore>) -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let default_repo = format!("{home}/Workshop/snip-36-prover-backend");
        fn var(k: &str, d: &str) -> String {
            std::env::var(k).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| d.to_string())
        }
        let bin = var("STRKD_SNIP36_BIN", &format!("{default_repo}/target/release/snip36"));
        let work_dir = var("STRKD_SNIP36_WORK_DIR", &default_repo);
        NativeProver {
            settings,
            bin: PathBuf::from(bin),
            work_dir: PathBuf::from(work_dir),
            http: snip36::http_client(),
            seq: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl Prover for NativeProver {
    async fn prove(&self, req: ProveRequest) -> Result<ProveResult, String> {
        if !self.bin.exists() {
            return Err(format!(
                "snip36 binary not found at {} (set STRKD_SNIP36_BIN)",
                self.bin.display()
            ));
        }
        let inp = snip36::preflight(&self.settings, &self.http, &req).await?;

        // Absolute temp dir (temp_dir() is absolute) so the CLI's tx/output paths
        // resolve even though it runs with cwd = work_dir (to find deps/).
        let dir = std::env::temp_dir().join(format!(
            "strkd-prove-{}-{}",
            std::process::id(),
            self.seq.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("workdir: {e}"))?;
        let tx_path = dir.join("tx_a.json");
        let out_path = dir.join("out.proof");
        std::fs::write(&tx_path, serde_json::to_string(&inp.tx).unwrap_or_default())
            .map_err(|e| format!("write tx: {e}"))?;

        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.current_dir(&self.work_dir);
        for (k, v) in snip36::prove_env(&inp) {
            cmd.env(k, v);
        }
        cmd.args([
            "prove",
            "virtual-os",
            "--block-number",
            &inp.block_number.to_string(),
            "--tx-json",
            &tx_path.to_string_lossy(),
            "--rpc-url",
            &inp.rpc_url,
            "--output",
            &out_path.to_string_lossy(),
            "--strk-fee-token",
            inp.strk,
        ]);
        // The CLI spawns its runner on a fixed port (default 9900); a leaked
        // runner from a prior/interrupted run, or two concurrent proofs, collide
        // there. Give each prove its own free port so it never clashes.
        if let Some(port) = free_port() {
            cmd.arg("--port").arg(port.to_string());
        }

        let result = run_and_parse(cmd, &dir, "is STRKD_SNIP36_BIN correct?").await;
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    fn kind(&self) -> &'static str {
        "native"
    }

    /// Ready when the configured `snip36` binary exists.
    fn ready(&self) -> bool {
        self.bin.exists()
    }
}
