# Backlog

Deferred work — things we've consciously decided *not* to build yet, with enough
context to pick them up later. Active state is in [`status.md`](./status.md);
this is the "later" pile. Newest first within each group.

## Features

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

## Privacy — parked

**Out of scope, not merely deferred.** See
[issue #20](https://github.com/starknet-innovation/strkd/issues/20) and
[`bramble-convergence.md`](./bramble-convergence.md) §5.6.

An implementation exists on `feat/strk20-tongo-phase3` (PR #11, unmerged):
`wallet_strk20PrepareInvoke` and `wallet_strk20InvokeTransaction` over Tongo via
`krusty-kms-sdk`. It is not being merged, because Tongo cannot express the
standard STRK20 surface — it is a per-token encrypted balance where the spec
models a note-based pool, so `invoke`, `subaccount_invoke` and `OPEN` amounts
are parsed and rejected and `wallet_strk20SubaccountCommitment` stays `-32601`.

Bramble implements that surface properly against the canonical mainnet pool
through the official Starknet Privacy SDK. Two independent privacy systems are
not interoperable in any case: different protocols, different contracts, value
shielded in one is unreachable from the other. Shipping strkd's partial version
on the **standard method names** would additionally mean the same call meant
different things in the two wallets.

If privacy returns, the options are adopting the official SDK (browser
TypeScript against a Rust wallet — a real architectural decision) or keeping
Tongo under `companion_*` names so the standard surface stays free.

## Verification owed

- **Live broadcast/declare/deploy wire format** against a node (read + estimate are
  verified; the submit hop needs a funded-account run). See
  [`wallet-rpc.md`](../code/wallet-rpc.md#node-broadcast--fee-estimation).
- **Desktop GUI** is built but not run-verified (tray, dialogs, tabs, zoom,
  notifications).
- **Derivation portability** (Argent/Braavos round-trip) per
  [`spec/portability-test-plan.md`](../../spec/portability-test-plan.md).
