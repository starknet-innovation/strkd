# `prover` crate

On-device proving companion, ported from [`../dinner`](https://github.com/starknet-innovation/dinner).
Hand it an opaque, **already-signed** payload + a network; it proves locally and
returns the proof. It holds **no key material** — signing happens upstream in
`wallet-core`, and `prover` only ever sees a signed transaction.

> For *why* the wallet and prover are split this way, see
> [`architecture.md` → Proving sits downstream of signing](./architecture.md#proving-sits-downstream-of-signing).

## What changed from `../dinner`

dinner was a standalone app with its own open HTTP server. In strkd it is a
library crate with **no HTTP layer of its own**: proving is exposed over the
wallet's authenticated loopback JSON-RPC service (`companion_prove*`) and the
desktop's IPC. Concretely:

- **Dropped:** dinner's `lib.rs` router/handlers/`serve`, `main.rs`, `openapi.rs`,
  and the **Docker** backend.
- **Relocated:** the `run_and_parse` process-group runner (the `libc` user) moved
  from `docker_prover.rs` into `snip36.rs`, since the native backend shares it.
- **Renamed:** the `DINNER_*` env vars are now `STRKD_*` (see below); the proof
  temp dir prefix is `strkd-prove-…`.
- **Added:** `ProverState` + `build_prover_state(data_dir, cfg)` + `enqueue_prove`
  as the one orchestration path both the RPC handlers and desktop IPC call.

## Modules

| Module | Responsibility |
|---|---|
| `config` | `ProverConfig` from env (`STRKD_PROVER`). |
| `settings` | `SettingsStore` — per-network `rpc_url` / `prover_url` / `prover_api_key` + `prover_backend`, persisted to `settings.json`, hot-reloaded on external edits. **Carries secrets — IPC-only.** In the desktop, `rpc_url` is **mirrored from the wallet's shared RPC config** (Settings shows one RPC field per network for both wallet + prover), not edited here. |
| `storage` | `Storage` — one `ProofRecord` JSON per job under `storage/`; `ProofSummary`/`StorageStats` for the UI; `max_seq` seeds the job counter across restarts. |
| `jobs` | `Jobs` — in-memory job lifecycle (`queued`→`proving`→`succeeded`/`failed`) + a capped activity feed. Jobs can be owned by a client (`JobOptions::client_id`, read with `get_for`) and ephemeral (`result_ttl_ms`: the result is dropped that long after the job finishes, or at once by `wipe_ephemeral`). Failures can carry `error_reason`. |
| `prover` | The `Prover` seam (`prove`/`kind`/`ready`) + `RemoteProver` (forwards to a configured remote prover; **no mock** — errors if no URL is set). |
| `snip36` | Shared SNIP-36 helpers: nonce/block preflight, the CLI env (incl. the dummy `0x1` key), output parsing, and `run_and_parse`. |
| `native_prover` | `NativeProver` — runs the local/bundled `snip36 prove virtual-os` CLI. The default, preferred backend. |
| `state` | `ProverState { prover, jobs, settings, storage }` — cheap to clone (all `Arc`). |
| `lib` | `build_prover_state`, `enqueue_prove` / `enqueue_prove_as` (recorded), `prove_unrecorded` / `enqueue_prove_unrecorded` (nothing persisted). |

## Backends (`STRKD_PROVER` / Settings `prover_backend`)

Both backends produce **real** proofs — there is no mock. An unconfigured backend
fails the prove with a clear, actionable error rather than returning a fake proof.

- `native` (**default**) — runs the bundled `snip36` CLI on-device. Fails with
  "snip36 binary not found" if no prover bundle is staged.
- `remote` (legacy alias `companion`) — forwards to the per-network remote prover
  configured in Settings. Fails with "no remote prover configured" if no URL is set.

The persisted Settings toggle wins over the env/default, chosen once at startup
(so a change applies on restart).

## Env vars

| Var | Meaning | Default |
|---|---|---|
| `STRKD_PROVER` | backend: `native` \| `remote` | `native` |
| `STRKD_SNIP36_BIN` | path to the `snip36` binary | bundled `resources/prover/snip36`, else `~/Workshop/snip-36-prover-backend/target/release/snip36` |
| `STRKD_SNIP36_WORK_DIR` | dir to run the CLI from (must contain `deps/`) | alongside the binary |
| `STRKD_PROVE_TIMEOUT_SECS` | prove timeout before the process group is killed | `900` |

The desktop app sets `STRKD_SNIP36_BIN`/`STRKD_SNIP36_WORK_DIR` automatically when
a prover bundle is staged into `resources/prover/` (see
[native prover bundling](#native-prover-bundling)).

## JSON-RPC surface (in `wallet-rpc`)

Paired callers only. The first three change no wallet state, so they don't prompt:

- `companion_prove { payload, network?, label? }` → `{ job_id, status }` — prove an
  already-signed payload the caller supplies.
- `companion_proveStatus { job_id }` → job (status + `result`/`error`). Every
  job belongs to the paired client that queued it (`enqueue_prove_as` /
  `enqueue_prove_unrecorded` tag it, `Jobs::get_for` reads it): any other
  client gets "unknown job" (114), the same answer as for a missing id. Before
  #48 part 3, any paired client could poll any job by its sequential id.
  Untagged jobs (the desktop's own) are not visible over RPC.
- `companion_proofActivity` → recent activity feed of the caller's own jobs
  (`Jobs::recent_activity_for`; the desktop IPC feed lists all)
- `companion_signAndProve { account_address, calls, resource_bounds, nonce?, block_number?, chainId?, label? }`
  → `{ job_id, status, transaction_hash, next }` — **approval-gated**; signs the
  virtual tx itself, then proves it (below).

### Unrecorded jobs (payloads that carry a secret)

A STRK20 proof invocation holds the subject's viewing key in its calldata, so
it must never reach `Storage`. Two paths keep it off disk:

- `prove_unrecorded` (synchronous, the one-shot `wallet_strk20*` methods): the
  job shows in the feed, is untagged (no RPC client can poll it) and ephemeral.
- `enqueue_prove_unrecorded(state, payload, label, network, client_id, finish)`
  (asynchronous, `companion_strk20Prove`): writes neither the payload nor the
  result to `Storage`. The job is owned by `client_id` and ephemeral
  (`UNRECORDED_RESULT_TTL_MS`, 1 h after it finishes). `finish(job_id, outcome)`
  runs in the background task and turns the prover's outcome into the job's
  result or `JobFailure { reason, message }`; the caller makes errors safe
  there (wallet-rpc returns only fixed text for STRK20). The payload is dropped
  as soon as the prover returns.

**Wiped on lock.** `ServerState::with_prover` adds a lock hook to the session
(`WalletSession::add_lock_hook`), so every lock, by any path (desktop button,
auto-lock, tests), calls `Jobs::wipe_ephemeral`: finished ephemeral results are
dropped (`result_dropped: "wiped_on_lock"`), and running ephemeral jobs finish
without a result (`error_reason: "WIPED_ON_LOCK"`; `Jobs::is_wiped` lets a job
stop before submitting). A running ephemeral job is also **cancelled**: each
has a `Jobs::cancel_signal` (`tokio::sync::Notify`, a stored permit) that the
wipe fires, and `prove_or_cancel` then drops the prover's future, so `finish`
never runs. Dropping the native backend's future kills the `snip36` process
group (`snip36::GroupKiller`, armed while the CLI runs) and removes its temp
dir (`native_prover::WorkDir`'s `Drop`). `prove_unrecorded` returns
`WIPED_MESSAGE` then. `Jobs` uses a plain mutex, never held across an
`.await`, so the hook can run from the synchronous lock path.

**Remaining exposure:** the `snip36` CLI reads the transaction only from a file
(`--tx-json`; no stdin option in `--help` as of the v1.4.2 bundle; its source
reads the path once with `read_to_string`, so `/dev/stdin` might work but is
untested). The native backend writes the transaction (and so the viewing key
and, for STRK20, the batch's channel keys) to
`$TMPDIR/strkd-prove-<pid>-<n>/tx_a.json`: the dir is created `0700` and the
file `0600` (`create_new`), and the dir is removed when the CLI exits, when the
prove is cancelled by a lock, and, after a crash or kill, at the next prover
start (`remove_stale_workdirs`, which skips dirs whose pid is still alive).
While a prove runs, the file is readable by the same OS user (and root), and
the `snip36` process holds the payload in memory. A remote backend sees the
whole payload.

For SNIP-36 the `companion_prove` `payload` is `{ transaction: <signed invoke-v3>,
block_number? }` and the proof comes back as `{ proof (base64 STWO), proof_facts,
l2_to_l1_messages }`, which feeds `wallet_addInvokeTransaction` (`proof_facts` at
sign time, `proof` on submit). See `usage.rs` and `wallet-rpc.md`.

### `companion_signAndProve` — sign + prove in one call

SNIP-36 is a **two-transaction** flow: a private virtual **Tx A** (calls e.g.
`create_proof(public, private)`) is signed and proven off-chain, then a separate
verifier **Tx B** (e.g. `verify_result(public_message)`) is broadcast carrying
`proof_facts` + `proof`. `companion_signAndProve` owns the key-holding half: it
signs Tx A (a standard v3 invoke — **not** proof-carrying; `proof_facts` are an
*output* of proving) and hands the signed tx straight to the in-process prover, so
the caller skips the manual `wallet_addInvokeTransaction(sign-only)` →
`companion_prove` round-trip and the secret never leaves the device.

`resource_bounds` is **required** — the virtual tx carries private calldata, so
strkd refuses to fee-estimate it online (that would leak the inputs to the RPC
node; matches the SNIP-36 "fee estimation on virtual tx" pitfall). It's
approval-gated (a real signature, though proven locally and never broadcast).

It deliberately does **not** build or broadcast Tx B: that invoke's calldata is
decoded from the prover's L2→L1 message and is application-specific, so a generic
wallet can't assemble it. The caller takes `result.{proof,proof_facts,l2_to_l1_messages}`
and broadcasts Tx B via `wallet_addInvokeTransaction { proof_facts, proof, submit:true }`.

## Proof size limit

One SNIP-36 transaction can be proved only if its execution trace fits the
prover whose proofs the network's verifier accepts. Past that size the prover
panics with `Not enough twiddles!`. More RAM or a retry won't help.

- Rule of thumb ≈ 1M Cairo steps (the sequencer's own sizing guidance), but the
  bound is per AIR component: `ec_op` and `keccak` are emulated in Cairo by the
  privacy bootloader, so ECDSA/keccak-heavy code hits it well before 1M steps.
- Splitting across calls *inside* one tx doesn't help — they share one trace.
  Split across separate transactions, each proved on its own.
- `classify_prover_error` maps the panic to `transaction too large to prove: …`.
  The measured `n_steps` can't be reported: the runner logs it to stdout, which
  the snip36 CLI pipes and never reads.

**PROOF1 → V2 (PROOF2), 2026-11.** Until snip36 v1.3.1 strkd bundled the PROOF1
stack (proving-utils `v0.14.3-rust-bump`), which capped every component at
exactly 2^20 rows (`CAIRO_TRACE_LOG_SIZE = 20`, blowup 3; issue #29: ~100 ECDSA
verifies per tx). Sepolia and Mainnet now run 0.14.4 and take V2 proofs
(snip36 v1.4.1 passed live e2e on both, 2026-11-10), and `starkware-libs/sequencer`
accepts only V2 from 0.14.5 (#15240). strkd bundles the V2 stack from snip36
v1.4.2. Its per-tx limit has not been re-measured yet; re-run the #29 batch
sweep before quoting a number.

## Native prover bundling

The native backend needs the SNIP-36 / stwo prover stack on disk (286–403 MB).
`desktop/scripts/` stages it into `desktop/src-tauri/resources/prover/` (gitignored):

- `prover-pin.env` — the pinned release (`v1.4.2`, deps-v16: the V2 / PROOF2
  prover with the reviewed low-memory port, proof bytes identical to stock V2).
  Previous pin v1.3.1 (PROOF1) was live-verified on Sepolia 2026-10-27.
  Do not bump without re-verifying against the live Sepolia verifier.
- `stage-prover.sh` — from a locally-built checkout (~286 MB).
- `stage-prover-source.sh` — build from source + apply the relocatability patch
  (~403 MB, ~30–40 min); used by CI.
- `stage-prover-prebuilt.sh` — from GitHub prebuilt artifacts (CI-friendly).

`tauri.conf.json` bundles `resources/prover/**/*`; the desktop release workflow
(`.github/workflows/desktop-release.yml`) stages the prover and builds installers.

## Tests

Since there is no mock backend, the success path is exercised with a **test-only
stub `Prover`** (in the test code, never shipped):

- `crates/prover/tests/enqueue.rs` drives the orchestration through the real
  failure path — an unconfigured `remote` backend fails fast with a clear error
  and still persists a `failed` `ProofRecord` — and checks the job counter seeds
  past existing records. It also checks that `enqueue_prove_unrecorded` writes
  nothing to storage and keeps the job for its client only; `jobs.rs` unit tests
  cover ownership, result expiry and the lock wipe.
- `wallet-rpc`'s `tests/dispatch.rs` attaches a stub `Prover` (returns a canned
  proof) to cover `companion_prove` / `companion_signAndProve` success end-to-end
  through the service, plus the unconfigured (`-32601`) and rejected (`113`) paths.
