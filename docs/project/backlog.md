# Backlog

Deferred work — things we've consciously decided *not* to build yet, with enough
context to pick them up later. Active state is in [`status.md`](./status.md);
this is the "later" pile. Newest first within each group.

## Features

### Block-sized SNIP-36 proofs (v0.14.4 large-proof path, `PROOF2`)
*Issue #29; investigated 2026-10-01 (PR #30 names the current cap). Up next — STRK20 local proving on mainnet depends on it ([`strk20-plan.md`](./strk20-plan.md) P6).*

Today a SNIP-36 tx can use at most 2^20 rows per AIR component (the `PROOF1`
small prover — see [`prover.md` → Proof size limit](../code/prover.md#proof-size-limit)).
Starknet v0.14.4 can prove any single-tx virtual block that would fit in a block,
up to 1.1B L2 gas, via `privacy_recursive_prove_large` (starkware-libs/proving
≥ `2b495a36`). It emits `PROOF2`. The stock `starknet_transaction_prover` never
calls it.

- **Shape:** patch the `main-v0.14.4` runner's `prove()` to use the small prover
  when the trace fits and fall back to `privacy_recursive_prove_large` when it
  doesn't. Build from source (nightly, ~40+ min) through the staging scripts, and
  bump to the 0.14.4 stack (upstream backend PR #118 / deps-v11).
- **Verify locally first:** prove a tx over the cap (#29's 200-intent settlement),
  then check it with `starknet_proof_verifier::verify_proof` from `main-v0.14.4`.
  That is the same call the node makes (`apollo_transaction_converter`). Also
  check the proof is ≤ 480,000 bytes (gateway `max_proof_size`) and that the
  `proof_facts` program hash is the one 0.14.4 expects.
- **On-chain e2e later:** as of 2026-10-01 the Sepolia gateway rejects `PROOF2`
  (`allow_proof_version_v2` is off) and accepts `PROOF1`. Mainnet is on 0.14.3.
  0.14.4 is slated for 2026-10-05 (pending governance) and its config turns
  `PROOF1` **off**, which breaks the current v1.2.2 pin. Run the e2e on whichever
  network flips first, and keep the `PROOF1` pin for Sepolia until then.
- **Watch:** the snip36 CLI's hard 600 s timeout on `starknet_proveTransaction`.
  Large proofs on a laptop may need it raised (or strkd calling the runner
  directly).

### Sweep stranded/agent funds back to the manager
*Raised by the GoL agent's feedback (item 4, alternative to re-attach), 2026-06-10.*

A **user-initiated** desktop action to drain an agent account's STRK back to the
manager account. The funds aren't lost when an agent loses its pairing — the
account's key is seed-derived, so the wallet can always sign for it; what's lost
is the *agent's RPC access*. Sweep reclaims the balance for decommissioning /
consolidation.

- **Shape:** a `sweep_account(address)` desktop command + a per-account "Sweep →
  manager" button. Reads the on-chain STRK balance, builds `transfer(manager,
  balance − gas)`, signs with the account's key, broadcasts.
- **Requirements:** a configured node; the account must hold enough STRK to pay
  **its own** gas (dust below the fee is unrecoverable without a paymaster).
- **Why deferred:** re-attach (built) prevents the stranding in the first place;
  sweep is recovery/cleanup. It also leans on the broadcast path that's still
  pending live verification. Open UX question: sweep one account vs. all orphaned
  at once.

### Notification action buttons (if not shipped inline)
Approve/Deny buttons directly on the menu-bar notification banner (vs. opening the
window). Platform-specific (`tauri-plugin-notification` action types + `onAction`);
GUI-only, needs runtime + permission verification.

## Hardening / platform

- **`addStarknetChain`** — supporting arbitrary custom networks needs generalizing
  `ChainId` beyond the Sepolia/Mainnet enum. Currently returns `-32601`.
- **UDS transport** — a Unix-domain-socket option with peer-credential / code-sign
  verification, stronger than the loopback-HTTP + token model (spec §5.5).
- **BIP-39 passphrase** — onboarding doesn't surface the optional 25th-word
  passphrase yet (the unlocked session assumes empty).
- **Windows** — `0600` file perms + tray are unix/macOS-focused; revisit for
  Windows packaging.

## Privacy — now planned

No longer parked: STRK20 against the canonical pool is planned in
[`strk20-plan.md`](./strk20-plan.md) (2026-10-05). The Tongo implementation on
`feat/strk20-tongo-phase3` (PR #11) is not revived.

## Verification owed

- **Live broadcast/declare/deploy wire format** against a node (read + estimate are
  verified; the submit hop needs a funded-account run). See
  [`wallet-rpc.md`](../code/wallet-rpc.md#node-broadcast--fee-estimation).
- **Desktop GUI** is built but not run-verified (tray, dialogs, tabs, zoom,
  notifications).
- **Derivation portability** (Argent/Braavos round-trip) per
  [`spec/portability-test-plan.md`](../../spec/portability-test-plan.md).
