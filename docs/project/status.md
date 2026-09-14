# Project Status

**Snapshot of where the project is right now.** Overwrite this file as work
progresses — it must always reflect the present. History lives in
[`progress-log.md`](./progress-log.md). Process lives in
[`workflow.md`](./workflow.md).

_Last updated: 2026-07-06_

---

## ▶ Resume here

**Recommended: run-verify end-to-end now** — the wallet is usable. On a machine
with a display, set `STRKD_RPC_URL` to a Sepolia RPC endpoint, `cd desktop &&
npm run tauri dev`, then: onboard → unlock → add account → from a second process
`curl http://127.0.0.1:<port>/` for usage → pair → `companion_requestFunding`
with `submit:true` and confirm the menu-bar approval + on-chain broadcast. This
exercises the **node wire format**, which is the one thing not verified by
automated tests (see the Node caveat in
[`wallet-rpc.md`](../code/wallet-rpc.md#node-broadcast--fee-estimation)).

**Phase 2 is effectively complete** for the in-scope methods (broadcast, fees,
switchChain, watchAsset, declare; `addStarknetChain` intentionally skipped). The
open item is the **funded-account live submit** to confirm the broadcast/declare
wire format end-to-end. After that: the **bramble convergence** work
([plan](./bramble-convergence.md)). Privacy is **parked**, not next — see
[#20](https://github.com/starknet-innovation/strkd/issues/20).

**Declare fixed 2026-09-09 (#9).** Declares failed on-chain with `Account:
invalid signature` because the wallet signed the caller's `class_hash`, while
the node derives that hash from the broadcast `contract_class` and validates
against the tx hash built from *its* value. The wallet now derives the class
hash itself (`wallet_core::class_hash`), cross-checks a supplied one (mismatch →
`114`, both hashes named), and returns a complete `BROADCASTED_DECLARE_TXN_V3`
sign-only instead of five fields. Verified against live Sepolia with
`cargo run -p wallet-rpc --example live_declare_check` (read-only): the derived
class hash matches the node's for a live class, the node returns a real estimate
for our declare object, and with validation on plus a bogus signature it fails
at `__validate_declare__` alone. See spec §7.4.1.

Start from [`desktop.md`](../code/desktop.md) (for 1) or spec
[§7.4 Broadcast modes](../../spec/wallet-companion-spec.md#74-broadcast-modes-sign-only-default-submit-opt-in) (for 2).

---

## Phase board

Phases are defined in [spec §13](../../spec/wallet-companion-spec.md#13-phasing--milestones).

### ✅ Done

- **Specification** — technical spec ([`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md))
  and derivation portability test plan
  ([`spec/portability-test-plan.md`](../../spec/portability-test-plan.md)).
- **On-device proving (`prover` crate)** — merged from `../dinner`. Generic prove
  seam + native SNIP-36 backend (Docker backend dropped), per-network settings,
  job store, on-disk proof storage. Folded onto the loopback service as
  `companion_prove` / `companion_proveStatus` / `companion_proofActivity` (paired,
  no prompt) and surfaced in a desktop **Proving** tab + Settings. Holds no key
  material (proves already-signed payloads). `companion_signAndProve`
  (approval-gated) wires signing → proving in one call: it signs the private
  virtual tx and hands it to the in-process prover. SNIP-36 is inherently two
  transactions, so the on-chain verifier invoke is still broadcast separately via
  `wallet_addInvokeTransaction` (its calldata is app-specific). **No mock backend
  or test-proof button** — both backends are real; an unconfigured one fails with
  a clear error. Unit + dispatch tests green (incl. signAndProve; the success path
  uses a test-only stub `Prover`), clippy clean. **Live-verified end-to-end on
  Sepolia (2026-07-02):** the native backend (prover pin `v1.2.2` / deps-v7)
  generated a proof-carrying invoke on-device that **verified on-chain** — the
  full sign → prove → broadcast loop works on the current 0.14.3 network.
  Reference: [`docs/code/prover.md`](../code/prover.md).
- **SNIP-12 typed-data hashing fixed** (agent feedback #7) — `wallet_signTypedData`
  now signs the exact SNIP-12 rev-1 digest `starknet.js typedData.getMessageHash`
  produces, so an account's on-chain `is_valid_signature` accepts it (the standard
  committee/multisig approval pattern). Two bugs were in `krusty-kms`
  ([PR #36](https://github.com/starknet-innovation/krusty-kms/pull/36), **merged**):
  the message prefix was `keccak("StarkNet Message")` instead of the short-string
  felt, and `shortstring` values were always ASCII-encoded instead of via
  `parse_felt` (so a domain `version`/`revision` of `"1"` hashed as `0x31`, not `1`).
  Pin bumped to krusty `main` (`b50afd6`); new `companion_typedDataHash` returns the
  digest for cross-checking; a dispatch test verifies `[r, s]` under the account key
  against a starknet.py-checked golden vector.
- **Phase 0 — crypto core (`wallet-core` crate)** — built, 15 tests green,
  clippy clean. Covers: two-domain derivation, OZ address calc, Stark signing,
  Argon2id→AES-256-GCM vault, account registry + per-caller scoping. Reference:
  [`docs/code/wallet-core.md`](../code/wallet-core.md).
- **Phase 1 (most) — RPC service (`wallet-rpc` crate)** — built, 41 tests green
  (23 dispatch + 5 log + 2 persistence + 2 session + 5 transport-unit + 4 HTTP),
  clippy clean. Covers: loopback JSON-RPC + `port.lock` discovery, a
  self-describing **`GET /` usage endpoint** (open discovery), **transport
  hardening** (Origin/Referer reject, loopback-Host check, `X-Companion-Client`
  required → 403/-32003), pairing + bearer-token auth, blocking approval broker,
  per-caller scoping, handlers for the read-only methods + `wallet_signTypedData`
  + `wallet_deploymentData` + `wallet_addInvokeTransaction` +
  `wallet_addDeclareTransaction` (+ **SNIP-36 proof-carrying invoke**:
  `proof_facts`/`proof` on `addInvokeTransaction`) + `wallet_switchStarknetChain`
  + `wallet_watchAsset` +
  `companion_*` (incl. **agent funding** `companion_requestFunding` /
  `companion_fundingSource` and **agent deploy** `companion_deployAccount`), a
  **time-bounded per-agent permission grants** (auto-approve own-account ops;
  funding always prompts; revocable, ≤90 days; **pairings + grants persisted** to
  `clients.json`; **re-attach** to recover a re-paired client's accounts; **grant
  state on `companion_getStatus`**; **`companion_requestGrant`** (agent asks for a
  grant, always prompts); **`companion_estimateFee`** + complete canonical
  sign-only tx; **auto-lock** after inactivity), a **persistent SQLite request
  log** with
  full-payload redaction toggle, and **atomic on-disk vault persistence**
  (`VaultStore`) so agent-account creation is durable (transactional, with
  rollback). Deferred methods return not-implemented. Reference:
  [`docs/code/wallet-rpc.md`](../code/wallet-rpc.md).
- **Invoke V3 primitives (`wallet-core::tx`)** — selectors, Cairo 1 multicall
  encoding, V3 hash + signing; golden `transfer`-selector KAT. Reference:
  [`docs/code/wallet-core.md`](../code/wallet-core.md).
- **Phase 2 (core) — broadcast + auto nonce/fee** — built, mock-tested. Injectable
  `StarknetRpc` node seam (`wallet-rpc::node`) + `HttpStarknetRpc`, **set at
  runtime** from the desktop **Settings** panel (`config.json`, no restart). With
  a node, `wallet_addInvokeTransaction` and `companion_requestFunding` auto-fetch
  nonce, estimate fee, and broadcast on `submit:true` (funding is now turnkey).
  Without a node → sign-only (caller supplies nonce/bounds, else `-32005`).
  **Wire format live-verified** (Sepolia v0.10) for nonce/deploy-status/estimate;
  broadcast hop pending a funded submit. Reference:
  [`docs/code/wallet-rpc.md`](../code/wallet-rpc.md#node-broadcast--fee-estimation).
- **Phase 1 wrap — desktop app shell (`desktop/`)** — **built, not run-verified**.
  Tauri 2 + React 18 + TS + Vite menu-bar app: hosts the loopback service,
  bridges service approval prompts → menu-bar dialogs (60s auto-reject),
  onboarding (generate/import + passphrase), unlock, accounts (add user
  account; **copy-address**; **Deploy** button for undeployed accounts;
  **Testnet/Mainnet subtabs** that switch the active network), request-log
  viewer, a **Connect tab** (copy-paste agent prompt), and a **Settings tab**
  (per-network RPC URLs → runtime node). Default network **Sepolia/testnet**.
  Starknet-themed icon
  (tray follows it + red-dot pending badge); ⌘-zoom; non-invasive request
  notifications. Rust shell compiles, frontend type-checks + builds, clippy
  clean. Reference: [`docs/code/desktop.md`](../code/desktop.md).
- **Workspace + dependency pinning** — Cargo workspace; `krusty-kms` pinned at
  commit `1e36829`.

### 🔄 In progress

- **Desktop app run-verification** — built but needs a human to launch and click
  through the flows (see Resume here). Not blocking other work.

### ⛔ Blocked (needs human / security review)

- **Derivation portability (T1/T2) + agent isolation (T4)** — requires running
  [`spec/portability-test-plan.md`](../../spec/portability-test-plan.md) against
  real Argent/Braavos on isolated infra, security-team sign-off. Do **not** mark
  the portability claim done from structural tests alone.
- **Mainnet enablement** — gated on an independent audit of the crypto path
  (`krusty-kms` is experimental). Spec §14.

### ⬜ Remaining

- **`addStarknetChain`** — deliberately skipped (needs a generalized `ChainId`
  beyond Sepolia/Mainnet); returns `-32601`.
- **Phase 2 verification** — node wire format **live-verified** for
  nonce/deploy-status/estimate (Sepolia v0.10, 2026-06-09); the **broadcast hop**
  (`add_invoke`/`add_deploy_account`) still needs a funded-account submit.
- **Privacy (`strk20*`)** — **parked**, not pending. An implementation exists on
  `feat/strk20-tongo-phase3` (PR #11, unmerged); Tongo cannot express the
  standard surface. See [#20](https://github.com/starknet-innovation/strkd/issues/20)
  and [`bramble-convergence.md`](./bramble-convergence.md) §5.6.
- **Invoke end-to-end** — prove the computed invoke tx hash is accepted by a
  Sepolia node / matches a golden vector (belongs with the security-reviewed
  test plan).
- **App shell** — Tauri menu-bar tray, onboarding (generate/import), confirmation
  dialogs, log viewer (frontend framework not yet chosen).
- **Hardening** — UDS + peer-cred transport option; portability tests executed;
  security audit.

---

## Test & build state

| Check | Command | State (2026-06-11) |
|---|---|---|
| Build | `cargo build` | ✅ |
| Tests (core) | `cargo test` | ✅ 98 passed (21 core + 56 dispatch + 5 log + 2 persistence + 3 session + 2 clients + 5 transport-unit + 4 HTTP) |
| Lint (core) | `cargo clippy --all-targets` | ✅ clean |
| Desktop build | `cd desktop && npm run build` + `(cd src-tauri && cargo build)` | ✅ compiles; clippy clean. GUI **not run-verified** |
| Installable bundle | `cd desktop && npm run tauri build` | ✅ produces `strkd.app` + `.dmg` (release) |

## Open decisions not yet made

- Whether v1 ships Sepolia-only until the audit, vs. Sepolia + Mainnet.

## Decided

- **Frontend stack: Tauri 2 + React 18 + TypeScript + Vite** — chosen to match
  the sibling `../dinner` project (2026-06-09).
- **Node config via the desktop Settings panel** → `config.json` (per-network
  RPC URLs); applied at runtime via `ServerState::set_node`, no restart.

## Open items to confirm against `krusty-kms` source (spec §14)

- `StarkSignature` field usage / `compute_typed_data_message_hash` input shape
  (needed for `signTypedData`).
- OZ class-hash manifest currency per network.
- Tongo SDK method signatures (Phase 3).
