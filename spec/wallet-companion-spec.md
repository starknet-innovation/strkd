# Starknet Wallet Companion — Technical Spec (v1 / Minimal)

**Status:** Draft for review
**Owner:** henri@starknet.org
**Date:** 2026-06-09
**Scope:** Minimal first cut of a desktop menu-bar companion wallet for Starknet.

> ⚠️ **Security review required.** This document describes a key-custody security model. Every security claim here (KDF choice, transport isolation, threat boundaries, derivation/grinding) must be independently reviewed/audited before any use with real funds. `krusty-kms` itself is flagged **experimental / not for production** by its authors — see [Risks](#14-risks--open-questions).

---

## 1. Overview

A desktop **companion wallet** that lives in the macOS menu bar. It safely stores a Starknet seed, derives multiple accounts from it, and exposes a **local service** that other programs on the same machine — AI agents, desktop apps — can call to request wallet operations. The wallet never returns private keys; it returns signed payloads, and can optionally **broadcast** transactions on the caller's behalf. Every signing or state-changing request triggers a **menu-bar confirmation prompt**, and all requests are logged and viewable in the app.

The service speaks the **standard Starknet Wallet RPC API** ([`wallet_rpc.json`](https://github.com/starkware-libs/starknet-specs/blob/master/wallet-api/wallet_rpc.json)) so callers use a known contract, plus a small **companion extension namespace** for things the standard doesn't cover (pairing, agent account creation).

### 1.1 Goals
- Store a seed and derived keys safely at rest and in memory.
- Use `krusty-kms` for all core cryptography (derivation, signing, address calc, tx-hash).
- Derive **multiple accounts from one seed**, in two segregated domains (user vs agent).
- Expose a local JSON-RPC service implementing the wallet RPC spec.
- Never leak private keys — return signed payloads; **broadcast on request**.
- Pair callers once; confirm every sign/state-change with a menu-bar prompt.
- Log and display all requests.

### 1.2 Non-goals (v1)
- ❌ Hardware-wallet / external-signer support.
- ❌ Multi-device vault sync (vault stays local).
- ❌ Key export / seed re-reveal UI after setup.
- ❌ Browser-dApp (`get-starknet`) injection — callers are local native processes only.
- ❌ Transaction **simulation / effects preview** in the prompt (decode + fee only; see [§8](#8-confirmation-ux)). Note: fee *estimation* is in scope; full simulation is not.
- ✅ Auto-approval via **time-bounded permission grants** (see §5.8). (Per-call
  spending *limits* — max-fee/allowlist caps — remain out of scope.)

---

## 2. Key Decisions (resolved in interview + research)

| Area | Decision |
|---|---|
| App framework | **Tauri** (Rust core), `krusty-kms` crates linked in-process |
| Key storage at rest | **Encrypted vault file + passphrase** (Argon2id → AES-256-GCM) |
| Lock behavior | **Auto-lock after inactivity** (`auto_lock_minutes`, default 15; 0 = never). Any RPC request or unlock resets the timer (so a busy granted agent stays unlocked); status-polling does not — see [§5.3](#53-lock--unlock) |
| Service transport | **Localhost HTTP + JSON-RPC 2.0**, bound to `127.0.0.1` |
| Caller auth | **Pairing + per-request approval** (token per client; reads auto-served once paired) |
| RPC method scope | **Full `wallet_rpc.json`** + companion extensions, delivered in phases |
| Submit vs sign | **Both.** Default = **sign-only** (return payload + computed hash); caller opts in with `submit: true` to broadcast |
| Node & fees | When submitting (or estimating), wallet uses a **configured RPC endpoint per network** and **auto-estimates** fees |
| Account contract | **OpenZeppelin** accounts (counterfactual address from derived pubkey) |
| Networks | **Sepolia + Mainnet**. Agents pick the chain **per request** (optional `chainId` on the operational methods) — explicit and race-free across concurrent clients. `wallet_switchStarknetChain` is **deprecated for agents** (it mutates one shared default for all clients); it remains for EIP-1193 compatibility and as the omitted-`chainId` fallback, which the human sets in Settings |
| Confirmation scope | Prompt on **signing / state-changing** methods; read-only auto-served to paired callers |
| Tx display | **Decode calls + max fee + network + caller**; no full simulation |
| Logging | **Persist full payloads** by default (debugging), toggle to disable later; **never** log key material |
| Seed onboarding | **Generate new** BIP-39 mnemonic **or import** existing 12/24 words |
| Account model | **Two derivation domains** — user (portable path) + agent (separate branch) |
| Account scoping | **Agent callers see only their own agent-domain accounts**; human/app callers see user accounts |
| Agent account creation | Prompt by default; **auto-approved under an active grant** |
| Agent signing | Prompt by default; **auto-approved under an active grant** (funding still always prompts) |
| Permission grants | Per-agent, **time-bounded (≤ 90 days) and revocable**; see [§5.8](#58-permission-grants) |

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│  Companion Wallet (single Tauri process, macOS menu-bar app)           │
│                                                                        │
│  ┌────────────────┐   ┌───────────────────────────────────────────┐   │
│  │  Tray / Menu    │   │  Frontend (web UI, Tauri webview)          │   │
│  │  bar UI         │◀─▶│  • Onboarding (gen/import seed)            │   │
│  │  • status       │   │  • Account manager                         │   │
│  │  • confirm popup│   │  • Confirmation dialogs                     │   │
│  │  • open log     │   │  • Request log viewer                       │   │
│  └────────┬────────┘   └───────────────────┬───────────────────────┘   │
│           │  Tauri IPC (commands/events)    │                           │
│  ┌────────▼─────────────────────────────────▼──────────────────────┐   │
│  │  Rust core                                                       │   │
│  │  ┌────────────┐ ┌──────────────┐ ┌────────────────────────────┐ │   │
│  │  │ RPC server │ │ Approval      │ │ Vault manager              │ │   │
│  │  │ (JSON-RPC  │▶│ broker        │▶│ (lock/unlock,              │ │   │
│  │  │  /127.0.0.1)│ │ (prompt+wait) │ │  Argon2id+AES-GCM)         │ │   │
│  │  └─────┬──────┘ └──────────────┘ └──────────┬─────────────────┘ │   │
│  │        │                                     │ (in-mem seed     │   │
│  │  ┌─────▼──────┐ ┌───────────────┐ ┌──────────▼──────────────┐   │   │
│  │  │ Pairing /  │ │ Request log   │ │  krusty-kms (derive/sign/ │   │   │
│  │  │ auth store │ │ (SQLite)      │ │  addr/tx-hash)            │   │   │
│  │  └────────────┘ └───────────────┘ └───────────────────────────┘ │   │
│  │  ┌─────────────────────────────────────────────────────────────┐│   │
│  │  │ Starknet RPC client  (fee estimate + optional broadcast)     ││   │
│  │  └──────────────────────────────┬──────────────────────────────┘│   │
│  └─────────────────────────────────┼───────────────────────────────┘   │
└───────────▲──────────────────────── │ ─────────────────────────────────┘
            │ loopback HTTP (callers)  │ outbound HTTPS (Starknet node)
   ┌────────┴─────────┬──────────────┐ ▼
   │ AI agent (MCP)   │ Desktop app   │   Sepolia / Mainnet RPC endpoint
   └──────────────────┴───────────────┘
```

### 3.1 Components
- **Tray / Menu-bar UI** — persistent status, entry point for confirmation popups and the log viewer.
- **Frontend (webview)** — onboarding, account management, confirmation dialogs, log viewer. Renders only; holds no secrets.
- **RPC server** — async HTTP server bound to loopback; parses JSON-RPC 2.0; authenticates the caller token; routes to handlers.
- **Approval broker** — for any method requiring confirmation, raises a UI prompt and **blocks the request** until the user approves/rejects or it times out.
- **Vault manager** — owns the encrypted vault; handles unlock/lock, KDF, in-memory seed lifecycle, zeroization.
- **Pairing / auth store** — registered callers, their tokens, and their account scope.
- **Request log** — append-only SQLite store of every request and its outcome.
- **krusty-kms** — all cryptography. No keys leave this boundary except as signatures/public data.
- **Starknet RPC client** — outbound connection to a configured node per network; used for **fee estimation** and, when `submit: true`, **broadcasting**. (`krusty-kms-client` is the RPC-facing crate; `starknet-rs` is an alternative.)

---

## 4. Tech Stack & Dependencies

| Concern | Choice | Notes |
|---|---|---|
| Shell / packaging | Tauri 2.x | Native macOS tray; small binary; webview frontend |
| Core language | Rust | Same language as `krusty-kms` → keys never cross an FFI/language boundary |
| Crypto | `krusty-kms` (+ `krusty-kms-common`) | BIP-39/44 derivation, STARK signing, OZ address calc, invoke-V3 hash |
| Tongo / STRK20 (Phase 3) | `krusty-kms-sdk` | Confidential proof generation |
| Node RPC | `krusty-kms-client` or `starknet-rs` | Fee estimation + optional broadcast |
| Frontend | Web (framework TBD — Svelte/React) | Renders prompts + log; no key access |
| Vault encryption | `argon2` (Argon2id) + AES-256-GCM (`aes-gcm`) | Authenticated encryption |
| Secret hygiene | `zeroize` | Wipe seed/keys from memory on lock |
| RPC server | `axum`/`hyper` (or Tauri-embedded) | Loopback only |
| Log store | SQLite (`rusqlite`/`sqlx`) | Local, queryable |

**Dependency on krusty-kms.** Not published to crates.io (workspace `v0.4.2`); depend via **git, pinned to an exact commit**:
```toml
[dependencies]
krusty-kms        = { git = "https://github.com/starknet-innovation/krusty-kms", rev = "<commit>" }
krusty-kms-common = { git = "https://github.com/starknet-innovation/krusty-kms", rev = "<commit>" }
# Phase 3 only:
# krusty-kms-sdk  = { git = "https://github.com/starknet-innovation/krusty-kms", rev = "<commit>" }
```
It is **pure Rust** — no FFI needed for an in-process Rust wallet. It is **experimental**; vendor if practical and gate updates behind review.

---

## 5. Security Model

### 5.1 Storage at rest
- Seed is stored as an **encrypted vault file** (e.g. `~/Library/Application Support/strk-companion/vault.bin`).
- Encryption key derived from a **user passphrase** via **Argon2id** (tuned params; salt + KDF params in the vault header).
- Payload encrypted with **AES-256-GCM** (random nonce per write; AEAD tag verified on read).
- Vault contains: BIP-39 mnemonic/entropy, account index registry (per domain), labels, settings. **No plaintext keys ever touch disk.**

### 5.2 In memory
- On unlock, hold the **mnemonic/seed only in memory**, in `zeroize`-wrapped buffers.
- Per-account private keys are derived **on demand** for a signing operation and zeroized immediately after.
- On lock (manual, inactivity timeout, or app quit) → zeroize the seed and all derived keys.

### 5.3 Lock / unlock
- **Auto-lock** after a configurable inactivity window (`auto_lock_minutes`,
  default 15; **0 = never**). Implemented as a background task in the desktop that
  locks the session once it's been idle past the window.
- **Activity = any RPC request, or an unlock** (`ServerState::touch_activity`,
  called in `dispatch`). The desktop's own status-polling is IPC, not RPC, so it
  does **not** keep the wallet alive. *(Revised from the original "only user
  interaction" rule: permission grants now let agents operate unattended, so a
  busy granted agent's requests must keep the session unlocked — otherwise it'd
  get locked out mid-run. A truly idle wallet — no user, no agent — still locks.
  For fully-unattended agents, set `auto_lock_minutes = 0`.)*
- Locked state: signing/state-changing methods surface an unlock prompt
  (interactive) or return `-32001` (locked).

### 5.4 Transport isolation
- Server binds **`127.0.0.1` only** (never `0.0.0.0`).
- **Caller discovery:** ephemeral port written to a `port.lock` file (port + nonce) in the data dir; callers read it. (Resolves open Q#4.)
- **Anti-DNS-rebinding / anti-CSRF:** reject requests with a browser-like `Origin`/`Referer`; require a custom header (`X-Companion-Client`); validate `Host`. Require the Bearer token on every call.

### 5.5 Caller authentication (pairing)
- A new caller calls `companion_requestPairing` with metadata (display name, purpose, requested kind: `agent` | `app`). The wallet shows an **approval prompt**; on approval it mints a **per-client token** and stores `{client_id, label, kind, token_hash, scope, created_at}`.
- Every subsequent request carries `Authorization: Bearer <token>`. Unknown/invalid → `NOT_REGISTERED` (118).
- **Scope:** an `agent` client is scoped to its own agent-domain accounts; an `app`/user client sees user accounts (see [§6.3](#63-creation-flows--account-scoping)).
- Tokens are revocable from the UI. Each token maps to one client identity in the log.

> **Known limitation (documented):** loopback HTTP is reachable by any local process, and a hostile local process could read another app's token. v1 mitigates with per-client tokens + per-request prompts on anything sensitive. Future hardening: a **Unix domain socket with peer-credential + code-signature verification**. Flagged in [Risks](#14-risks--open-questions).

### 5.6 What never leaves the boundary
Private keys, seed, mnemonic, passphrase, Argon2 output. The service returns only: public keys, addresses, chain metadata, signatures, signed payloads, and transaction hashes.

### 5.7 Threat model (summary)
| Threat | Mitigation (v1) | Residual / future |
|---|---|---|
| Malware reads vault file | Encrypted at rest; passphrase-derived key | OS keychain wrap (future) |
| Malware reads keys from memory | Zeroize on lock; short auto-lock (user-activity only) | Re-prompt-per-sign mode (future) |
| Rogue local process calls service | Pairing + token + per-sign prompt + scope | UDS + peer-cred/codesign (future) |
| Browser DNS-rebinding to loopback | Origin/Host checks, custom header, token | — |
| User approves a malicious tx | Decoded call + fee + caller in prompt | Simulation (future) |
| Token theft | Per-client tokens, revocable, scoped, logged | Short-lived/rotating tokens (future) |

### 5.8 Permission grants (auto-approval)

By default every signing / state-changing request prompts. The user may instead
grant a **paired agent** a **time-bounded auto-approval window** so it can operate
without per-request prompts:

- **What it covers:** the agent's *own-account* operations — `signTypedData`,
  `addInvokeTransaction`, `addDeclareTransaction`, `deployAccount`,
  `createAgentAccount`. While a grant is active these skip the menu-bar prompt.
- **What it never covers:** **`requestFunding`** (it spends the user's *manager*
  account, so it always prompts), **pairing** (bootstraps a new client), and
  wallet-config changes (`switchStarknetChain`, `watchAsset`).
- **Why it's safe:** an agent is independently *scoped* to the accounts it
  created, so a grant's blast radius is limited to funds the agent already
  controls — exactly the intent ("once funded, it can use that money freely").
- **Bounds:** per-agent, **time-bounded (clamped to ≤ 90 days / 3 months)** and
  **revocable** at any time from the desktop **Agents** panel. Grants (and
  pairings) are **persisted** to `clients.json` (`0600`; only token *hashes* are
  written), so they survive a restart.
- **Audit:** granted operations are still **logged** (Activity tab) — the grant
  removes the prompt, not the record.

---

## 6. Key & Account Model

### 6.1 Single seed, two derivation domains
All accounts derive from one BIP-39 seed via `krusty-kms`. krusty takes **structured args** (not a path string): `derive_keypair_with_coin_type(mnemonic, index, account_index, coin_type, passphrase)`, which builds `m/44'/{coin_type}'/{account_index}'/0/{index}` and performs the **EIP-2645 STARK-curve grind internally**.

Accounts are partitioned into two **non-overlapping branches**:

| Domain | Who creates | Path | krusty call | Rationale |
|---|---|---|---|---|
| **User** | Human, in-app | `m/44'/9004'/0'/0/i` | `account_index = 0`, `coin_type = STARKNET_COIN_TYPE (9004)`, vary `index = i` | Matches Argent's base path → **portable**: re-importing the mnemonic into Argent/Braavos surfaces the same accounts. |
| **Agent** | Agent, via RPC | `m/44'/9004'/0x41'/0/j` | `account_index = 0x41` (reserved "A"), vary `index = j` | A reserved hardened `account_index` fully isolates the keyspace; mainstream wallets only scan `account'=0'`, so agent accounts never collide with or appear in user wallets. |

- **Reserved constant:** `AGENT_ACCOUNT_INDEX = 0x41`. Document it as reserved-for-agents; never reuse it for user accounts.
- **Grinding/curve:** EIP-2645 SHA-256 rejection sampling to the STARK order, BIP-32 secp256k1 master (`HMAC-SHA512("Bitcoin seed", …)`) — all internal to krusty.

> **Portability is a claim that must be tested, not assumed.** Argent's recover tool routes the seed through `ethers` (`Wallet.fromMnemonic`); krusty uses standard BIP-39→BIP-32. Both should agree, but before asserting portability we **must round-trip**: derive a user account here, import the same mnemonic into Argent (and Braavos), and confirm identical addresses. Braavos's exact base was not verified from a primary source. See [Risks](#14-risks--open-questions).

### 6.2 Account contract
- Accounts are **OpenZeppelin** account contracts.
- Address computed **counterfactually**: `OpenZeppelinAccount::latest(chain_id).deployment_descriptor(&public_key, SaltPolicy::PublicKey)` → `OzDeploymentDescriptor { address, class_hash, salt, constructor_calldata, deployer_address }`.
- OZ **class hash is resolved from krusty's embedded per-network manifest** (`OzAccountClassConfig::latest(chain_id)`), not a hardcoded constant — so it tracks the manifest per network.
- Deployment data is exposed via `wallet_deploymentData`; deployment broadcasting follows the same submit/sign rules as any transaction ([§7.4](#74-broadcast-modes-sign-only-default-submit-opt-in)).

### 6.3 Creation flows & account scoping
- **User account (in-app):** "Add account" derives the next free `i` in the user branch, optional label, sets active. No RPC needed.
- **Agent account (RPC):** `companion_createAgentAccount` derives the next free `j` in the agent branch. **Every creation prompts** the user, is logged, and surfaces in the UI. Label defaults to the requesting client's name.
- **Scoping (resolves open Q#2):** `wallet_requestAccounts` / `companion_listAccounts` return only the accounts in the **caller's scope** — an `agent` client sees only the agent-domain accounts it created; an `app`/user client sees user accounts. No caller sees another caller's agent accounts.

### 6.4 Seed onboarding
On first run, the user chooses:
- **Generate:** `generate_mnemonic` → display once (12/24 words) with a verification step → encrypt into the vault. Set passphrase.
- **Import:** paste an existing phrase → `validate_mnemonic` → encrypt into the vault. Set passphrase.

No re-reveal/export of the mnemonic after setup (non-goal).

---

## 7. Service API

### 7.1 Transport
- **JSON-RPC 2.0** over HTTP `POST /` on `127.0.0.1:<port>` (port from `port.lock`).
- Headers: `Content-Type: application/json`; `Authorization: Bearer <token>`; `X-Companion-Client: <client_id>`.
- Batch permitted for read-only methods; approval-gated methods handled one prompt at a time.
- **Discovery — `GET /`** (and `GET /usage`): a self-describing usage document
  (service summary, transport + headers, approval model, agent quickstart,
  method list, error codes). **Open** (no auth, no transport guard) — public
  usage info only, so an agent can discover the wallet before pairing. The
  desktop "Connect" panel shows a copy-paste prompt that points agents here.

### 7.2 Standard wallet RPC methods (full spec, phased)
Implements all of `wallet_rpc.json`. **P** = phase ([§13](#13-phasing--milestones)); **Confirm** = triggers a menu-bar prompt.

| Method | P | Confirm | Notes |
|---|---|---|---|
| `wallet_supportedWalletApi` | 1 | no | Supported API versions |
| `wallet_supportedSpecs` | 1 | no | Supported Starknet JSON-RPC spec versions |
| `wallet_getPermissions` | 1 | no | Returns granted permissions (`accounts`) |
| `wallet_requestAccounts` | 1 | no¹ | Scoped account addresses; `silent_mode` supported |
| `wallet_requestChainId` | 1 | no | Current chain id |
| `wallet_deploymentData` | 1 | no | OZ deployment requirements for an undeployed account |
| `wallet_signTypedData` | 1 | **yes** | Sign SNIP-12 typed data → `SIGNATURE` |
| `wallet_addInvokeTransaction` | 1/2 | **yes** | Sign-only (P1) → broadcast opt-in (P2); see [§7.4](#74-broadcast-modes-sign-only-default-submit-opt-in). Optional `proof_facts`/`proof` for **SNIP-36** proof-carrying invokes |
| `wallet_switchStarknetChain` | 2 | **yes** | Switch Sepolia ⇄ Mainnet |
| `wallet_addDeclareTransaction` | 2 | **yes** | Same submit/sign rules; `class_hash` **derived** from `contract_class` ([§7.4.1](#741-declare-the-class-hash-is-derived-not-trusted)); returns class hash + tx hash |
| `wallet_watchAsset` | 2 | **yes** | Add token to display |
| `wallet_addStarknetChain` | 2 | **yes** | Add a custom network |
| `wallet_strk20PrepareInvoke` | 3 | **yes**² | Build STRK20 (Tongo) call + proof, no submit |
| `wallet_strk20InvokeTransaction` | 3 | **yes** | STRK20 privacy action |
| `wallet_strk20Balances` | 3 | no | Query private balances |

¹ First connection requires pairing approval; thereafter auto-served to the paired caller (within scope).
² Proof generation may be heavy; prompt + progress indication.

### 7.3 Companion extension methods (`companion_*`)
Non-standard, namespaced to avoid clashing with `wallet_*`.

| Method | Confirm | Description |
|---|---|---|
| `companion_requestPairing` | **yes** | `{name, kind, reattach?}` → user approves → `{client_id, token}`. `reattach:true` re-issues a token for an existing same-name client (keeps its accounts/grant) |
| `companion_getStatus` | no | `{locked, network, api_version, grant}` — `grant: {active, expires_at}` when a token is presented, else null |
| `companion_createAgentAccount` | **yes** | Derive next agent-domain account → `{address, index, label}` |
| `companion_listAccounts` | no | Accounts in this caller's scope |
| `companion_fundingSource` | no | The manager/funding-source account address (so an agent can look up its nonce) |
| `companion_estimateFee` | no | Suggested `resource_bounds` + nonce from the node — opt-in fee help for sign-only callers (not for private SNIP-36 calldata) |
| `companion_requestGrant` | **yes (always)** | Agent asks the user for an auto-approval window (1–90 days); never auto-approved |
| `companion_requestFunding` | **yes** | Agent asks to be funded with STRK; user approves a transfer **from the manager account → the agent's own account** |
| `companion_deployAccount` | **yes** | Deploy one of the caller's own (counterfactual) accounts on-chain (DEPLOY_ACCOUNT v3); sign-only or `submit:true`; account must be funded first |

#### Agent funding (`companion_requestFunding`)

A recurring operational pain: agents need STRK to pay fees, so a human manually
sends them funds. This method lets an agent **request** funding and the user
approve it with one menu-bar click:

- The agent calls `companion_requestFunding { amount, recipient?, token?, funding_source_index?, nonce, resource_bounds }`.
  `amount` is in **fri** (STRK's smallest unit). `recipient` must be one of the
  **caller's own** accounts (defaults to its first); the agent can only pull
  funds **into its own scope**, never elsewhere.
- The **sender is the manager account**, resolved by the wallet — *not* chosen
  by the agent. Default = the user-domain **root** account (`m/44'/9004'/0'/0/0`),
  overridable via `funding_source_index`. (A persistent "marked as manager"
  setting is a small follow-up; the default is the root.)
- The wallet builds a STRK `transfer(recipient, amount)` from the manager, shows
  an approval prompt summarizing recipient / amount / manager / network, and on
  approval **signs** it. The manager pays the fee.
- **Sign-only this phase:** returns the signed transfer (`submitted:false`); the
  agent broadcasts it (keyless — the signature authorizes it; no agent funds
  needed to receive). Because the manager's nonce/fee can't be fetched without a
  node yet, the caller supplies `nonce` + `resource_bounds` (use
  `companion_fundingSource` to discover the manager address, then query its
  nonce). **Phase 2** adds auto-nonce/estimation + `submit:true` to make this
  fully turnkey.
- **Security:** the agent never names the sender, so it cannot drain an
  arbitrary account; it can only request funds *into* accounts it owns, and every
  request needs human approval.

### 7.4 Broadcast modes (sign-only default, submit opt-in)
The wallet supports **both** signing and broadcasting. Transaction methods accept an optional `submit` flag:

- **`submit: false` (default) — sign-only / return payload:**
  1. Build the V3 transaction fields.
  2. Determine **resource bounds**: wallet **auto-estimates** via the configured node (`starknet_estimateFee`). The bounds used are returned in the response (so the returned hash/signature are valid for exactly those bounds).
  3. Compute the deterministic hash via `compute_invoke_v3_hash(...)` (independent of the signature).
  4. Sign with the derived key.
  5. Return `{ transaction_hash, signature, signed_transaction, resource_bounds, submitted: false }`. The caller broadcasts.
- **`submit: true` — broadcast:**
  - Same as above, then the wallet **broadcasts** via the configured node and returns `{ transaction_hash, submitted: true }`. This is the standard `wallet_rpc.json` semantics.

**Documented default deviation:** the standard implies `addInvokeTransaction` *submits*. Here the **default is sign-only** (per requirements: return payloads); spec-exact submission is one flag away (`submit: true`). The `submitted` field always tells the caller which happened.

> Fee policy: wallet **auto-estimates** by default ([decision §2](#2-key-decisions-resolved-in-interview--research)). The estimated fee/resource bounds are shown in the confirmation prompt. (Caller-supplied bound override is a possible future nicety, out of scope for v1.)

**SNIP-36 proof-carrying invokes:** `wallet_addInvokeTransaction` accepts optional
`proof_facts` (felt[]) and `proof` (base64 STWO string). When `proof_facts` is
present the V3 hash is extended with `Poseidon(proof_facts)` (krusty's
`compute_invoke_v3_hash_with_proof_facts`) so the signature covers them — required
at sign time. On `submit:true` the `proof` rides along on broadcast (required
then); sign-only echoes both back so the caller can assemble the broadcast.
Absent ⇒ a standard invoke (identical hash). The loopback service accepts large
bodies (proofs are multi-MB).

**Implementation status (Phase 2):** broadcast + auto nonce/fee are implemented
behind the injectable per-network `StarknetRpc` node seam (`wallet-rpc::node`),
configured from the desktop **Settings** panel (RPC URL per network →
`config.json`; applied at runtime). When **no node** is configured for the active
chain the methods are sign-only and require caller-supplied `nonce` +
`resource_bounds` (else error `-32005`). The wire format is **live-verified**
against a Sepolia v0.10 node for nonce/deploy-status/estimate (the broadcast hop
still needs a funded-account submit to confirm).
`wallet_switchStarknetChain` (Sepolia ⇄ Mainnet, switching the active node too)
`wallet_watchAsset`, and `wallet_addDeclareTransaction` (§7.4.1) are
implemented. Still pending: `addStarknetChain` (needs a generalized `ChainId`).

#### 7.4.1 Declare: the class hash is derived, not trusted

A node does not take the caller's word for a declare's `class_hash`. It
**recomputes** it from the `contract_class` in the broadcast, and that value
goes into the transaction hash the account's `__validate_declare__` checks the
signature against. So a wallet that signs a caller-supplied `class_hash` which
does not match the class actually broadcast emits a signature the node rejects
— surfacing as `Account: invalid signature` (RPC 55, or execution error 41)
with nothing wrong in the wallet's own transaction hashing (strkd #9).

The wallet therefore derives the class hash itself, from the exact class it
broadcasts (`wallet_core::SierraClass::class_hash`, the `CONTRACT_CLASS_V0.1.0`
Poseidon layout):

- `contract_class` given → `class_hash` is **derived**, and the parameter is
  optional. Accepts scarb's `*.contract_class.json` verbatim (ABI as an array,
  debug info ignored) as well as the RPC `CONTRACT_CLASS` object.
- Both given → cross-checked; a mismatch is **`114`** naming both hashes,
  rather than a signature that fails on-chain.
- `class_hash` alone → signed on trust (offline/hash-only flows): without the
  class there is nothing to check it against.

The ABI is hashed as the **exact string** the class carries, byte for byte, so
the same ABI serialised two ways is two different classes. A string ABI is
never re-serialised; an ABI supplied as an array is serialised the way the
compiler hashes it (Python `json.dumps` separators, key order preserved).

Sign-only returns a **complete** `BROADCASTED_DECLARE_TXN_V3` in
`signed_transaction` — every field the RPC requires, signature and (when
supplied) class included — so it can be POSTed as `declare_transaction`
unchanged, matching what `addInvokeTransaction` already returns.

### 7.5 Error codes
Use the spec's codes verbatim:

`111` NOT_ERC20 · `112` UNLISTED_NETWORK · `113` USER_REFUSED_OP · `114` INVALID_REQUEST_PAYLOAD · `115` ACCOUNT_ALREADY_DEPLOYED · `116` DEPLOYMENT_DATA_NOT_AVAILABLE · `117` CHAIN_ID_NOT_SUPPORTED · `118` NOT_REGISTERED · `119` INSUFFICIENT_PRIVATE_BALANCE · `120` PRIVACY_LEAK · `162` API_VERSION_NOT_SUPPORTED · `163` UNKNOWN_ERROR.

- User rejects / prompt times out → **`113` USER_REFUSED_OP**.
- Unpaired/invalid token → **`118` NOT_REGISTERED**.
- Locked + `silent_mode` → error; locked + interactive → unlock UI then proceed.
- Node unreachable during estimate/submit → `163` with a clear message (don't sign over guessed bounds).

### 7.6 Example exchange

Request (paired agent, sign-only default):
```json
{
  "jsonrpc": "2.0", "id": 7,
  "method": "wallet_addInvokeTransaction",
  "params": {
    "calls": [{
      "contract_address": "0x049d36...",
      "entry_point_selector": "transfer",
      "calldata": ["0x0123...", "0x2710", "0x0"]
    }]
  }
}
```
→ wallet estimates fee, pops a confirmation showing decoded call + fee + network + caller → user approves →
```json
{
  "jsonrpc": "2.0", "id": 7,
  "result": {
    "transaction_hash": "0x06f2...",
    "signature": ["0x...", "0x..."],
    "signed_transaction": { "...": "..." },
    "resource_bounds": { "l1_gas": {...}, "l2_gas": {...}, "l1_data_gas": {...} },
    "submitted": false
  }
}
```
With `"submit": true` in params, the result is `{ "transaction_hash": "0x06f2...", "submitted": true }`.

---

## 8. Confirmation UX

- **When:** every signing / state-changing method (Confirm column in [§7.2](#72-standard-wallet-rpc-methods-full-spec-phased)) and every `companion_createAgentAccount` / `companion_requestPairing`.
- **What's shown:**
  - Requesting **caller** (paired client name + id).
  - **Method** and a human summary; whether it will **submit** or just sign.
  - For transactions: **decoded calls** (target contract, entrypoint/selector, calldata decoded where ABI is known), **estimated max fee**, **network**.
  - For typed-data signing: the domain + message being signed.
  - Approve / Reject; reject → `113`.
- **Timeout:** prompts auto-reject after a configurable window (e.g. 60 s) → `113`.
- **Presentation:** raised from the menu bar; one prompt at a time (queue if multiple arrive).
- **No full simulation in v1** (decode + estimated fee only).

---

## 9. Logging & Audit

- **Store:** append-only **SQLite** table.
- **Captured per request:** timestamp, caller (`client_id` + label), method, network, **full request params**, decision (approved/rejected/auto/timeout), submit-vs-sign, result summary, **full signed payload** (default on), tx hash, error code if any, latency.
- **Default = persist full payloads** for easier debugging; a setting disables full-payload capture later (then store only a redacted summary).
- **Never logged, under any setting:** seed, mnemonic, private keys, passphrase, Argon2 output, raw tokens (store token **hashes** only).
- **UI:** filterable/searchable log viewer (by caller, method, decision, date); export.

> Full signed payloads + calldata at rest reveal account activity — treat the data dir as **AMBER** sensitivity; ship the disable-toggle.

---

## 10. On-disk Layout

```
~/Library/Application Support/strk-companion/
  vault.bin          # encrypted mnemonic + account registry (Argon2id + AES-256-GCM)
  config.json        # network, node RPC URLs, auto-lock timeout, log-full-payloads flag
  clients.json       # paired clients: id, label, kind, token_hash (hex), granted_until (0600)
  requests.db        # request/audit log (SQLite)
  port.lock          # current loopback port + nonce for caller discovery
```
- `vault.bin` is the only file holding secret-derived material, and only encrypted. File perms restricted to the user (`0600`).

---

## 11. Failure Modes & Handling

| Failure | Behavior |
|---|---|
| Wrong passphrase | AEAD verification fails → "incorrect passphrase"; rate-limit attempts |
| Vault corrupt / tampered | GCM tag mismatch → refuse to load; guide to restore from mnemonic |
| Service port in use | Pick another ephemeral port; rewrite `port.lock` |
| Caller token invalid | `118 NOT_REGISTERED` |
| Caller out of scope | Return only in-scope accounts; deny cross-scope ops |
| User rejects / prompt times out | `113 USER_REFUSED_OP` |
| Locked during request | Interactive: unlock UI then continue; `silent_mode`: error |
| Node unreachable (estimate/submit) | `163` with message; do **not** sign over guessed bounds |
| `krusty-kms` signing error | `163 UNKNOWN_ERROR` + logged detail; never expose key material |
| Unsupported network/chain | `117 CHAIN_ID_NOT_SUPPORTED` / `112 UNLISTED_NETWORK` |
| Malformed JSON-RPC | Standard parse/invalid-request errors; `114` for bad payload semantics |

---

## 12. Testing & Validation

- **Crypto correctness:** known-answer tests for derivation (both domains), OZ address calc, and invoke-V3 hash against `krusty-kms` references; verify signatures validate on-chain (Sepolia) for an OZ account.
- **Portability round-trip (must pass before claiming it):** derive a user account here → import the same mnemonic into **Argent** and **Braavos** → assert identical addresses. Confirm **agent** branch (`account_index = 0x41`) never appears in those wallets.
- **Domain isolation:** prove user (`0'`) and agent (`0x41'`) branches never collide.
- **Vault:** round-trip encrypt/decrypt; tamper detection; wrong-passphrase handling; scan to confirm no plaintext key material on disk.
- **Memory hygiene:** assert seed/keys zeroized on lock (where testable).
- **Service contract:** conformance tests against `wallet_rpc.json` per method; error-code coverage; `silent_mode` paths; sign-only vs submit results.
- **Auth & scope:** unpaired/revoked token rejected; agent caller can't see user or other agents' accounts; Origin/Host/rebinding checks.
- **Fee/broadcast:** estimate path returns sane bounds; node-unreachable handled without signing over guesses; submit returns a hash that lands on Sepolia.
- **Approval flow:** approve/reject/timeout map correctly; queueing under concurrent requests; auto-lock not reset by agent traffic.
- **Logging:** redaction guarantees (no secrets ever); full-payload toggle behavior.
- **E2E (Sepolia):** agent pairs → creates agent account → requests invoke (both sign-only and submit) → user approves → tx lands.

---

## 13. Phasing & Milestones

Full spec is the target; deliver in phases.

- **Phase 0 — Skeleton:** Tauri menu-bar app, vault (gen/import + passphrase + lock/unlock), single user account, `krusty-kms` wired in (`generate_mnemonic`, `derive_keypair_with_coin_type`, OZ address). No service.
- **Phase 1 — Core service (minimal usable wallet):** loopback JSON-RPC + pairing + per-request approval + log; methods `supportedWalletApi/Specs`, `getPermissions`, `requestAccounts`, `requestChainId`, `deploymentData`, `signTypedData`, `addInvokeTransaction` **sign-only** with node-backed **fee estimation**. Multi-account (user domain) + agent domain + `companion_createAgentAccount` + scoping.
- **Phase 2 — Broadcast & remaining standard methods:** `submit: true` broadcasting via configured node; `switchStarknetChain` (Sepolia ⇄ Mainnet); `addDeclareTransaction`, `watchAsset`, `addStarknetChain`. Log viewer polish.
- **Phase 3 — Privacy (Tongo / STRK20):** `strk20PrepareInvoke`, `strk20InvokeTransaction`, `strk20Balances` via `krusty-kms-sdk`.
- **Hardening (parallel/after):** portability round-trip tests; UDS + peer-cred transport option; security audit; validate experimental crypto before Mainnet.

---

## 14. Risks & Open Questions

**Risks**
- 🔴 **`krusty-kms` is experimental / not for production.** Pin an exact commit (`v0.4.2`, no crates.io release, no confirmed tag), vendor if feasible, and **do not use Mainnet with real funds until the crypto path is independently audited.** Reconsider whether v1 should ship Mainnet at all vs. Sepolia-only until audit.
- 🔴 **Portability is unverified until tested.** User-branch portability to Argent is strongly indicated (same base path, same grind) but the BIP-39→seed step vs Argent's `ethers` path, and Braavos's exact base, are **not confirmed**. Block the portability claim on the round-trip test ([§12](#12-testing--validation)).
- 🟠 **Loopback HTTP is reachable by any local process.** Token theft by hostile local software is possible. Per-client tokens + per-sign prompts + scope mitigate; UDS + peer-cred/codesign is the planned hardening.
- 🟠 **Full-payload logging at rest** reveals activity (AMBER). Default-on for debugging; ensure the disable toggle ships and key material is never logged.
- 🟠 **Node connectivity** adds an outbound dependency and a trust point (the RPC endpoint can mis-estimate fees / mislead). Use reputable endpoints; show estimated fee in the prompt; never sign over guessed bounds when the node is unreachable.
- 🟠 **DNS-rebinding / CSRF** to the loopback port — mitigated by Origin/Host checks + custom header + token; must be tested.
- 🟡 **Mainnet + experimental code** is a deliberate risk per the network decision; revisit.

**Resolved (was open)**
- ✅ **Derivation paths** — user `m/44'/9004'/0'/0/i`; agent `m/44'/9004'/0x41'/0/j`; via `derive_keypair_with_coin_type`. (Portability still pending the round-trip test above.)
- ✅ **krusty-kms API surface** — mapped in [Appendix A](#appendix-a--method--krusty-kms-capability-map).
- ✅ **`wallet_requestAccounts` scoping** — agent callers scoped to their own accounts.
- ✅ **Auto-lock activity** — any RPC request or unlock resets the timer (busy granted agents stay unlocked); status-polling does not. Revised from "user-only" because grants enable unattended agents. See §5.3.
- ✅ **Port discovery** — `port.lock` file (port + nonce).

**Remaining open (confirm at implementation)**
1. **`StarkSignature` field names** and whether `compute_typed_data_message_hash` takes a structured SNIP-12 object vs pre-decomposed felts — verify against source at the pinned commit.
2. **Tongo SDK signatures** (`transfer`/`withdraw`/`rollover`/`ragequit`) — only `fund` was observed; confirm before Phase 3.
3. **OZ class-hash manifest** currency per network (`OzAccountClassConfig::latest`) — confirm it carries the class hashes you want on Sepolia + Mainnet.

---

## Appendix A — Method ↔ krusty-kms capability map

| Wallet need | krusty-kms API |
|---|---|
| New mnemonic | `generate_mnemonic()` |
| Import / validate mnemonic | `validate_mnemonic(...)` |
| Mnemonic → seed | `mnemonic_to_seed(mnemonic, passphrase)` |
| Derive account key (both domains) | `derive_keypair_with_coin_type(mnemonic, index, account_index, coin_type, passphrase)` → builds `m/44'/{coin}'/{account_index}'/0/{index}`, grinds internally |
| Public key | `stark_public_key(...)` |
| Sign message hash | `sign_stark_hash(priv, msg_hash) -> StarkSignature` |
| Sign typed data (SNIP-12) | `compute_typed_data_message_hash(...)` → `sign_stark_hash(...)` |
| OZ address (counterfactual) | `OpenZeppelinAccount::latest(chain_id).deployment_descriptor(&pubkey, SaltPolicy::PublicKey)` → `OzDeploymentDescriptor` |
| Invoke-V3 tx hash | `compute_invoke_v3_hash(sender, calldata, chain_id, nonce, account_deployment_data, tip, l1_gas, l2_gas, l1_data_gas, paymaster_data, nonce_da_mode, fee_da_mode)` |
| Invoke-V3 with proof (Tongo) | `compute_invoke_v3_hash_with_proof_facts(..., proof_facts)` |
| Tongo / STRK20 (Phase 3) | `krusty-kms-sdk` (`TongoAccount`, `FundParams`, …) |

Constants: `STARKNET_COIN_TYPE = 9004`, `TONGO_COIN_TYPE = 5454`. Types: `ResourceBounds { max_amount: u64, max_price_per_unit: u128 }`, `DaMode { L1, L2 }`, `SaltPolicy { PublicKey, Zero, Explicit(Felt) }`.

## Appendix B — References
- Wallet RPC spec: `https://github.com/starkware-libs/starknet-specs/blob/master/wallet-api/wallet_rpc.json`
- krusty-kms: `https://github.com/starknet-innovation/krusty-kms` (`crates/kms/src/{lib,derivation,tx_hash,account_class}.rs`, `crates/kms/examples/*`)
- Argent key derivation: `https://github.com/argentlabs/argent-starknet-recover/blob/main/keyDerivation.ts`
- EIP-2645 HD paths: `https://eips.ethereum.org/EIPS/eip-2645` · `https://book.starkli.rs/eip-2645-hd-paths`

---
*Internal technical spec — review before implementation. Security model and any crypto/compliance claims must be independently verified before use with real funds. Derivation/portability claims are unverified until the round-trip test passes.*
