# Code Architecture

A map of the codebase: what exists, what's planned, and where the security
boundaries are. For *why* the system is shaped this way, read the technical spec
([`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md)) — this
doc describes the code, the spec describes the design.

## Workspace layout

```
strkd/
├── Cargo.toml                 # workspace root; pins krusty-kms (rev 1e36829)
├── crates/
│   ├── wallet-core/           # ✅ built — key mgmt, derivation, signing, vault
│   │   ├── src/{lib,domain,keys,vault,accounts,error}.rs
│   │   └── tests/{derivation,vault}.rs
│   └── wallet-rpc/            # ✅ built — JSON-RPC service, auth, approval, log, vault store, node
│       ├── src/{lib,jsonrpc,error,auth,approval,session,log,store,node,usage,dispatch,server}.rs
│       └── tests/{dispatch,server,log,persistence,session}.rs
├── desktop/                   # ✅ built (not run-verified) — Tauri 2 + React menu-bar app
│   ├── src/                   #   React frontend (TS + Vite)
│   └── src-tauri/             #   Rust shell (own crate, excluded from workspace)
├── docs/                      # this documentation tree
└── spec/                      # design specs (referenced by the code docs)
```

## Crates

| Crate | Status | Responsibility | Reference |
|---|---|---|---|
| `wallet-core` | ✅ built | The only crate that touches seed/private-key material. Derivation, signing, address calc, encrypted vault, account registry. | [`wallet-core.md`](./wallet-core.md) |
| `wallet-rpc` | ✅ built | Loopback JSON-RPC server, pairing/auth, approval broker, read + `signTypedData` + `addInvokeTransaction` + `companion_*` handlers (incl. `companion_prove*`), SQLite log, vault store, transport hardening. Calls into `wallet-core` and `prover`. | [`wallet-rpc.md`](./wallet-rpc.md) |
| `prover` | ✅ built | On-device proving companion (ported from `../dinner`). Generic prove seam + native SNIP-36 backend, per-network settings, job store, on-disk proof storage. **Holds no key material** — proves already-signed payloads. | [`prover.md`](./prover.md) |
| `desktop` (Tauri) | ✅ built, not run-verified | Menu-bar tray app: onboarding, unlock, accounts, confirmation dialogs, log viewer, **Proving panel**. Hosts the service + approval bridge + the prover. Tauri 2 + React. | [`desktop.md`](./desktop.md) |

Planned crates get their own `docs/code/<crate>.md` reference when they're built
(see the [doc contribution rules](../../README.md#contributing-to-the-documentation)).

## Security boundary (the most important diagram)

```
            outside callers (agents, desktop apps)
                          │  loopback JSON-RPC (axum, 127.0.0.1)
                ┌─────────▼──────────┐
                │     wallet-rpc      │  no key material here
                │  auth · approval    │  returns only public data + signatures
                │  dispatch · log     │
                └─────────┬──────────┘
                          │ in-process Rust calls (WalletSession)
                ┌─────────▼──────────┐
                │     wallet-core     │  ◀── ONLY place seeds/keys live
                │  derive · sign      │      (decrypted in memory while unlocked)
                │  vault (at rest)    │
                └─────────┬──────────┘
                          │
                    krusty-kms (crypto)
```

The approval broker sits inside `wallet-rpc`: a signing request takes a brief
session lock to resolve the in-scope account, releases it, blocks on the user's
menu-bar decision, then re-locks only to sign.

The loopback transport is hardened (spec §5.4): requests carrying
`Origin`/`Referer`, a non-loopback `Host`, or no `X-Companion-Client` header are
rejected with HTTP 403 before reaching dispatch — defeating CSRF / DNS-rebinding
from a malicious web page.

**Invariant:** private keys, seed, mnemonic, and passphrase never leave
`wallet-core`. Everything above it sees only addresses, public keys, signatures,
and signed payloads. Any change that risks crossing this line is a security-gated
change (see [workflow §security gates](../project/workflow.md#security-gates-non-negotiable)).

### Proving sits downstream of signing

The `prover` crate is strictly downstream of the signing boundary: it receives an
**already-signed** transaction and produces a proof. It holds no key material —
the native SNIP-36 backend even passes a dummy `0x1` private key to satisfy the
upstream CLI's config check, and `preflight` rejects unsigned transactions. The
`companion_prove*` methods are folded onto the same authenticated loopback
service (pairing + transport guard + request log) rather than a separate open
port. The prover's per-network settings carry a remote-prover **API key**, so —
like the wallet's own settings — they are reachable over the desktop's trusted
IPC only, never the loopback service.

```
  wallet-core ──signs──► signed tx ──► prover (no keys) ──► proof ──► broadcast
```

This wallet/prover split mirrors the two repos it came from: strkd signs but
never proved; `../dinner` proved but never signs. Merged, the same device can
sign → prove → broadcast a SNIP-36 proof-carrying invoke without the secret ever
leaving it.

`companion_signAndProve` wires the key-holding half into one call: it signs the
private virtual transaction ("Tx A") and hands it straight to the in-process
prover. SNIP-36 is inherently two transactions, though — the on-chain verifier
invoke ("Tx B", e.g. `verify_result(public_message)`) is broadcast separately via
`wallet_addInvokeTransaction { proof_facts, proof, submit:true }`. strkd doesn't
assemble Tx B: its calldata is decoded from the prover's L2→L1 message and is
application-specific, so a generic wallet can't build it.

## Dependency notes

- **`krusty-kms`** (git, pinned `1e36829`) — all derivation/signing/address/
  tx-hash cryptography. **Experimental; not for production.** Bumps go through
  review.
- **`krusty-kms-common`** — shared types (`ChainId`, `KmsError`).
- **`argon2` + `aes-gcm`** — vault KDF + AEAD. Chosen over krusty's internal
  cipher to keep the at-rest format mainstream and auditable.
- **`starknet-types-core`** — pinned to the same `Felt` version as krusty to
  avoid type skew.
