//! Shared SNIP-36 proving helpers for the native backend.
//!
//! Backend-independent logic lives here: nonce preflight, the env the CLI needs,
//! output parsing, and the process-group-managed CLI runner. (`../dinner` shared
//! this between a native and a docker backend; strkd ships only the native one,
//! so `run_and_parse` — the process-group reaper that lived in `docker_prover` —
//! now lives here next to the other shared helpers.)

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::prover::{ProveRequest, ProveResult};
use crate::settings::SettingsStore;

pub const STRK_FEE_TOKEN: &str =
    "0x04718f5a0fc34cc1af16a1cdee98ffb20c31f5cd61d6ab07201858f4287c938d";

/// Shared HTTP client for RPC preflight + remote proving.
///
/// Redirects are **not** followed: a JSON-RPC POST that 301s (e.g. `http://`→
/// `https://`) gets silently turned into a GET per RFC, the node returns
/// non-JSON, and preflight failed with an opaque decode error. With no-follow we
/// see the 3xx and report it. Only a connect timeout is set (not a request
/// timeout) so slow remote proves aren't cut off; quick RPC calls set their own.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

/// First ~300 chars of a body/stderr, trimmed — enough to diagnose, bounded.
fn snippet(s: &str) -> String {
    s.trim().chars().take(300).collect()
}

/// Bound on the prover error text returned to the caller. A revert trace names
/// the failing contract and its reason near the end, so 300 chars cut it off
/// (#50); this keeps a typical trace whole while still bounding the error.
const MAX_PROVER_ERROR_CHARS: usize = 4000;

/// `s` trimmed and bounded to about `max` chars. A longer text keeps its head
/// and its tail (where a revert reason sits) around a `[…]` marker. Cuts land
/// on a non-alphanumeric char, so no hex value is split: a caller that scrubs a
/// secret from the result (wallet-rpc's viewing key) never sees a fragment.
fn bounded(s: &str, max: usize) -> String {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let half = max / 2;
    let is_cut = |c: &char| !c.is_ascii_alphanumeric();
    // Head: up to and including the last boundary char in the first `half`.
    let head_end = chars[..half].iter().rposition(is_cut).map_or(0, |i| i + 1);
    // Tail: from the first boundary char in the last `half`.
    let tail_from = chars.len() - half;
    let tail_start = chars[tail_from..].iter().position(is_cut).map_or(chars.len(), |i| tail_from + i);
    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();
    format!("{} […] {}", head.trim_end(), tail.trim_start())
}

/// The revert reason the prover reports for a transaction that failed while it
/// executed (`... revert reason: <trace>`), without the JSON-RPC envelope that
/// closes the error's `data` string, bounded.
fn revert_reason(err: &str) -> Option<String> {
    let i = err.find("revert reason:")?;
    let reason = &err[i + "revert reason:".len()..];
    let reason = match reason.rfind("\",\"message\"") {
        Some(j) => &reason[..j],
        None => reason,
    };
    let reason = reason.trim();
    (!reason.is_empty()).then(|| bounded(reason, MAX_PROVER_ERROR_CHARS))
}

/// A JSON-RPC call with explicit handling of redirects, non-2xx, and non-JSON
/// bodies (each of which previously surfaced as `expected value at line 1
/// column 1` with no context).
async fn rpc_call(http: &reqwest::Client, rpc: &str, method: &str, params: Value) -> Result<Value, String> {
    let resp = http
        .post(rpc)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("rpc {method}: request failed ({e})"))?;
    let status = resp.status();
    if status.is_redirection() {
        let loc = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?");
        return Err(format!(
            "rpc {method}: rpc_url returned {status} → {loc}. JSON-RPC needs the final URL — \
             set the https URL directly in Settings (a 301 silently turns the POST into a GET)."
        ));
    }
    let text = resp.text().await.map_err(|e| format!("rpc {method}: reading body failed ({e})"))?;
    if !status.is_success() {
        return Err(format!("rpc {method}: HTTP {status}: {}", snippet(&text)));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|e| format!("rpc {method}: non-JSON response ({e}); body: {}", snippet(&text)))
}

/// The sequencer's own sizing guidance for one SNIP-36 tx (its README and
/// out-of-gas hint): `l2_gas.max_amount` 100M ≈ 1M Cairo steps. A rule of thumb:
/// the prover's real bound is per AIR component, so builtin-heavy code hits it
/// sooner. (The old PROOF1 stack capped every component at exactly 2^20 rows;
/// the V2 stack bundled since snip36 v1.4.2 has not been re-measured, see #29.)
pub const APPROX_MAX_STEPS: u64 = 1_000_000;

/// Strip ANSI colour escapes (color-eyre colours the CLI's final error).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The CLI's own error report: everything from its final `Error:` (color-eyre)
/// on, so the CLI's progress log lines ahead of it don't crowd out the cause.
fn error_part(stderr: &str) -> String {
    let clean = strip_ansi(stderr);
    match clean.rfind("Error:") {
        Some(i) => clean[i..].to_string(),
        None => clean,
    }
}

/// Map a known prover-CLI failure to an actionable message; else return the raw
/// stderr (bounded, see [`MAX_PROVER_ERROR_CHARS`]). Lets a driving agent react
/// instead of staring at a backtrace.
pub fn classify_prover_error(stderr: &str) -> String {
    let stderr = &error_part(stderr);
    let low = stderr.to_lowercase();
    let raw = |s: &str| bounded(s, MAX_PROVER_ERROR_CHARS);
    // First: a revert trace quotes addresses and hex data that could match the
    // keyword checks below.
    if low.contains("reverted transactions are not supported") {
        let reason = revert_reason(stderr).unwrap_or_else(|| raw(stderr));
        format!(
            "the transaction reverted while it was proved, and the prover proves only transactions \
             that succeed. It runs against the reference block's state, so every contract it touches \
             (the sender account included) must already be deployed at that block. revert reason: {reason}"
        )
    } else if low.contains("not enough twiddles") {
        format!(
            "transaction too large to prove: its execution trace exceeds the bundled SNIP-36 \
             prover's maximum size in at least one AIR component (≈{APPROX_MAX_STEPS} Cairo \
             steps as a rule of thumb). This is a fixed size of the prover the network's \
             verifier accepts, not a machine limit: more RAM or a retry won't help. Split the \
             work across separate transactions, each proved on its own (several calls inside \
             one tx share one trace). ec_op and keccak are emulated in Cairo by the prover's \
             bootloader, so ECDSA/keccak-heavy code hits the cap well before that — find the \
             largest batch that proves and chunk below it. raw: {}",
            raw(stderr)
        )
    } else if low.contains("429") || low.contains("too many requests") || low.contains("rpc provider error") {
        format!(
            "upstream RPC throttled (HTTP 429 / provider error) — proving fetches lots of state; \
             use a higher-quota RPC in Settings. raw: {}",
            raw(stderr)
        )
    } else if low.contains("state_diff_commitment") {
        format!(
            "RPC too old (missing `state_diff_commitment`) — needs a spec ≥0.10 endpoint. raw: {}",
            raw(stderr)
        )
    } else if low.contains("port") && low.contains("already in use") {
        format!("prover port in use (stale runner) — retry; strkd picks a fresh port per prove. raw: {}", raw(stderr))
    } else if low.contains("invalid starknet version") || low.contains("convert block header") {
        format!(
            "the bundled prover does not support this network's Starknet protocol version — it \
             rejected the reference block header, so the pinned prover stack predates the network's \
             current protocol. Fix: update the prover pin (desktop/scripts/prover-pin.env) to a \
             release that supports it and re-stage, or set the prover backend to `remote` (Settings) \
             and point at a compatible prover. raw: {}",
            raw(stderr)
        )
    } else {
        format!("prover failed: {}", raw(stderr))
    }
}

/// `(chain_id, strk_fee_token)` for a network name.
pub fn net_params(network: &str) -> (&'static str, &'static str) {
    match network {
        "testnet" | "sepolia" => ("SN_SEPOLIA", STRK_FEE_TOKEN),
        _ => ("SN_MAIN", STRK_FEE_TOKEN),
    }
}

/// Validated, backend-independent inputs for one prove.
pub struct ProveInputs {
    pub tx: Value,
    pub block_number: u64,
    pub chain_id: &'static str,
    pub strk: &'static str,
    pub rpc_url: String,
}

/// Invariant: the prover never signs, so it rejects an unsigned transaction up
/// front. The `signature` field must be an array; it must also be non-empty
/// unless `allow_empty` (strkd's STRK20 paths only: an owner contract decides
/// what its signature means, and `[]` can be valid for it).
fn check_signed(tx: &Value, allow_empty: bool) -> Result<(), String> {
    let signed = match tx.get("signature").and_then(|s| s.as_array()) {
        Some(a) => !a.is_empty() || allow_empty,
        None => false,
    };
    if signed {
        Ok(())
    } else {
        Err("transaction is not signed (missing/empty `signature`); the prover proves \
             pre-signed transactions only and never signs"
            .into())
    }
}

/// Validate the request and resolve the reference block + nonce, independent of
/// which backend will run the prover. Returns a clear error early (before any
/// ~minute-long proof) on the common failures.
pub async fn preflight(
    settings: &SettingsStore,
    http: &reqwest::Client,
    req: &ProveRequest,
) -> Result<ProveInputs, String> {
    let net = settings.for_network(&req.network).await;
    if net.rpc_url.is_empty() {
        return Err(format!("no RPC URL configured for {} (set it in Settings)", req.network));
    }

    let tx = req
        .payload
        .get("transaction")
        .cloned()
        .ok_or("payload missing `transaction` (the signed Tx A)")?;

    check_signed(&tx, req.allow_empty_signature)?;

    let block_number = match req.payload.get("block_number").and_then(|v| v.as_u64()) {
        Some(n) => n,
        None => latest_block(http, &net.rpc_url).await?.saturating_sub(2),
    };

    check_nonce(http, &net.rpc_url, &tx, block_number).await?;

    let (chain_id, strk) = net_params(&req.network);
    Ok(ProveInputs { tx, block_number, chain_id, strk, rpc_url: net.rpc_url })
}

/// The env vars the `snip36` CLI's shared config requires. `prove virtual-os`
/// only uses RPC + chain id; account/key are for `submit`, so they're dummy
/// `0x1` values — no real key is ever needed (the prover holds none).
pub fn prove_env(inp: &ProveInputs) -> Vec<(&'static str, String)> {
    vec![
        ("STARKNET_CHAIN_ID", inp.chain_id.to_string()),
        ("STARKNET_RPC_URL", inp.rpc_url.clone()),
        ("STARKNET_STRK_TOKEN", inp.strk.to_string()),
        ("STARKNET_ACCOUNT_ADDRESS", "0x1".to_string()),
        ("STARKNET_PRIVATE_KEY", "0x1".to_string()),
    ]
}

/// Read the prover's outputs from `dir` (`out.proof`, `out.proof_facts`,
/// `out.raw_messages.json`) into the result shape callers consume.
pub fn parse_outputs(dir: &Path) -> Result<ProveResult, String> {
    let proof = std::fs::read_to_string(dir.join("out.proof"))
        .map_err(|e| format!("read proof: {e}"))?
        .trim()
        .to_string();
    let facts: Value = std::fs::read_to_string(dir.join("out.proof_facts"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let messages: Value = std::fs::read_to_string(dir.join("out.raw_messages.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("l2_to_l1_messages").cloned())
        .unwrap_or(Value::Array(vec![]));
    Ok(ProveResult {
        proof: json!({ "proof": proof, "proof_facts": facts, "l2_to_l1_messages": messages }),
    })
}

async fn latest_block(http: &reqwest::Client, rpc: &str) -> Result<u64, String> {
    let v = rpc_call(http, rpc, "starknet_blockNumber", json!([])).await?;
    v.get("result")
        .and_then(|r| r.as_u64())
        .ok_or_else(|| format!("rpc blockNumber: unexpected response {v}"))
}

/// Fetch the account nonce at `block` and check it matches the tx nonce — saves
/// a ~minute-long proof when the nonce is wrong.
async fn check_nonce(http: &reqwest::Client, rpc: &str, tx: &Value, block: u64) -> Result<(), String> {
    let (Some(sender), Some(tx_nonce)) = (
        tx.get("sender_address").and_then(|v| v.as_str()),
        tx.get("nonce").and_then(|v| v.as_str()),
    ) else {
        return Ok(()); // can't check without sender + nonce
    };
    let v = rpc_call(http, rpc, "starknet_getNonce", json!([{ "block_number": block }, sender])).await?;
    let Some(chain_nonce) = v.get("result").and_then(|r| r.as_str()) else {
        return Ok(()); // RPC didn't return a nonce; let the prover surface it
    };
    let parse_hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok();
    if let (Some(a), Some(b)) = (parse_hex(tx_nonce), parse_hex(chain_nonce)) {
        if a != b {
            return Err(format!(
                "nonce mismatch: virtual tx has nonce {tx_nonce} but account {sender} had \
                 nonce {chain_nonce} at block {block}. The virtual tx nonce must equal the \
                 account nonce at the reference block, not just the current nonce."
            ));
        }
    }
    Ok(())
}

/// Run a configured `prove virtual-os` command and parse its outputs from `dir`.
///
/// Runs the CLI in its own process group (unix) with a timeout
/// (`STRKD_PROVE_TIMEOUT_SECS`, default 900): on timeout or failure the whole
/// group is killed, so the runner the CLI spawns can't outlive the job and leak
/// its port. The group is also killed if this future is dropped while the CLI
/// runs (a cancelled job: the wallet locked). Recognized failures get a
/// friendly message via `classify_prover_error`.
pub(crate) async fn run_and_parse(
    mut cmd: tokio::process::Command,
    dir: &std::path::Path,
    launch_hint: &str,
) -> Result<ProveResult, String> {
    let secs: u64 = std::env::var("STRKD_PROVE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);

    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true); // reap the direct child if we bail
    #[cfg(unix)]
    cmd.process_group(0); // child leads its own group so we can reap grandchildren

    let child = cmd.spawn().map_err(|e| format!("failed to launch prover ({launch_hint}): {e}"))?;
    #[cfg(unix)]
    let pgid = child.id().map(|p| p as i32);
    #[cfg(unix)]
    let kill_group = || {
        if let Some(pg) = pgid {
            unsafe { libc::kill(-pg, libc::SIGKILL) };
        }
    };
    // Kills the group if this future is dropped mid-wait (cancellation);
    // disarmed once the CLI has exited, after which the paths below decide.
    #[cfg(unix)]
    let mut on_drop = GroupKiller(pgid);

    let waited = tokio::time::timeout(std::time::Duration::from_secs(secs), child.wait_with_output()).await;
    #[cfg(unix)]
    on_drop.disarm();
    let output = match waited {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("prover wait failed: {e}")),
        Err(_) => {
            #[cfg(unix)]
            kill_group();
            return Err(format!("prover timed out after {secs}s (killed)"));
        }
    };

    if !output.status.success() {
        // Reap any runner the CLI may have orphaned on failure.
        #[cfg(unix)]
        kill_group();
        return Err(classify_prover_error(&String::from_utf8_lossy(&output.stderr)));
    }
    parse_outputs(dir)
}

/// SIGKILLs a process group when dropped, unless disarmed.
#[cfg(unix)]
struct GroupKiller(Option<i32>);

#[cfg(unix)]
impl GroupKiller {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

#[cfg(unix)]
impl Drop for GroupKiller {
    fn drop(&mut self) {
        if let Some(pg) = self.0 {
            // SAFETY: plain syscall on a process group we spawned and still own
            // (its leader has not been reaped yet: we were still waiting on it).
            unsafe { libc::kill(-pg, libc::SIGKILL) };
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_signed_requires_signature_field_and_non_empty_unless_allowed() {
        use serde_json::json;
        let empty = json!({ "signature": [] });
        let missing = json!({});
        let not_array = json!({ "signature": "0x1" });
        let one = json!({ "signature": ["0x1"] });
        assert!(super::check_signed(&one, false).is_ok());
        assert!(super::check_signed(&one, true).is_ok());
        assert!(super::check_signed(&empty, false).unwrap_err().contains("not signed"));
        assert!(super::check_signed(&empty, true).is_ok());
        assert!(super::check_signed(&missing, true).is_err());
        assert!(super::check_signed(&not_array, true).is_err());
    }
    use super::{bounded, classify_prover_error, MAX_PROVER_ERROR_CHARS};

    /// Dropping `run_and_parse` mid-run (a cancelled job) kills the CLI and
    /// the children it spawned (the runner), not just the direct child.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_a_running_prove_kills_its_process_group() {
        let dir = std::env::temp_dir().join(format!("strkd-snip36-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("grandchild.pid");
        let mut cmd = tokio::process::Command::new("sh");
        // A background grandchild (the "runner") plus a waiting parent.
        cmd.arg("-c").arg(format!("sleep 60 & echo $! > {}; wait", pidfile.display()));
        let run = super::run_and_parse(cmd, &dir, "test");
        let pid = tokio::select! {
            _ = run => panic!("the command should still be running"),
            pid = async {
                loop {
                    if let Some(p) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
                        break p;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            } => pid,
        };
        // `run` was dropped by the select. The grandchild must be gone.
        let mut gone = false;
        for _ in 0..200 {
            // SAFETY: signal 0 only probes for the process.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(gone, "grandchild {pid} survived the cancelled prove");
    }

    #[test]
    fn unsupported_protocol_version_is_actionable() {
        // The exact shape reported from Sepolia 0.14.3 against the v1.1.3 prover.
        let stderr = r#"starknet_proveTransaction failed: {"code":-32603,"data":"Transaction execution failed: Failed to convert block header to block info: Invalid Starknet version: [0, 14, 3]","message":"Internal error"}"#;
        let msg = classify_prover_error(stderr);
        assert!(msg.contains("does not support this network's Starknet protocol"));
        assert!(msg.contains("prover-pin.env") && msg.contains("remote"));
        // Still carries the raw error for debugging.
        assert!(msg.contains("Invalid Starknet version"));
    }

    /// The CLI's stderr when the trace is over the cap: color-eyre's report
    /// (shape captured from the bundled v1.2.2 CLI) carrying the panic from #29.
    const TWIDDLES_STDERR: &str = concat!(
        "  INFO Sending starknet_proveTransaction request...\n",
        "Error: \n   0: \u{1b}[91mstarknet_proveTransaction failed: {\"code\":-32603,",
        "\"data\":\"Proving task failed to join: task 858 panicked with message ",
        "\\\"Not enough twiddles!\\\"\",\"message\":\"Internal error\"}\u{1b}[0m\n\n",
        "Location:\n   \u{1b}[35mcrates/snip36-cli/src/commands/prove.rs\u{1b}[0m:\u{1b}[35m342\u{1b}[0m\n",
    );

    #[test]
    fn twiddles_panic_names_the_cap() {
        let msg = classify_prover_error(TWIDDLES_STDERR);
        assert!(msg.starts_with("transaction too large to prove"), "{msg}");
        assert!(msg.contains("AIR component") && !msg.contains("PROOF1"));
        assert!(msg.contains("separate transactions"));
        // The raw cause is kept, starting at the error and without ANSI noise.
        assert!(msg.contains("raw: Error:"), "{msg}");
        assert!(msg.contains("Not enough twiddles") && !msg.contains('\u{1b}'));
    }

    #[test]
    fn log_lines_ahead_of_the_error_do_not_mask_it() {
        // A log line mentioning "429" must not be read as RPC throttling.
        let stderr = "  INFO fetched 429 storage keys\nError: \n   0: some other failure";
        assert_eq!(classify_prover_error(stderr), "prover failed: Error: \n   0: some other failure");
    }

    /// The shape from #50: the virtual tx reverted in the prover because the
    /// sender account wasn't deployed at the reference block yet. The revert
    /// reason comes after a long preamble, past the old 300-char cut.
    fn revert_stderr() -> String {
        let trace = format!(
            "Transaction execution has failed:\\n0: Error in the called contract (contract address: \
             0x{:064x}, class hash: 0x{:064x}, selector: 0x{:064x}):\\nError at pc=0:4573:\\n\
             Got an exception while executing a hint: Requested contract address 0x{:064x} is not deployed.",
            0xa11ce_u64, 0xc1a55_u64, 0x5e1_u64, 0xb0b_u64
        );
        format!(
            "  INFO Sending starknet_proveTransaction request...\nError: \n   0: \u{1b}[91mstarknet_proveTransaction \
             failed: {{\"code\":-32603,\"data\":\"Reverted transactions are not supported; the transaction \
             reverted during execution. revert reason: {trace}\",\"message\":\"Internal error\"}}\u{1b}[0m\n\n\
             Location:\n   crates/snip36-cli/src/commands/prove.rs:342\n"
        )
    }

    #[test]
    fn revert_reason_is_returned_whole() {
        let stderr = revert_stderr();
        let msg = classify_prover_error(&stderr);
        assert!(msg.starts_with("the transaction reverted while it was proved"), "{msg}");
        assert!(msg.contains("deployed at that block"), "{msg}");
        // The full reason survives, through the tail of the trace, and the
        // JSON-RPC envelope after it is dropped.
        assert!(msg.contains("Error in the called contract (contract address: 0x"), "{msg}");
        assert!(msg.ends_with("is not deployed."), "{msg}");
        assert!(msg.contains(&format!("0x{:064x}", 0xb0b_u64)), "{msg}");
        assert!(!msg.contains("Internal error") && !msg.contains('\u{1b}'), "{msg}");
        assert!(msg.chars().count() > 300);
    }

    #[test]
    fn long_errors_stay_bounded_and_keep_the_tail() {
        let filler = "x".repeat(10) + " ";
        let stderr = format!("Error: start {} the actual cause", filler.repeat(1000));
        let msg = classify_prover_error(&stderr);
        assert!(msg.chars().count() <= MAX_PROVER_ERROR_CHARS + 40, "{}", msg.len());
        assert!(msg.starts_with("prover failed: Error: start") && msg.ends_with("the actual cause"), "{msg}");
        assert!(msg.contains(" […] "));
    }

    #[test]
    fn bounding_never_splits_a_hex_value() {
        // A run of hex words: every cut must fall between two of them, so a
        // caller scrubbing one value (wallet-rpc's viewing key) never sees a
        // fragment of it.
        let word = format!("0x{:064x}", 0x2f6c0b3c1a9e5d4f_u64);
        let text = vec![word.as_str(); 200].join(", ");
        let out = bounded(&text, 1000);
        assert!(out.chars().count() <= 1000 + 5);
        for part in out.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()) {
            assert_eq!(part, word, "split value");
        }
        // Short text is untouched apart from trimming.
        assert_eq!(bounded("  short  ", 1000), "short");
    }

    #[test]
    fn unknown_error_falls_back() {
        let msg = classify_prover_error("some other failure");
        assert!(msg.starts_with("prover failed:"));
    }
}
