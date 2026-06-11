# `wallet-rpc` — Reference

The local JSON-RPC service. It implements the read-only + signing slice of the
standard `wallet_*` API plus the `companion_*` extensions (spec §7), with
pairing-based caller authentication (§5.5) and a blocking approval broker (§8).

It holds **no key material**: all signing goes through
[`session::WalletSession`], which delegates to `wallet-core`. See the
[security boundary](./architecture.md#security-boundary-the-most-important-diagram).

- **Source:** `crates/wallet-rpc/src/`
- **Design rationale:** spec §7 (service API), §5.5 (pairing), §8 (confirmation)

## Module map

| Module | Purpose |
|---|---|
| `jsonrpc.rs` | JSON-RPC 2.0 `Request` / `Response` / error object types. |
| `error.rs` | `WalletRpcError` + mapping to standard `wallet_rpc.json` codes (and JSON-RPC reserved codes for transport/lock conditions). |
| `auth.rs` | `ClientStore`: pairing, per-client bearer tokens (only SHA-256 hashes stored), `ClientKind` (Agent/App), time-bounded auto-approval grants (`grant`/`revoke_grant`/`list`), **persisted** to `clients.json` (`open`/`persist`). |
| `approval.rs` | The `Approver` trait + `AutoApprover` (tests) and `ChannelApprover` (real UI). Carries only public info in a prompt. |
| `session.rs` | `WalletSession`: holds the unlocked mnemonic + vault passphrase + registry; performs deployment-data, typed-data signing, invoke signing, user/agent-account creation, and re-sealing. |
| `log.rs` | `RequestLog` — SQLite-backed (file or in-memory), with full-payload redaction toggle. |
| `store.rs` | `VaultStore` — atomic on-disk `vault.bin` load/save (temp + rename, `0600`). |
| `node.rs` | `StarknetRpc` trait + `HttpStarknetRpc` (reqwest JSON-RPC): nonce, fee estimate, broadcast, deploy-status, deploy-account. |
| `dispatch.rs` | `dispatch()` — transport-agnostic request routing, auth, scoping, approval, logging. |
| `server.rs` | Loopback axum HTTP transport + `port.lock` discovery + transport hardening + `GET /` usage doc. |
| `usage.rs` | Self-describing usage document served at `GET /` / `GET /usage`. |

## Discovery: `GET /` — the complete agent contract

`GET /` (and `GET /usage`) return a self-describing JSON usage document
(`usage.rs`) that is the **full agent-facing contract** — an agent should need no
other docs. It covers: service identity/versions, transport + required headers +
what's rejected, a `concepts` block (accounts/scoping, networks, funding,
**permission grants**), the `approval_model` and `submit_model`, a step-by-step
`quickstart`, the **complete method catalogue** (each with auth/prompts/params/
returns/notes, plus the Call shape + accepted aliases), a `deferred` list, and
**every error code**. It also carries an **`alpha_notice`**: a disclaimer that
strkd is alpha, plus a call-to-action telling agents **not to work around
limitations** but to emit a structured `=== STRKD-FEEDBACK ===` block (goal /
attempted / observed / limitation / needed / impact / workaround-avoided /
context) for their operator to relay to the maintainer — so gaps surface as
actionable reports instead of silent hacks. It's **open** (no auth, no transport
guard) — public info only — so a client can discover it before pairing.
`usage.rs` is the single source of truth, and `tests/server.rs` asserts the doc
lists every implemented method + the core sections (incl. `alpha_notice` and its
feedback sentinel), so it can't silently drift from the dispatcher. The
desktop "Connect" tab shows a copy-paste prompt that points agents here (see
[`desktop.md`](./desktop.md)).

## Implemented methods (this phase)

| Method | Auth | Prompts | Notes |
|---|---|---|---|
| `wallet_supportedWalletApi` | public | no | API versions |
| `wallet_supportedSpecs` | public | no | Spec versions (placeholder until confirmed against node) |
| `wallet_getPermissions` | public | no | `["accounts"]` if paired, else `[]` |
| `companion_getStatus` | public | no | `{locked, network, api_version}` |
| `companion_requestPairing` | public | **yes** | `{name, kind}` → `{client_id, token}` |
| `wallet_requestChainId` | paired | no | chain id felt |
| `wallet_requestAccounts` | paired | no | scoped addresses |
| `companion_listAccounts` | paired | no | scoped accounts (detailed) |
| `wallet_deploymentData` | paired | no | OZ counterfactual deploy data for first in-scope account |
| `wallet_signTypedData` | paired | **yes** | SNIP-12 sign → `[r, s]` |
| `wallet_addInvokeTransaction` | paired | **yes** | encode multicall, V3 hash, sign; `submit:true` broadcasts (needs node). With a node, nonce + fee auto-filled; else caller supplies them. Call = `{contract_address, entry_point_selector, calldata}` — selector accepts a **name or 0x**; aliases `contractAddress`/`to`, `entrypoint`/`entry_point`/`selector`. **SNIP-36:** optional `proof_facts` (extends the signed hash via `Poseidon(proof_facts)`) + `proof` (base64, required on broadcast) |
| `companion_deployAccount` | paired | **yes** | deploy one of the caller's own accounts (DEPLOY_ACCOUNT v3); sign-only or `submit:true`; account must be funded |
| `wallet_addDeclareTransaction` | paired | **yes** | declare a class; sign needs `class_hash` + `compiled_class_hash`; estimate/`submit:true` need the full `contract_class` |
| `wallet_switchStarknetChain` | paired | **yes** | switch active network (Sepolia ⇄ Mainnet); also switches the per-network node |
| `wallet_watchAsset` | paired | **yes** | add a token to the watch list (display-only, in-memory) |
| `companion_createAgentAccount` | paired (agent) | **yes** | derive next agent account |
| `companion_fundingSource` | paired | no | manager (funding-source) account address |
| `companion_estimateFee` | paired | no | suggested `resource_bounds` (canonical hex) + nonce from the node — opt-in fee help for sign-only callers |
| `companion_requestGrant` | paired | **yes (always)** | agent asks for an auto-approval window (1–90 days); always prompts (escalation is never auto-approved) |
| `companion_requestFunding` | paired (agent) | **yes** | sign a STRK transfer **manager → agent's own account** (sign-only) |

`wallet_addInvokeTransaction`: `entry_point_selector` may be a name or a `0x…`
selector. **With a node configured** (see [Node](#node-broadcast--fee-estimation)),
`nonce` and `resource_bounds` are optional (auto-fetched / estimated) and
`submit: true` broadcasts, returning the on-chain `transaction_hash`. **Without
a node**, it's sign-only: pass `nonce` + `resource_bounds`
(`l1_gas`/`l2_gas`/`l1_data_gas`, each `{max_amount, max_price_per_unit}`); omit
`submit`. The approval prompt shows the decoded calls, network, and fee.

Still **deferred** (return `-32601`): `addStarknetChain` (arbitrary custom chains
need a generalized `ChainId` beyond Sepolia/Mainnet) and the `strk20*` privacy
methods (Phase 3).

## Node (broadcast & fee estimation)

`node.rs` is the I/O seam for Starknet (Phase 2). `StarknetRpc` (trait) exposes
`get_nonce`, `estimate_invoke`, `add_invoke`; `HttpStarknetRpc` is the reqwest
JSON-RPC impl. Nodes are kept **per network** (`HashMap<ChainId, _>` behind an `RwLock`), so
`switchStarknetChain` also switches the RPC endpoint and a node configured only
for one chain is never reused on another. Attach at construction via
`with_node(chain, node)`, or **swap at runtime** via `set_node(chain, …)` (the
Settings panel registers both networks when the user saves). `has_node_for` /
`node_for` read them (the clone is never held across an await). When present,
`wallet_addInvokeTransaction` and `companion_requestFunding` auto-fetch the
nonce, estimate the fee (with a 1.5× margin), and broadcast on `submit:true`.
When absent, those calls are sign-only and require caller-supplied `nonce` +
`resource_bounds` (else `-32005 NoNode`).

`add_invoke` also carries optional **SNIP-36** `proof_facts` + `proof` on
broadcast (attached to the invoke tx JSON when present). The exact SNIP-36 wire
fields share the broadcast wire-format caveat (verify against a v0.10 node); the
hash extension itself is krusty's parity-tested `compute_invoke_v3_hash_with_proof_facts`.

The trait also covers **account deployment**: `is_deployed` (via
`getClassHashAt`), `estimate_deploy_account`, and `add_deploy_account`
(`starknet_addDeployAccountTransaction`). `wallet-core::sign_deploy_account_v3`
computes the DEPLOY_ACCOUNT v3 hash from the OZ deployment data and signs it; the
desktop's `deploy_account` command estimates → signs → broadcasts. The account
must be **pre-funded** (it pays its own deploy fee).

**Wire format — live-verified (2026-06-09)** against a Sepolia **v0.10** node
(`sepolia.nodes.starknet.org/rpc/v0_10`) using the real client: `getNonce`,
`getClassHashAt`/deploy-status, and `estimateFee` (3-resource fee model, V3 tx
JSON, DA modes) all work, as does the http→https redirect. Two bugs the live run
caught and fixed: the block tag (`pending` → `latest`; v0.10 dropped `pending`)
and 301-redirect handling (reqwest turns a 301 POST into a GET, so the client
re-POSTs to the `Location` once and caches it). The only path **not** yet
confirmed end-to-end is the actual **broadcast** (`add_invoke` /
`add_deploy_account`) — it needs a signed tx from a funded account, but it
submits the same tx object `estimateFee` already accepted, so only the submit
wrapper is unconfirmed. (Verify live with `cargo run -p wallet-rpc --example
node_probe`.)

## Key design points

- **`dispatch()` is the testable core.** It takes `(state, token, Request)` and
  returns a `Response` — no HTTP needed. `server.rs` is a thin axum wrapper.
- **Transport hardening (spec §5.4)** in `server.rs::transport_guard`, applied
  before dispatch: reject requests carrying `Origin`/`Referer`; require `Host`
  to be loopback (`127.0.0.1`/`localhost`/`[::1]`, defeats DNS-rebinding);
  require a non-empty `X-Companion-Client` header (forces browsers into a CORS
  preflight the server never answers). Failures return **HTTP 403** with JSON-RPC
  error `-32003`. The header value is *not* authenticated — the bearer token does
  auth; the header is purely the CSRF/rebinding tripwire.
- **Scoping (spec §6.3):** an `Agent` client is scoped to the accounts it owns
  (`Registry::scoped_for(Some(id))`); an `App` client sees user accounts
  (`scoped_for(None)`). A caller cannot sign with, or even see, an account
  outside its scope — enforced before the approval prompt (`Forbidden`).
- **Approval is blocking and lock-free across the await.** Account resolution
  takes a brief lock, drops it, then awaits the user's decision; only after
  approval is the session locked again to sign. This avoids holding the session
  mutex across user-think-time.
- **Tokens:** high-entropy bearer tokens; only the SHA-256 hash is stored. An
  unknown/invalid token → `118 NOT_REGISTERED`.
- **Permission grants (auto-approval):** a paired client may carry a
  `granted_until` expiry (`ClientStore::grant`/`revoke_grant`, capped to ≤90 days
  by the desktop). `gated_approval` auto-approves the client's **own-account**
  ops (sign/invoke/declare/deploy/create) while the grant is active —
  `companion_requestFunding`, pairing, and wallet-config calls bypass it and
  always prompt. Safe because the client is independently scoped to its own
  accounts. Auto-approved ops are still logged. **Pairings + grants persist** to
  `clients.json` (`ClientStore::open`/`persist`, `0600`, token *hashes* only), so
  they survive restarts. `companion_getStatus` echoes `grant: {active, expires_at}`
  when a token is presented, so a client can tell whether it will prompt.
- **Re-attach (`reattach:true` on pairing):** if a client with the same
  `(name, kind)` exists, `ClientStore::reattach` issues a fresh token keeping the
  existing `client_id` + grant + accounts (user-approved, with the account count
  shown) — so a re-paired agent isn't stranded from funds it created.
- **Sign-only returns a complete tx:** the sign-only response's
  `signed_transaction` is a full canonical-hex `INVOKE_TXN_V3` built by the shared
  `node::invoke_v3_tx_json` (same builder broadcast uses), so callers broadcast it
  as-is instead of hand-assembling tip/DA-modes/bounds.
- **Activity tracking for auto-lock:** `dispatch()` calls `touch_activity()` on
  every RPC request (so a busy agent keeps the session unlocked); the desktop's
  status-polling is IPC and doesn't count. The desktop runs the actual auto-lock
  timer (see [`desktop.md`](./desktop.md)) off `ServerState::last_activity_ms`.
- **Agent funding (`companion_requestFunding`):** an agent requests STRK; the
  wallet builds a `transfer` **from the manager account (user root by default,
  resolved by the wallet — never chosen by the agent) to one of the agent's own
  scoped accounts**, prompts, and signs. The manager pays the fee. Sign-only
  this phase (caller supplies the manager's nonce + bounds; `companion_fundingSource`
  reveals the manager address to look the nonce up). Because the approval broker
  is generic, this prompt surfaces in the desktop menu bar with **no desktop
  code change**. STRK token address is a constant (`STRK_TOKEN_ADDRESS`),
  overridable via `token` and flagged for network verification.
- **Durable registry changes (transactional):** the unlocked session retains the
  vault passphrase so it can `reseal()` itself. When a `VaultStore` is configured
  (`ServerState::with_vault_store`), `companion_createAgentAccount` re-seals and
  writes `vault.bin` **atomically** (temp + rename, `0600`). If the write fails,
  the in-memory add is **rolled back** (`rollback_account`) so memory and disk
  never diverge. With no store configured (e.g. tests), the change is in-memory
  only.
- **No secrets in logs/prompts:** `ApprovalRequest` carries only public info.
  The request log (`log.rs`) persists timestamp, method, caller label, network,
  decision, outcome, error code, and latency; with `full_payloads` on (default,
  debug) it also stores request params + result JSON (public data only). It
  **never** stores seed/key/passphrase/raw tokens — bearer tokens ride the
  header and only the resolved client label/id is logged. Redaction is
  centralized in `RequestLog::record`, and `set_full_payloads(false)` drops the
  params/result columns for subsequent rows (spec §9). The store is SQLite
  (file-backed in production via `ServerState::with_log`, in-memory by default /
  in tests); inserts are synchronous under the log mutex — fine at wallet
  request rates, movable to a writer task if throughput ever demands it.

## Known limitations (this phase)

- **Node wire format unverified** against a live node (see the Node caveat
  above). Broadcast/estimate orchestration is mock-tested only.
- **End-to-end invoke-hash acceptance is unproven** — see the caveat in the
  [`tx` module reference](./wallet-core.md#tx--invoke-transactions).
- **`addStarknetChain`** still returns `-32601` (see the deferred note above).
  `watchAsset`'s list is in-memory only (lost on restart). For declare, the node
  declare wire format (`contract_class` encoding) shares the same
  needs-live-verification caveat as broadcast.
- **BIP-39 passphrase assumed empty.** When supported it must be stored in the
  unlocked state and threaded into every derivation call.
- **No transport hardening yet** beyond loopback binding — Origin/Host checks
  and the custom-header requirement (spec §5.4) are a follow-up. UDS is future
  hardening.
- **`spec_versions` is a placeholder** until confirmed against the target node.

## Tests

| File | Covers |
|---|---|
| `tests/dispatch.rs` (50) | public vs authed methods, unpaired rejection (118), permissions reflect pairing, app/agent **scoping**, agent-account creation, app-can't-create-agent (forbidden), typed-data sign success/reject(113)/scope, invoke sign-only/scope/empty-calls(114), **submit without node (-32005), submit with mock node broadcasts, auto nonce+fee when omitted, runtime set/clear node, entrypoint-name alias**, funding (source/transfer/reject/forbidden/unowned/zero-amount) + **turnkey-with-node**, **deploy-account (sign-only / submit / no-node-errors)**, **declare (sign-only / submit / missing-class-hash-114)**, **SNIP-36 proof-carrying invoke (proof_facts extends+echoes hash / submit requires proof)**, **switchChain (success/unknown-117/reject-113), watchAsset**, **grant auto-approves own-account ops / does NOT auto-approve funding / revoke+expiry re-prompt**, **estimateFee returns canonical hex bounds / errors without node**, **getStatus reports grant state**, **reattach keeps a client's accounts**, **requestGrant approved→active / always prompts under an existing grant**, **dispatch records activity (auto-lock)**, deferred → not-implemented, locked blocks access, logging. |
| `server.rs` unit (5) | `is_loopback_host` parsing (IPv4/host/IPv6, with/without port); `transport_guard` accepts a well-formed native request and rejects Origin/Referer, non-loopback Host, and missing custom header. |
| `tests/server.rs` (3) | real loopback HTTP via reqwest: `port.lock` written with bound port, public call, unauth rejected (118), pair → bearer-authed call; **403 without `X-Companion-Client`; 403 when `Origin` present**. |
| `tests/log.rs` (5) | SQLite log: newest-first ordering, full-payloads on keeps params/result, off redacts them but keeps the summary, toggle applies to subsequent rows, file-backed persistence across reopen. |
| `tests/persistence.rs` (2) | agent-account creation re-seals + writes `vault.bin`; the account survives a reopen + unlock cycle; wrong passphrase can't open the persisted vault. |
| `tests/session.rs` (3) | `create_user_account` derives a user-domain account and grows the registry; fails when locked; `sign_deploy_account_for` produces a hash/signature + deployment fields matching the account. |
| `tests/clients.rs` (2) | pairings + grants survive reopen (token still verifies, grant still active); raw token never persisted (hash only); revoke persists. |
| `tests/server.rs` (+1) | `GET /` usage doc is open (no auth/headers), 200, self-describing (lists pairing + funding methods); `/usage` alias. |

Run them:
```bash
cargo test -p wallet-rpc
```
All tests use the public BIP-39 test vector; no real key material.

## Extending

- New method → add a branch in `dispatch::handle`, decide whether it needs auth
  and approval, enforce scope before prompting, and add a dispatch-level test.
- Anything that broadcasts or talks to a node belongs in the next phase and
  needs the Starknet RPC client wired in (spec §7.4).
