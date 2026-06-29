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
| `config` | `ProverConfig` from env (`STRKD_PROVER`, `STRKD_MOCK_PROVE_MS`). |
| `settings` | `SettingsStore` — per-network `rpc_url` / `prover_url` / `prover_api_key` + `prover_backend`, persisted to `settings.json`, hot-reloaded on external edits. **Carries secrets — IPC-only.** |
| `storage` | `Storage` — one `ProofRecord` JSON per job under `storage/`; `ProofSummary`/`StorageStats` for the UI; `max_seq` seeds the job counter across restarts. |
| `jobs` | `Jobs` — in-memory job lifecycle (`queued`→`proving`→`succeeded`/`failed`) + a capped activity feed. |
| `prover` | The `Prover` seam (`prove`/`kind`/`ready`) + `CompanionProver` (remote-when-configured, else a deterministic mock). |
| `snip36` | Shared SNIP-36 helpers: nonce/block preflight, the CLI env (incl. the dummy `0x1` key), output parsing, and `run_and_parse`. |
| `native_prover` | `NativeProver` — runs the local/bundled `snip36 prove virtual-os` CLI. The preferred (and only real) backend. |
| `state` | `ProverState { prover, jobs, settings, storage }` — cheap to clone (all `Arc`). |
| `lib` | `build_prover_state`, `enqueue_prove`. |

## Backends (`STRKD_PROVER` / Settings `prover_backend`)

- `native` — runs the bundled `snip36` CLI on-device (real proofs).
- `remote` (default; legacy `companion`) — forwards to a per-network configured
  remote prover if set, else returns a mock proof so the app is always exercisable.

The persisted Settings toggle wins over the env/default, chosen once at startup
(so a change applies on restart).

## Env vars

| Var | Meaning | Default |
|---|---|---|
| `STRKD_PROVER` | backend: `native` \| `remote` | `remote` |
| `STRKD_MOCK_PROVE_MS` | mock-proof delay (ms) | `3000` |
| `STRKD_SNIP36_BIN` | path to the `snip36` binary | bundled `resources/prover/snip36`, else `~/Workshop/snip-36-prover-backend/target/release/snip36` |
| `STRKD_SNIP36_WORK_DIR` | dir to run the CLI from (must contain `deps/`) | alongside the binary |
| `STRKD_PROVE_TIMEOUT_SECS` | prove timeout before the process group is killed | `900` |

The desktop app sets `STRKD_SNIP36_BIN`/`STRKD_SNIP36_WORK_DIR` automatically when
a prover bundle is staged into `resources/prover/` (see
[native prover bundling](#native-prover-bundling)).

## JSON-RPC surface (in `wallet-rpc`)

Paired callers only; no approval prompt (proving changes no wallet state):

- `companion_prove { payload, network?, label? }` → `{ job_id, status }`
- `companion_proveStatus { job_id }` → job (status + `result`/`error`)
- `companion_proofActivity` → recent activity feed

For SNIP-36 the `payload` is `{ transaction: <signed invoke-v3>, block_number? }`
and the proof comes back as `{ proof (base64 STWO), proof_facts, l2_to_l1_messages }`,
which feeds `wallet_addInvokeTransaction` (`proof_facts` at sign time, `proof` on
submit). See `usage.rs` and `wallet-rpc.md`.

## Native prover bundling

The native backend needs the SNIP-36 / stwo prover stack on disk (286–403 MB).
`desktop/scripts/` stages it into `desktop/src-tauri/resources/prover/` (gitignored):

- `prover-pin.env` — the pinned release (`v1.1.3`, RC.6, **PROOF0** — verifies
  on-chain; do not bump without re-verifying against the live Sepolia verifier).
- `stage-prover.sh` — from a locally-built checkout (~286 MB).
- `stage-prover-source.sh` — build from source + apply the relocatability patch
  (~403 MB, ~30–40 min); used by CI.
- `stage-prover-prebuilt.sh` — from GitHub prebuilt artifacts (CI-friendly).

`tauri.conf.json` bundles `resources/prover/**/*`; the desktop release workflow
(`.github/workflows/desktop-release.yml`) stages the prover and builds installers.

## Tests

`crates/prover/tests/enqueue.rs` exercises the full orchestration against the
**mock** backend (no binary/network needed): enqueue → succeed → a `ProofRecord`
is persisted, and the job counter seeds past existing records. `wallet-rpc`'s
`tests/dispatch.rs` covers `companion_prove*` end-to-end through the service.
