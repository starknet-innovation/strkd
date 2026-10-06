# Backlog

Deferred work — things we've consciously decided *not* to build yet, with enough
context to pick them up later. Active state is in [`status.md`](./status.md);
this is the "later" pile. Newest first within each group.

## Features

### Block-sized SNIP-36 proofs (v0.14.4 large-proof path, `PROOF2`) — on hold
*Issue #29; investigated 2026-10-01 (PR #30 names the current cap). On hold since 2026-10-05:
StarkWare found soundness issues in the PROOF2 route and is patching it before it reaches
testnet. strkd stays on `PROOF1` (v1.2.2) until a fixed release lands.*

Today a SNIP-36 tx can use at most 2^20 rows per AIR component (the `PROOF1`
small prover — see [`prover.md` → Proof size limit](../code/prover.md#proof-size-limit)).

What we know (checked 2026-10-05 against `starkware-libs/sequencer` `main-v0.14.4`):

- The 0.14.4 runner stamps **every** proof `PROOF2` (`ProgramOutput::try_into_proof_facts`),
  small or large. `PROOF2` is the new circuit, not only the large mode.
- The stock runner calls only the small `privacy_recursive_prove`.
  `privacy_recursive_prove_large` (starkware-libs/proving `2b495a36`, already a dependency of
  `PRIVACY-0.14.4-RC.0`) is unused.
- [`large-prover-fallback.patch`](./large-prover-fallback.patch) (against `PRIVACY-0.14.4-RC.0`,
  `0ee373ac`) makes the runner try the small prover and fall back to the large one when it runs
  out of twiddles; `SNIP36_PROVER_MODE=small|large|auto` overrides. It builds
  (`cargo +nightly-2026-01-15 build --release -p starknet_transaction_prover --features
  stwo_proving`, with the RC.0 `scripts/requirements.txt` venv) but has **not** been run on a real
  oversized tx. Its home is snip-36-prover-backend's `setup.rs` patch step.
- snip-36-prover-backend `deps-v11` / PR #118 already ship prebuilt 0.14.4-RC.0 binaries
  (small prover only).
- As of 2026-10-05 the Sepolia gateway still refuses `PROOF2` ("not accepted by this gateway")
  and accepts `PROOF1`; mainnet is on 0.14.3.

**When it resumes:** rebase the patch on StarkWare's fixed release, prove #29's 200-intent
settlement, check it with `starknet_proof_verifier::verify_proof`, check size ≤ 480,000 bytes
(gateway `max_proof_size`), then e2e on whichever network enables `PROOF2` first. Mind the snip36
CLI's 600 s timeout on `starknet_proveTransaction`.

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
