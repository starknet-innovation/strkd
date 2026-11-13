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
//!
//! The CLI reads the transaction from a file (`--tx-json`; it has no stdin
//! option). For a STRK20 proof that transaction carries the viewing key, so
//! the file lives in a private temp dir ([`WorkDir`]: `0700`, the file `0600`)
//! that is removed when the prove returns, fails, or is cancelled (its future
//! dropped on a wallet lock), and at the next start if the process died
//! mid-prove ([`remove_stale_workdirs`]).

use std::path::{Path, PathBuf};
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

/// Prefix of the per-prove temp dirs: `strkd-prove-<pid>-<seq>`.
const WORKDIR_PREFIX: &str = "strkd-prove-";

/// A private per-prove temp dir, removed (with everything in it) on drop.
pub(crate) struct WorkDir {
    path: PathBuf,
}

impl WorkDir {
    /// Create `<base>/strkd-prove-<pid>-<seq>`, owner-only (`0700` on unix).
    pub(crate) fn create(base: &Path, seq: u64) -> std::io::Result<WorkDir> {
        let path = base.join(format!("{WORKDIR_PREFIX}{}-{seq}", std::process::id()));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        // A leftover dir of the same name (a recycled pid) is removed first,
        // so the new one gets fresh permissions.
        let _ = std::fs::remove_dir_all(&path);
        b.create(&path)?;
        Ok(WorkDir { path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Write `contents` to a new owner-only (`0600` on unix) file in the dir.
    pub(crate) fn write_private(&self, name: &str, contents: &[u8]) -> std::io::Result<PathBuf> {
        use std::io::Write;
        let path = self.path.join(name);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&path)?.write_all(contents)?;
        Ok(path)
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Remove `strkd-prove-<pid>-*` dirs under `base` whose process is gone (a
/// crash or kill mid-prove). Dirs of live processes — another strkd, or this
/// one — are left alone. Unix only (elsewhere it does nothing).
pub fn remove_stale_workdirs(base: &Path) {
    #[cfg(unix)]
    {
        let Ok(entries) = std::fs::read_dir(base) else { return };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(pid) = name
                .to_str()
                .and_then(|n| n.strip_prefix(WORKDIR_PREFIX))
                .and_then(|rest| rest.split('-').next())
                .and_then(|p| p.parse::<i32>().ok())
            else {
                continue;
            };
            if pid <= 0 || pid as u32 == std::process::id() || process_alive(pid) {
                continue;
            }
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = base;
}

/// Whether process `pid` exists (signal 0: no signal is sent).
#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 only checks for the process's existence and
    // our permission to signal it; it sends nothing.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
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
        // Private, and removed when `work` drops: on return, on error, and when
        // this future is dropped (a cancelled job).
        let work = WorkDir::create(&std::env::temp_dir(), self.seq.fetch_add(1, Ordering::Relaxed))
            .map_err(|e| format!("workdir: {e}"))?;
        let dir = work.path().to_path_buf();
        let out_path = dir.join("out.proof");
        let tx_path = work
            .write_private("tx_a.json", serde_json::to_string(&inp.tx).unwrap_or_default().as_bytes())
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
        drop(work);
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn base(tag: &str) -> PathBuf {
        let b = std::env::temp_dir().join(format!("strkd-native-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&b);
        std::fs::create_dir_all(&b).unwrap();
        b
    }

    #[test]
    fn workdir_is_private_and_removed_on_drop() {
        let b = base("perm");
        let w = WorkDir::create(&b, 7).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(w.path()), 0o700);
        let f = w.write_private("tx_a.json", b"{\"secret\":1}").unwrap();
        assert_eq!(mode(&f), 0o600);
        assert!(w.write_private("tx_a.json", b"x").is_err(), "never reuses an existing file");
        let dir = w.path().to_path_buf();
        drop(w);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn startup_removes_only_dead_processes_workdirs() {
        let b = base("stale");
        // A pid that just exited.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let stale = b.join(format!("{WORKDIR_PREFIX}{dead}-0"));
        let mine = b.join(format!("{WORKDIR_PREFIX}{}-0", std::process::id()));
        let parent = b.join(format!("{WORKDIR_PREFIX}{}-3", std::os::unix::process::parent_id()));
        let other = b.join("strkd-prove-notapid");
        for d in [&stale, &mine, &parent, &other] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("tx_a.json"), "{}").unwrap();
        }
        remove_stale_workdirs(&b);
        assert!(!stale.exists(), "a dead process's dir is removed");
        assert!(mine.exists() && parent.exists() && other.exists());
        let _ = std::fs::remove_dir_all(&b);
    }
}
