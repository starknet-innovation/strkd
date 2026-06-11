# Progress Log

**Append-only history of development.** Newest entry first. Never edit or delete
a past entry — if something was wrong, correct it in a new entry. The current
state is summarized in [`status.md`](./status.md); this file is the *why*.

Use the [entry template](#entry-template) at the bottom for every new entry.

---

## 2026-06-11 — Deploy/funding correctness + Accounts UX (balances, wiggle, pending) + user-action logging

Triggered by a maintainer deploy session: clicking Deploy too early failed with a
raw node "resources exceed balance (0)"; after funding, the first deploy
succeeded but the UI reverted to "Deploy", a second click hit "invalid nonce"
(nonce already 1). Also closes the agent-filed GitHub issue #1 (opaque
Contract-not-found when the manager isn't deployed).

**Did (core — `wallet-rpc`)**
- New `WalletRpcError::Precondition` (**-32006**): valid request, wrong on-chain
  state, with a human-actionable message.
- `companion_requestFunding`: **pre-check that the manager is deployed** on the
  active network → clear -32006 naming the manager + fix, instead of the node's
  opaque "Contract not found" (**fixes GitHub issue #1**).
- `companion_deployAccount`: **pre-check already-deployed** → clear -32006 instead
  of a confusing "invalid nonce" on a second deploy.
- Node trait gains `balance_of` (ERC-20 `balanceOf` via `starknet_call`).
- Tests: MockNode is now address-aware for `is_deployed` (+ `balance_of`); +2
  dispatch tests (manager-not-deployed, already-deployed). **98 tests green.**

**Did (desktop)**
- **Balance display** per account (`balance` IPC command; fri string → STRK,
  BigInt-formatted; works for undeployed accounts).
- **Deploy on a zero-balance account wiggles** the button + explains, instead of
  letting it fail on-chain.
- **Post-deploy is "pending"**: after broadcast the button shows "Deploying…" and
  **polls** until the node confirms — never reverts to "Deploy" → no double-submit
  (plus the same already-deployed guard mirrored into the `deploy_account`
  command). Fixes the maintainer's exact double-click bug.
- **Periodic refresh** of balance + deploy status every ~10 s (answers "how often
  does the RPC refresh?": on-demand + every 10 s, at `latest`).
- **User-initiated actions logged**: `add_user_account` + `deploy_account` now
  record to the request log (client "desktop (you)", decision "user"), so they
  show in the **Activity tab** alongside agent calls.

**Decisions**
- Pre-checks live in the dispatch handlers (one source of truth for agents) AND
  the desktop deploy command (its own node path). -32006 is a new custom code in
  our condition range (alongside -32001..-32005), kept out of the spec's 111–163.

**Not run-verified**
- Balance render, wiggle animation, pending-poll, and the Activity rows are GUI —
  need the running app. The error pre-checks + balance parsing are unit-tested.

---

## 2026-06-11 — Pending-approval visibility: Dock badge; diagnosed missing notif/red-dot

**Reported:** no menu-bar notifications, and no red dot on the tray or Dock icon.

**Diagnosis (joint symptom → shared cause).** The red dot (Rust) and the
notification (frontend) both fire from the *same* approval event, so losing both
together means **no prompt is reaching the UI**. Two features we just shipped
legitimately suppress the prompt: **auto-lock** (a locked wallet rejects requests
with `-32001` *before* prompting — confirmed: handlers touch the session, which
errors when locked, ahead of `gated_approval`) and **grants** (`gated_approval`
auto-approves a granted client's own-account ops with no prompt). Separately, the
OS **notification** needs permission + (reliably) the bundled app — that's why it
"stopped" after we moved notifications from Rust to the frontend.

**Did**
- Added a **Dock-tile badge** with the pending count
  (`WebviewWindow::set_badge_count`, macOS) to `refresh_tray`, alongside the tray
  red-dot. It's the most reliable signal — visible even if notification
  permission is denied — and now works because the app is `Regular` (has a Dock
  tile). Clears at zero pending. Desktop builds; clippy clean.
- Documented the indicator hierarchy + the "no indicator is usually correct"
  rule (lock/grant suppression; Activity tab is the diagnostic) in `desktop.md`.

**Not a code bug**
- The notification path + capability are correct; non-display is a macOS
  permission / bundled-app matter. The dialog + badge + tray dot don't depend on
  it. If prompts aren't firing at all, the cause is lock/grant (by design) — check
  the Activity tab.

**Not run-verified**
- Dock badge render, tray dot, and OS notification all need the running app.

---

## 2026-06-11 — `companion_reportIssue`: agent feedback → prefilled GitHub issue link

**Did**
- Added **`companion_reportIssue`** (auth, **no prompt**): takes the STRKD-FEEDBACK
  fields (`goal` + `needed` required; attempted/observed/limitation/impact/
  workaround_avoided/context/title optional) and returns a **prefilled GitHub
  `issues/new?title=…&body=…` link** plus the rendered title/body. It **files
  nothing** and stores **no GitHub token** — the operator opens the URL, reviews/
  edits, and submits in their own browser. Target repo defaults to
  `starknet-innovation/strkd`, overridable via `ServerState::with_issue_repo`
  (slug-validated to `owner/name`).
- Agent-authored text is **percent-encoded** (RFC 3986 query) so it can't break
  out of the query; body capped to ~5000 chars for URL-length safety; title
  tagged `[agent-feedback]`. Returns `filed:false` explicitly.
- Usage doc: new method entry + an `alpha_notice.github_shortcut` pointing agents
  at it (STRKD-FEEDBACK block stays the universal fallback). Docs + completeness
  test updated. **+4 dispatch tests** (URL shape/encoding, required fields, never
  prompts, requires pairing). **96 tests green, clippy clean**; desktop builds.

**Why / decisions**
- Chose the **prefilled-link** path over API auto-filing (maintainer's call): the
  repo is private and strkd is multi-operator, so a stored PAT + repo-write power
  doesn't generalize and adds credential risk. A deep-link keeps the human as the
  gate (their own GitHub identity/permissions, review before submit) and needs no
  secret in the wallet. The agent → operator relay block remains for everyone
  without repo access.
- No prompt: building a URL posts nothing, so it shouldn't burn a 60s approval.
- No desktop change needed — default repo slug is correct; reports also land in
  the Activity log like any RPC call.

**Not run-verified**
- That the generated URL actually opens a well-formed GitHub draft is a manual
  check (paste one into a browser). The URL construction + encoding are unit-tested.

---

## 2026-06-11 — Agent endpoint: alpha disclaimer + "report, don't work around" CTA

**Did**
- Added an **`alpha_notice`** block to the `GET /` agent contract (`usage.rs`):
  (1) a disclaimer that strkd is alpha; (2) a call-to-action telling agents **not
  to invent workarounds / fake results / handle keys themselves / silently give
  up** when they hit a limitation; (3) a copy-pasteable **`=== STRKD-FEEDBACK ===`**
  template the agent fills and hands to its operator to relay to the maintainer.
  Template fields chosen to make the fix actionable: goal, attempted, observed
  (literal JSON-RPC error), limitation, **needed** (the concrete capability),
  impact, **workaround_avoided** (so we design the need away), context
  (version/network from `companion_getStatus`).
- Guarded it in `tests/server.rs` (the doc must carry `alpha_notice` + the
  feedback sentinel). Updated `wallet-rpc.md`. **92 tests green, clippy clean.**

**Why**
- Maintainer wants agents to surface gaps as actionable reports, not paper over
  them — workarounds hide the gap and make it permanent. The structured block is
  greppable so a pasted report drops straight into a dev session.

**Decisions**
- Human-relay (agent → operator → maintainer), per the request — no new RPC
  method or programmatic issue sink for now. Could add `companion_reportIssue`
  later if we want strkd to capture reports itself.

---

## 2026-06-10 — Dock icon now shows; first git commit + push to origin

**Did**
- **Dock icon fix.** The strkd icon wasn't appearing in the Dock because the app
  set `ActivationPolicy::Accessory` (menu-bar-only, no Dock icon by design).
  Switched to `ActivationPolicy::Regular` so the icon shows in the Dock. Added a
  `RunEvent::Reopen` handler (macOS) so clicking the Dock icon re-shows the
  window after close-to-hide. Icon **assets** were already correct (navy/salmon
  strkd mark in `app-icon.png` → generated `icon.icns`/`128x128.png`); only the
  policy hid them. Desktop crate builds; clippy clean. Updated README + desktop.md.
- **Repository initialized.** `git init`, first commit of the whole tree, pushed
  to `https://github.com/starknet-innovation/strkd`.

**Decisions**
- Reversed the earlier "no Dock icon (accessory)" UX decision at the maintainer's
  request — the app is now a `Regular` app (Dock + Cmd-Tab presence) while still
  living in the menu bar via the tray. Close still hides rather than quits.

**Not run-verified**
- That the Dock icon renders and Dock-click re-show works is a GUI/runtime
  behavior — needs the running app to confirm.

---

## 2026-06-10 — Maintainer feedback: funding UX, notif buttons, requestGrant, auto-lock

Plus: created `docs/project/backlog.md` and recorded the **fund-sweep** path there
(not implemented).

**Did**
- **(a) Funding approval reworded** as a *request*, not a transaction: "Agent X
  (id) requests a top-up of Y STRK …". (The model was already request-based —
  agents call `companion_requestFunding {amount}`; the wallet builds the transfer
  from the manager. Only the display was tx-framed.)
- **(b) Notification Approve/Deny buttons.** Moved the banner to the frontend
  (`notify.ts`, `@tauri-apps/plugin-notification`): `registerActionTypes` +
  `onAction` → resolve the front-of-queue request. Removed the Rust-side post
  (no double). Added the notification capability perms. **GUI, not run-verified**;
  the in-app dialog is the reliable fallback.
- **(c) `companion_requestGrant`** — agents request an auto-approval window
  (1–90d); **always prompts** (escalation never auto-approved, even under an
  active grant). Also grantable from the Agents tab.
- **(d) Auto-lock** (was missing). `ServerState` tracks `last_activity_ms`
  (touched by every RPC request + on unlock; status-polling is IPC, doesn't
  count). Desktop background task locks after `auto_lock_minutes` (Settings;
  default 15, 0 = never). Revised the spec's "user-only activity" rule → "any RPC
  or unlock" so granted unattended agents aren't locked out mid-run.
- 3 new dispatch tests (requestGrant approved/always-prompts, activity recorded);
  usage doc + completeness test updated. Workspace: **92 green, clippy clean**;
  frontend + desktop build.

**Decisions**
- Auto-lock activity includes agent RPC (reconciles with grants); 0 disables it
  for fully-unattended agents. A genuine security/usability knob, user-controlled.
- requestGrant bypasses `gated_approval` (always prompts) — a grant can't escalate
  itself.

**Verify**
```bash
cargo test && cargo clippy --all-targets   # core 92
cd desktop && npm run build && (cd src-tauri && cargo build && cargo clippy)
```

**Not run-verified**
- Notification action buttons + the auto-lock background task are GUI/runtime —
  need the running app. The activity-tracking + requestGrant logic is unit-tested.

**Next / Resume**
- Funded-account live submit; Phase 3. Backlog: fund sweep.

---

## 2026-06-10 — Agent feedback round 2 (GoL): 4 DX improvements

Addresses `../gol_starknet/docs/strkd-feedback.md` items 2–5 (item 1, SNIP-36,
shipped in the entry below).

**Did**
- **#3 complete sign-only tx.** Extracted `node::invoke_v3_tx_json` (the single
  canonical INVOKE_TXN_V3 builder, used by both broadcast and sign-only). Sign-only
  now returns a full, canonical-hex, broadcast-ready tx (signature + tip +
  paymaster_data + account_deployment_data + DA modes + hex bounds + proof) — no
  hand-assembly, no mixed int/string typing. `sender_address` zero-padded to match
  account addresses elsewhere.
- **#2 `companion_estimateFee`.** Opt-in: returns suggested `resource_bounds`
  (canonical hex) + nonce from the node for sign-only callers who broadcast
  elsewhere. No prompt, no signing; needs a node (-32005 else). Documented "not
  for private SNIP-36 calldata".
- **#5 grant state on `companion_getStatus`.** When a token is presented, returns
  `grant: {active, expires_at}` (null otherwise) so a client knows whether to
  expect prompts.
- **#4 re-attach.** `companion_requestPairing {reattach:true}` re-issues a token
  for an existing same-(name,kind) client, keeping its `client_id` + grant +
  accounts (user-approved, account count shown). Stops re-paired agents from
  stranding funds. `ClientStore::find_id`/`reattach`.
- 4 new dispatch tests; updated 2 for the new tx shape. Workspace: **89 green,
  clippy clean**; desktop builds.

**Decisions**
- Re-attach is **name-based + human-approved** (prompt shows the account count) —
  the user is the gate against impersonation. Persisting the token remains the
  agent's first-line defense; re-attach is recovery.
- estimateFee is a **separate opt-in method**, not auto-on-addInvoke, so private
  SNIP-36 calldata is never simulated online unless the agent asks.

**Verify**
```bash
cargo test && cargo clippy --all-targets   # core 89
```

**Not addressed (noted)**
- A user-initiated **sweep** of stranded accounts → manager (item 4's alternative)
  isn't built; re-attach prevents the stranding in the first place. Sweep stays a
  possible follow-up.

**For the GoL agent**
- Sign-only now yields a ready-to-broadcast tx; call `companion_estimateFee` for
  bounds; check `companion_getStatus.grant`; recover via `reattach:true`.

**Next / Resume**
- Funded-account live submit; Phase 3 (Tongo/STRK20).

---

## 2026-06-09 — SNIP-36 proof-carrying invoke (feature request from GoL agent)

Implements `../gol_starknet/docs/strkd-snip36-feature-request.md`: enrich
`wallet_addInvokeTransaction` (not a new endpoint) with optional `proof_facts` +
`proof`.

**Did**
- `wallet-core`: `InvokeV3Params` gains `proof_facts: Vec<Felt>`; `invoke_v3_hash`
  now uses krusty's `compute_invoke_v3_hash_with_proof_facts` (empty ⇒ identical
  hash to before; non-empty ⇒ `Poseidon(proof_facts)` appended, per SNIP-36). The
  signature thus covers the proof facts.
- `node.add_invoke` carries `proof_facts` + `proof` and attaches them to the
  invoke tx JSON on broadcast.
- `dispatch.handle_add_invoke` parses `proof_facts`/`proof`; `submit:true` with
  proof_facts requires `proof` (114 otherwise); sign-only echoes them back so the
  agent can assemble its own broadcast. Approval summary notes "SNIP-36
  proof-carrying". Scoping/grant unchanged (own-account invoke).
- `server.rs`: raised the loopback body limit to 32 MB (proofs are multi-MB).
- Documented in the `GET /` contract (`usage.rs`: params + `snip36` note +
  top-level note), wallet-rpc.md, spec §7.2/§7.4.
- 2 dispatch tests: proof_facts changes the hash + is echoed; submit needs proof
  (114) then broadcasts. Workspace: **85 green, clippy clean**; desktop builds.

**Decisions**
- **Enrich, not a new endpoint** — exactly as the request argued; a verify tx is
  a normal INVOKE_TXN_V3, only the hash field + wire fields differ.
- The **sign-only path is fully delivered + tested** (krusty's hash variant is
  parity-tested). The SNIP-36 **broadcast wire fields** (`proof`/`proof_facts` on
  the tx) share the existing broadcast caveat — verify against a v0.10 node; the
  request confirms a v0.10 node accepts them.

**Verify**
```bash
cargo test && cargo clippy --all-targets   # core 85
```

**For the GoL agent**
- Acceptance test step 2 is now strkd-native: `wallet_addInvokeTransaction` with
  `proof_facts` (+`proof`, `submit:true`) from the agent's own account. Re-test
  the verify leg against the v0.10 node to confirm the broadcast wire fields.

**Next / Resume**
- Funded-account live submit (incl. a real proof-carrying verify); Phase 3.

---

## 2026-06-09 — Complete the `GET /` agent contract

**Did**
- Rewrote `usage.rs` (served at `GET /` / `/usage`) into the **complete
  agent-facing contract**. Added: a `concepts` block (accounts/scoping, networks,
  funding, **permission grants**), `submit_model`, the missing methods
  (`companion_getStatus`, `wallet_supportedWalletApi`, `wallet_switchStarknetChain`,
  `wallet_watchAsset`), per-method `auth`/`prompts` flags + Call-shape/aliases, a
  `deferred` list, and every error code (incl. 116/117/-32700). Documented that
  funding always prompts and that a grant may auto-approve own-account ops.
- Hardened `tests/server.rs`: asserts the doc carries the core sections and lists
  **every implemented method**, so it can't drift from the dispatcher.
- Updated `wallet-rpc.md` Discovery section to call it the full contract.
  Workspace: **83 green, clippy clean**.

**Decisions**
- Keep the contract in one hand-authored `usage.rs` (single source of truth)
  guarded by a test, rather than generating it — small surface, stays readable.

**Verify**
```bash
cargo test -p wallet-rpc --test server   # checks the doc's completeness
# or, against a running app: curl http://127.0.0.1:<port>/
```

**Next / Resume**
- Funded-account live submit; Phase 3 (Tongo/STRK20).

---

## 2026-06-09 — Persist pairings + grants across restarts

**Did**
- `ClientStore` is now **file-backed**: `open(path)` loads existing clients;
  every mutation (`pair`/`grant`/`revoke_grant`/`revoke`) persists to
  `clients.json` (atomic temp+rename, `0600`). `ServerState::with_clients_path`
  wires it; the desktop uses `data_dir/clients.json`.
- `PairedClient` is now serde-(de)serializable; `token_hash` changed from
  `[u8;32]` to a **hex String** (only the hash is written — never the raw token).
- 2 tests (`tests/clients.rs`): pairings + grants survive a reopen (token still
  verifies, grant still active); the raw token is never on disk; revoke persists.
  Workspace: **83 green, clippy clean**; desktop builds.

**Decisions**
- **JSON file** (`clients.json`), consistent with vault/config persistence
  (simpler than the SQLite the spec originally sketched; updated §10).
- **Auto-persist on every mutation** via `ClientStore::persist` — no separate
  "save" call to forget. In-memory store (no path) for tests.
- Token *hashes* only on disk (one-way) — an attacker with the file still can't
  authenticate without the token. `0600`, same data dir as the encrypted vault.

**Verify**
```bash
cargo test && cargo clippy --all-targets   # core 83
```

**Note**
- Corrects the previous entry's "in-memory, cleared on restart" caveat — pairings
  and grants now persist. (Vault + request log were already persisted.)

**Next / Resume**
- Funded-account live submit; Phase 3 (Tongo/STRK20).

---

## 2026-06-09 — Time-bounded agent permission grants (auto-approval)

**Did**
- `auth.rs`: `PairedClient.granted_until` + `grant_active(now)`; `ClientStore`
  `grant`/`revoke_grant`/`list` (+ `ClientInfo`).
- `dispatch.rs`: `gated_approval` — auto-approves a client's **own-account** ops
  (sign/invoke/declare/deploy/createAgentAccount) while a grant is active; routed
  all five through it. **Funding, pairing, switchChain, watchAsset still prompt**
  (call the approver directly).
- Desktop: `list_clients` / `grant_permission(days, clamped 1–90)` /
  `revoke_permission` commands + an **Agents** tab (grant 1/2/3 months, shows
  remaining time, revoke).
- 3 dispatch tests using a **Reject approver** to prove gating: grant
  auto-approves signing; grant does NOT auto-approve funding (still 113);
  expiry/revoke re-prompt. Workspace: **81 green, clippy clean**; frontend +
  desktop build.

**Decisions**
- **Grant covers own-account ops only; funding always prompts** — funding spends
  the user's manager account. Safe because agents are independently scoped to
  their own accounts, so a grant can't reach user/other-agent funds.
- **Capped at 90 days (3 months)**, revocable. **In-memory** like pairings
  (restart clears both) — consistent; persistence is a noted follow-up.
- Auto-approved ops are still logged (Activity) — grant removes the prompt, not
  the audit record.

**Verify**
```bash
cargo test && cargo clippy --all-targets   # core 81
```

**NOT verified**
- The Agents tab UI (grant/revoke buttons, countdown) is GUI — needs the running
  app. The gating logic itself is covered by the 3 dispatch tests.

**Next / Resume**
- Funded-account live submit; Phase 3 (Tongo/STRK20); optionally persist
  grants/pairings.

---

## 2026-06-09 — Accounts: Testnet/Mainnet subtabs + network selector

**Did**
- Answered the network question (in docs): **default network is Sepolia/testnet**
  (`ChainId::Sepolia` hardcoded at startup). Confirmed via krusty's OZ class
  manifest that the class hash is **identical on SN_MAIN and SN_SEPOLIA**, so an
  account's address is the **same on both networks** — only deployment status and
  execution target differ.
- Desktop `set_network(network)` IPC command → `session.set_chain`.
- Accounts tab gains **Testnet / Mainnet subtabs**: selecting one switches the
  active network and reloads per-network deploy-status (the account list is the
  same under both). Frontend `api.setNetwork`; `Accounts` now takes `status`.

**Decisions**
- Subtab = the wallet's **active network** (coherent with the single-active-chain
  model + per-network nodes), not just a view — so deploy/fund/sign and agent ops
  target the selected network. Topbar shows the active network.
- Not persisted across restart yet (defaults to Sepolia) — minor follow-up.

**Verify**
```bash
cargo test && cargo clippy --all-targets   # core 78, clippy clean
cd desktop && npm run build && (cd src-tauri && cargo build && cargo clippy)
```

**NOT verified**
- GUI behavior (subtab switching, per-network deploy-status) — needs the running
  app. Mainnet deploy-status needs a Mainnet RPC set in Settings (else shows
  unknown).

**Next / Resume**
- Funded-account live submit; Phase 3 (Tongo/STRK20).

---

## 2026-06-09 — wallet_addDeclareTransaction (addStarknetChain skipped)

**Did**
- `wallet-core::sign_declare_v3` (+ `SignedDeclare`) — DECLARE v3 hash via krusty's
  `compute_declare_v3_hash` + sign. Needs only `class_hash` + `compiled_class_hash`.
- Node trait `estimate_declare` / `add_declare` (+ HTTP impl `declare_tx_json`):
  these take the full Sierra `contract_class` (caller-supplied, passed verbatim).
- Session `sign_declare_for`; dispatch `handle_add_declare` (removed from
  deferred): scoped sender, auto nonce (node) or supplied, fee from
  caller-bounds or estimate (needs node + contract_class), prompt, sign;
  `submit:true` broadcasts (needs node + contract_class). Sign-only default needs
  no contract_class.
- 3 new dispatch tests (sign-only, submit w/ node, missing-class-hash → 114);
  repointed the deferred test at `addStarknetChain`. Workspace: **78 green,
  clippy clean**.

**Decisions**
- **Sign-only declare needs only the two class hashes** (no big blob) — the
  high-value, testable path. The full `contract_class` is required only for
  estimate/broadcast and is passed through verbatim (caller owns class encoding).
- **`addStarknetChain` skipped** per request — arbitrary chains need a
  generalized `ChainId`; still `-32601`.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```

**Caveat**
- The declare **node wire format** (`contract_class` encoding, addDeclare/estimate)
  shares the broadcast caveat: not live-verified (needs a real class to declare).
  The hash + signature path is tested.

**Next / Resume**
- Funded-account live submit (broadcast/declare); then Phase 3 (Tongo/STRK20).

---

## 2026-06-09 — Agent feedback: RPC deploy path + lenient Call shape

External agent testing surfaced two real gaps (pre-node-fix):

**Did**
- **`companion_deployAccount` RPC method** — agents had no way to deploy their
  (counterfactual) accounts over RPC; only the desktop had a button. New method:
  agent-scoped (deploys one of the caller's own accounts), prompts, auto-estimates
  the fee with a node (or caller-supplied bounds), and either signs-only (returns
  the DEPLOY_ACCOUNT tx to broadcast) or `submit:true` broadcasts. Reuses the
  existing deploy plumbing.
- **Lenient Call parsing** — the agent sent `entrypoint` (a function name) and got
  "missing entry_point_selector". Now `parse_calls` accepts
  `entry_point_selector`/`entrypoint`/`entry_point`/`selector` (value = name *or*
  0x; `resolve_selector` already handled both) and `contract_address`/
  `contractAddress`/`to`. The `GET /` usage doc now specifies the **Call object
  shape** explicitly (the agent noted it was undocumented).
- Updated the `GET /` quickstart to a clear create → fund → **deploy** → sign/send
  flow.
- 4 new dispatch tests (entrypoint-name alias; deploy sign-only / submit /
  no-node-errors). Workspace: **75 green, clippy clean**.

**Decisions**
- Deploy is scoped like funding (caller's own accounts only) and **prompts** (it
  spends funds). Default sign-only, `submit:true` broadcasts — consistent with
  invoke/funding.
- Kept the desktop Deploy button (IPC) *and* added the RPC method — humans and
  agents each have a path.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```

**Notes**
- The other items the agent hit (broadcast failing) were the node wire-format
  bugs fixed in the entry below (block tag + redirect).

**Next / Resume**
- Funded-account submit to confirm the broadcast hop; last two Phase 2 methods.

---

## 2026-06-09 — Node wire format live-verified (Sepolia v0.10) + 2 fixes

**Did**
- The user supplied a Sepolia node (`sepolia.nodes.starknet.org/rpc/v0_10`, spec
  **0.10.3**) — saved to memory ([[strkd-sepolia-rpc]]). Probed it with read-only
  calls (curl + a real `HttpStarknetRpc` run via `examples/node_probe`); no
  signing/broadcast/key material.
- **Confirmed correct** against v0.10: `getNonce`, `getClassHashAt` (deploy
  status — true for STRK, false for the counterfactual test account), and
  `estimateFee` request **and** response (3-resource fee model fields
  `l1_gas_consumed`/`…_price`/`l2…`/`l1_data…`; V3 tx JSON incl.
  `resource_bounds`, DA modes, `contract_address_salt`, `tip`, `paymaster_data`).
- **Fixed 2 bugs the live run caught:** (1) block tag `pending` → `latest`
  (v0.10 dropped `pending` — "unknown block tag"); (2) http→https **301**: reqwest
  turns a 301 POST into GET and drops the body, so the client now disables
  auto-redirect and re-POSTs to `Location` once, caching it.
- Added `examples/deploy_data.rs` (wallet-core) + `examples/node_probe.rs`
  (wallet-rpc) as wire-format verification tools (test vector, read-only).
- Workspace: **71 green, clippy clean**.

**Decisions**
- **`latest` block tag** (not `pre_confirmed`) for cross-version compatibility;
  both work on v0.10, `latest` is valid on older specs too.
- Kept the hand-written client (rather than swapping to `starknet-rs`) since it's
  now verified against the live node — only the broadcast hop remains.

**Verify**
```bash
cargo run -p wallet-rpc --example node_probe   # live read/estimate against Sepolia
cargo test && cargo clippy --all-targets
```

**Still unconfirmed**
- The **broadcast hop** (`add_invoke`/`add_deploy_account`) — needs a signed tx
  from a funded account. The tx object is the same one `estimateFee` accepted, so
  only the submit wrapper (`{"invoke_transaction":…}` /
  `{"deploy_account_transaction":…}`) is unproven.

**Next / Resume**
- Funded-account submit to confirm broadcast; last two Phase 2 methods.

---

## 2026-06-09 — Deploy undeployed accounts + tray-icon fix

**Did**
- **Tray-icon fix:** the menu-bar icon used hand-made `tray-normal/dot.png` that
  `tauri icon` doesn't regenerate, so a new `app-icon.png` updated the Dock but
  not the tray. Now the tray derives from the generated `icons/128x128.png`
  (same source as the app icon) and the red-dot variant is overlaid **at
  runtime** (via the `image` crate → `Image::new_owned`). Removed the stale PNGs.
- **Deploy feature:** `wallet-core::sign_deploy_account_v3` (DEPLOY_ACCOUNT v3
  hash + sign from OZ deployment data); node trait gains `is_deployed`,
  `estimate_deploy_account`, `add_deploy_account`; session
  `sign_deploy_account_for`; desktop `deploy_status` + `deploy_account` commands;
  Accounts tab shows an **"undeployed"** badge + **Deploy** button (estimate →
  sign → broadcast). The button click is the consent (no extra prompt).
- 1 new session test (deploy signing). Workspace: **71 green, clippy clean**
  (core + desktop); frontend builds.

**Decisions**
- **Deploy needs a node + a funded account** (OZ accounts pay their own deploy
  fee — chicken-and-egg, so fund first). `deploy_status` returns `None` without a
  node, so the UI only shows Deploy when it actually knows the account is
  undeployed.
- **Tray icon derived from the generated base** so it always tracks `app-icon.png`
  — fixes the reported bug at the source.

**Verify**
```bash
cargo test && cargo clippy --all-targets
cd desktop && npm run build && (cd src-tauri && cargo build && cargo clippy)
```

**NOT verified (honest boundary)**
- The **DEPLOY_ACCOUNT broadcast + estimate wire format** is mock-tested only —
  needs a live node (same caveat as invoke broadcast).
- The Deploy button / undeployed badge are GUI — need the running app.

**Next / Resume**
- Live-verify (incl. deploy a funded Sepolia account); last two Phase 2 methods.

---

## 2026-06-09 — Desktop UX: icon, copy-address, zoom, non-invasive notifications

**Did** (all desktop-layer; not run-verified)
- **Starknet-themed app icon** (navy + salmon mark) regenerated across all sizes
  via `tauri icon`; tray normal/dot variants generated too. Noted it's a themed
  placeholder, not the official trademarked logo (swap-and-regenerate documented).
- **Copy-address button** per row in the Accounts tab (`navigator.clipboard`).
- **⌘/Ctrl +, −, 0 zoom** (`useZoom` hook, persisted) + base font 13→14px.
- **Non-invasive request notifications:** the approval bridge no longer raises
  the window. It posts a menu-bar **notification** (`tauri-plugin-notification`)
  and swaps the tray to a **red-dot** icon + "N pending" tooltip while requests
  are waiting; the dot clears on respond/timeout (`refresh_tray`). Window only
  opens via tray → Open.

**Decisions**
- **Red-dot tray badge is the robust signal**; the notification banner is
  best-effort (needs OS notification permission). Both implemented.
- `Image::from_bytes` for the tray needs tauri's **`image-png`** feature (added).
- Themed icon, not the trademarked logo — avoids misusing the brand asset.

**Verify**
```bash
cargo test && cargo clippy --all-targets        # core (70), clippy clean
cd desktop && npm run build && (cd src-tauri && cargo build && cargo clippy)
```

**NOT verified (honest boundary)**
- All four are **GUI behaviors** — icon appearance, clipboard, zoom keys, the
  notification banner + tray red dot — none are exercised by automated tests.
  They need the running app (your current testing).

**Next / Resume**
- Unchanged: live-verify; then the last two Phase 2 methods.

---

## 2026-06-09 — Phase 2: switchStarknetChain + watchAsset (+ per-network nodes)

**Did**
- **Per-network node model:** `ServerState` now holds `HashMap<ChainId, node>`
  (was a single node). `set_node(chain, …)` / `node_for(chain)` /
  `has_node_for(chain)`; `resolve_exec` / `sign_and_submit` take the active
  chain. So switching networks switches the RPC endpoint — no cross-network
  broadcasts.
- **`wallet_switchStarknetChain`**: parse `chainId` felt → `ChainId::from_felt`
  (unknown → 117), prompt, set the session chain. The active node follows.
- **`wallet_watchAsset`**: prompt + store the token in an in-memory watch list
  (`watched_assets`), return true.
- Desktop: Settings `set_settings` + startup register a node for **each** network
  from `config.json`; `status.node_configured` reflects the active chain.
- 4 new dispatch tests (switch success/unknown-117/reject-113, watchAsset).
  Workspace: **70 green, clippy clean** (core + desktop).

**Decisions**
- **Deferred `addDeclareTransaction`** (sign-only is easy, but broadcasting needs
  the full contract-class blob through the node — bigger + unverifiable here) and
  **`addStarknetChain`** (arbitrary chains need a generalized `ChainId` beyond the
  Sepolia/Mainnet enum). Both still return `-32601`, documented.
- **watchAsset list is in-memory** (display-only) for now — noted.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 70 passing tests, no clippy warnings.

**Next / Resume**
- The last two Phase 2 methods (declare, addStarknetChain); live-verify the node.

---

## 2026-06-09 — Settings control panel + installable bundle

**Did**
- Made the node **runtime-swappable**: `ServerState.node` is now behind an
  `RwLock` with `set_node` / `has_node` / `current_node`. Dispatch reads a cloned
  snapshot (never held across an await).
- Desktop **Settings** tab + `settings.rs`: per-network RPC URLs persisted to
  `config.json`; `get_settings`/`set_settings` IPC commands; `set_settings`
  rebuilds the node for the active chain immediately. Replaced the
  startup-only `STRKD_RPC_URL` env var (env removed). Startup loads the node from
  `config.json`.
- Verified the **installable bundle**: `npm run tauri build` → `strkd.app` +
  `strkd_0.1.0_aarch64.dmg`. Documented build-and-install vs dev-run and the
  clean-quit/port behavior (answers "dead processes hogging ports": install +
  launch normally + Quit via tray = clean shutdown; ephemeral port + rewritten
  `port.lock` self-heals).
- 1 new dispatch test (runtime set/clear node). Workspace: **66 green, clippy
  clean** (core + desktop); frontend builds; release bundle builds.

**Decisions**
- **RwLock over the node** (not `arc-swap`) — std-only, and the snapshot-clone
  pattern keeps it off the async path.
- **Settings are IPC-only** (may embed an API key) — never exposed over the HTTP
  service, mirroring dinner.
- **Dropped the env var** in favor of the control panel, per the user's request.

**Verify**
```bash
cargo test && cargo clippy --all-targets
cd desktop && npm run tauri build   # → src-tauri/target/release/bundle/{macos,dmg}
```

**Next / Resume**
- Run-verify (set RPC in Settings, fund an agent with submit:true); then the
  remaining Phase 2 methods. See `status.md`.

---

## 2026-06-09 — Phase 2 (core): node seam — broadcast + auto nonce/fee

**Did**
- Added `wallet-rpc::node`: `StarknetRpc` trait (`get_nonce`, `estimate_invoke`,
  `add_invoke`) + `HttpStarknetRpc` (reqwest JSON-RPC). `ServerState::with_node`
  attaches it.
- Refactored the invoke path into `resolve_exec` (caller-supplied nonce/bounds
  win, else node-resolved) + `sign_and_submit` (sign; broadcast on `submit:true`).
  `wallet_addInvokeTransaction` and `companion_requestFunding` now share it.
- With a node: `submit:true` broadcasts and returns the on-chain hash; nonce +
  fee are auto-filled. Without a node: sign-only, caller supplies nonce/bounds,
  else `-32005 NoNode`. **`companion_requestFunding` is now turnkey** (no
  nonce/bounds needed when a node is set).
- Desktop reads `STRKD_RPC_URL` and attaches `HttpStarknetRpc`; `status` reports
  `node_configured`; the Connect tab shows node status + the env hint.
- Moved `reqwest` to a normal dep (rustls-tls). New error codes `-32004` (node)
  / `-32005` (no node). Updated the `GET /` usage doc.
- Tests: a `MockNode` (in `tests/dispatch.rs`) + 4 node tests (submit-without-node
  errors, submit-with-node broadcasts, auto nonce/fee, funding turnkey).
  Workspace: **65 green, clippy clean**; frontend + desktop build.

**Decisions**
- **Node is an injectable trait seam** (like `Approver`) so the orchestration is
  fully mock-tested and the live wire format is isolated/swappable.
- **Caller-supplied nonce/bounds still win** — keeps sign-only working with no
  node, and lets advanced callers override estimation.
- **Config via env (`STRKD_RPC_URL`)** for now; settings UI is a follow-up.
- 1.5× safety margin on estimated bounds.

**Verify**
```bash
cargo test && cargo clippy --all-targets        # core (65)
cd desktop && npm run build && (cd src-tauri && cargo build)
```

**NOT verified (honest boundary)**
- The **HTTP node wire format** (`starknet_estimateFee` request/response shape,
  V3 invoke JSON) is **not** verified against a live node — mock tests cover only
  the orchestration. This is the main thing to confirm in live testing.
- Broadcast/estimate are GUI-reachable but, like the rest of the desktop, not
  run-verified.

**Next / Resume**
- Live-verify against a Sepolia RPC; then the remaining Phase 2 methods
  (switchStarknetChain, addDeclareTransaction, watchAsset, addStarknetChain).

---

## 2026-06-09 — Self-describing `GET /` usage endpoint + desktop "Connect" prompt

**Did**
- Added `wallet-rpc::usage` — a hand-authored JSON usage document (service
  summary, transport + required headers, approval model, 5-step agent
  quickstart, method list with prompts, error codes).
- Served it at **`GET /`** and `GET /usage` (axum `get(usage).post(rpc)` on `/`).
  **Open** — no auth, no transport guard: it returns only public usage info and
  changes nothing, so an agent can discover the wallet before it knows to send
  `X-Companion-Client`.
- Desktop: new **Connect tab** (`Connect.tsx`) showing the local endpoint and a
  **copy-paste agent prompt** (with a Copy button) that points the agent at
  `GET <url>/`. Mirrors `../dinner`'s feature.
- 1 new HTTP test (usage doc open + self-describing + `/usage` alias).
  Workspace: **62 green, clippy clean**; frontend + desktop build.

**Decisions**
- **`GET /` is unguarded** while `POST /` keeps the CSRF/rebinding guard — the
  doc is harmless public info and must be reachable for discovery.
- **Prompt built in the frontend** from `status.service_url` (the port is
  ephemeral); the short prompt defers full detail to the endpoint — single
  source of truth lives in `usage.rs`, like dinner.

**Verify**
```bash
cargo test && cargo clippy --all-targets        # core (62)
cd desktop && npm run build                      # frontend
(cd desktop/src-tauri && cargo build)            # shell
# and over the wire: curl http://127.0.0.1:<port>/  → usage JSON
```

**Next / Resume**
- Unchanged: run-verify the desktop app, or Phase 2. See `status.md`.

---

## 2026-06-09 — Agent funding request (`companion_requestFunding`)

**Did**
- Added two `companion_*` methods (wallet-rpc dispatch): `companion_fundingSource`
  (reveals the manager account address; no approval) and `companion_requestFunding`
  (agent-only; approval-gated).
- Funding builds a STRK `transfer(recipient, amount)` from the **manager
  account** (resolved by the wallet) to one of the **agent's own scoped
  accounts**, shows an approval prompt, and signs it on approval (reuses
  `sign_invoke_for`). Sign-only → returns the signed transfer; the manager pays
  the fee.
- Added `WalletSession::manager_account(index)` (default user root, index 0) and
  `parse_u128` helpers; a `STRK_TOKEN_ADDRESS` constant (overridable, flagged
  for network verification).
- 6 new dispatch tests. Workspace: **61 green, clippy clean**. Desktop still
  builds (no change needed).

**Decisions**
- **Agent never chooses the sender.** The recipient must be a scoped agent
  account (default its first); the sender is the wallet-resolved manager. So an
  agent can only pull funds *into its own accounts*, never drain an arbitrary
  one — and every request needs human approval.
- **Manager default = user root** (`m/44'/9004'/0'/0/0`), per the user's
  suggestion; overridable via `funding_source_index`. A persistent "marked as
  manager" setting is a noted follow-up.
- **No desktop change needed** — the approval broker is generic, so the funding
  prompt surfaces in the menu bar through the existing bridge. The summary
  (recipient / amount / manager / network) is built in the handler.
- **Amount in fri, u128** (fits any real STRK amount; high felt = 0). Sign-only
  requires the caller to supply the manager's `nonce` + `resource_bounds`
  (Phase 2 removes this).

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 61 passing tests, no clippy warnings.

**Next / Resume**
- Unchanged: run-verify the desktop app, or Phase 2 (which also makes funding
  turnkey). See `status.md` → Resume here.

**Notes / caveats**
- `STRK_TOKEN_ADDRESS` is the commonly-cited STRK fee-token address; **verify
  against the target network** before relying on it (callers can override via
  `token`). *(Crypto/address claim — verify independently.)*
- Sign-only: the returned transfer still needs broadcasting (keyless; the agent
  can do it). Full automation (auto-nonce/estimate/broadcast) is Phase 2.

---

## 2026-06-09 — Desktop app shell (Tauri 2 + React) — built, not run-verified

**Did**
- Chose the frontend stack by matching `../dinner`: **Tauri 2 + React 18 + TS +
  Vite** (dev port 1420), desktop crate excluded from the root workspace,
  reusing the core crates by path.
- Built `desktop/` (React frontend) + `desktop/src-tauri/` (Rust shell). The
  shell hosts the loopback service, builds a `ServerState` (ChannelApprover +
  file log + vault store, locked session), and adds a menu-bar tray
  (Open/Quit, hide-on-close, macOS accessory).
- **Approval bridge:** drains `PendingApproval`s → emits `approval-request`
  events → frontend `ApprovalDialog` → `respond_approval` resolves the oneshot;
  60s auto-reject (spec §8).
- IPC commands: status, generate/import/finalize onboarding, unlock/lock,
  list/add accounts, recent_log, respond_approval. Frontend routes
  onboarding → unlock → main (Accounts/Activity).
- Added `WalletSession::create_user_account` (+ 2 tests) and a `Display`/`Error`
  impl for `WalletRpcError` (needed by the desktop's `?`/`to_string`).
- Generated app icons from a stdlib-built source PNG via `tauri icon`.

**Verified**
- Core: **55 tests pass** (+2 session), clippy clean.
- Frontend: `tsc` (strict) + `vite build` pass → `dist/` produced.
- Desktop Rust crate: `cargo build` compiles, clippy clean.

**NOT verified (honest boundary)**
- The **GUI is not run-verified** — tray appearance, window, and the
  onboarding/unlock/approval flows need the app launched on a display. No
  automated test covers them. Status reflects this as *built, not run-verified*.

**Decisions**
- **No starknet-rs / extra deps** in the desktop crate — it only needs
  `wallet-core` + `wallet-rpc` + tauri.
- **Approval bridge over Tauri events**, with the oneshot responder held in a
  shared map keyed by id; whoever removes the entry first (user response or 60s
  timer) resolves it — no double-send.
- Onboarding briefly holds the generated mnemonic (zeroized) so the user can
  back it up before setting a passphrase — the one place a real seed transits
  the desktop crate, by necessity.

**Verify**
```bash
cargo test && cargo clippy --all-targets        # core
cd desktop && npm install && npm run build      # frontend
(cd desktop/src-tauri && cargo build)           # shell
cd desktop && npm run tauri dev                 # run (human verification)
```

**Next / Resume**
- Run-verify the app, or start Phase 2 (broadcast + networks). See `status.md`
  → Resume here.

---

## 2026-06-09 — Transport hardening (CSRF / DNS-rebinding guard)

**Did**
- Added `server.rs::transport_guard`, applied before dispatch: reject
  `Origin`/`Referer`; require loopback `Host` (`is_loopback_host`); require a
  non-empty `X-Companion-Client` header. Failures → HTTP 403 + JSON-RPC error
  `-32003` (new `WalletRpcError::TransportRejected`).
- Changed `rpc_handler` to return `(StatusCode, Json<Response>)` so transport
  rejections are 403 while JSON-RPC-level errors stay 200.
- 5 inline unit tests (host parsing + each guard rejection) and 2 new HTTP tests
  (403 without the custom header; 403 with `Origin`); updated the existing HTTP
  test's helper to send the header. Workspace: **53 green, clippy clean**.

**Decisions**
- **Host check rejects any non-loopback host-part** (not a port match): a
  DNS-rebinding attack sends `Host: evil.com` resolving to 127.0.0.1, so the
  host *name* is the tell — no need to know the bound port.
- **Custom header value is not authenticated** — the bearer token does auth; the
  header exists purely to force browsers into a CORS preflight we never answer.
- Guard lives in `server.rs` (HTTP layer); `dispatch()` stays transport-agnostic.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 53 passing tests, no clippy warnings.

**Fixed during the session**
- `is_loopback_host` mishandled a bare unbracketed `::1` (last-colon port-strip
  mangled the IPv6 literal). Reworked to: bracketed → between `[]`; exactly one
  colon → strip port; otherwise use as-is.

**Next / Resume**
- Tauri app shell (Phase 1 wrap-up). See `status.md` → Resume here. Pick the
  frontend framework first.

**Notes / caveats**
- The header requirement assumes native callers know to send
  `X-Companion-Client`; document this in the (future) client/SDK guidance.

---

## 2026-06-09 — On-disk vault persistence (transactional)

**Did**
- Added `VaultStore` (`wallet-rpc::store`): atomic `vault.bin` load/save (temp
  file + rename, `0600` on unix). Sees ciphertext only — no key material.
- Threaded the **vault passphrase** into the unlocked `WalletSession` (alongside
  the mnemonic, zeroized) so it can `reseal()` itself. Renamed the old
  `seal(passphrase)` → `reseal()` (uses the stored passphrase). Added
  `rollback_account()`.
- Made `companion_createAgentAccount` **persist transactionally**: when a
  `VaultStore` is configured (`ServerState::with_vault_store`), it re-seals and
  writes the vault; on write failure it rolls back the in-memory add so memory
  and disk never diverge.
- Added `Registry::remove_by_address` (wallet-core) for the rollback.
- 2 new persistence tests (full create→reopen→unlock cycle; wrong-passphrase
  rejection). Workspace: **46 green, clippy clean**.

**Decisions**
- **Store the vault passphrase in the unlocked session**, not just the mnemonic.
  The mnemonic (more sensitive) is already in memory while unlocked; holding the
  passphrase (zeroized) is what enables auto-reseal without re-prompting on every
  registry change. Wiped on lock.
- **Transactional with rollback** rather than fire-and-forget persistence: if the
  disk write fails, the account is removed from the in-memory registry and the
  request errors — so a caller never believes an account is durable when it
  isn't.
- **VaultStore lives in `wallet-rpc`, not `wallet-core`.** Keeps `wallet-core`
  filesystem-free (pure crypto); file I/O already lives in `wallet-rpc` (the
  SQLite log).
- **Atomic write** (temp + rename) so a crash mid-write can't corrupt the vault.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 46 passing tests, no clippy warnings. (The persistence test runs ~3s —
two real Argon2id seals at 64 MiB / 3 passes; expected.)

**Next / Resume**
- Transport hardening (Origin/Host/custom-header). See `status.md` → Resume here.

**Notes / caveats**
- `vault.bin` is written `0600` (unix); on non-unix the perms step is skipped —
  revisit for Windows when that platform is targeted.
- Unlock is still driven by `WalletSession::unlock` directly; a user-facing
  unlock flow (and a `companion_unlock` method, if desired) comes with the app
  shell.

---

## 2026-06-09 — Persistent SQLite request log

**Did**
- Replaced the in-memory `RequestLog` with a SQLite-backed store (`rusqlite`,
  `bundled` feature — compiles SQLite from source, no system dep). Supports
  file-backed (`open`) and in-memory (`in_memory`) stores; tests use in-memory
  so they exercise the real SQL.
- Enriched `LogEntry` to spec §9: timestamp, method, client, network, decision,
  outcome, error code, latency, and full params/result JSON.
- Added the `full_payloads` toggle (default on for debugging); when off,
  `record` drops the params/result columns. Redaction centralized in
  `RequestLog::record`.
- `dispatch` now measures latency, captures params/result, reads the network,
  and writes a rich entry. `ServerState::new` defaults to an in-memory log;
  `ServerState::with_log` accepts a file-backed one for production.
- 5 new log tests; updated the `requests_are_logged` dispatch test to the new
  API and to assert network/outcome/payload capture. Workspace: **44 green,
  clippy clean**.

**Decisions**
- **rusqlite (sync) under the existing log mutex**, not async sqlx. Log inserts
  are tiny and human/agent-paced; a sync insert under the mutex is simplest.
  Noted that a writer task / `spawn_blocking` is the escape hatch if throughput
  ever matters.
- **One SQLite type, two open modes** (file + `:memory:`) instead of a trait
  with separate in-memory and SQLite impls — tests then run the real SQL path.
- Confirmed params are safe to log: no RPC method takes secret material in
  params, and bearer tokens ride the `Authorization` header (never logged; only
  the resolved client label/id is stored).

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 44 passing tests, no clippy warnings.

**Next / Resume**
- Persist registry changes + on-disk vault lifecycle (thread the vault
  passphrase through the session). See `status.md` → Resume here.

**Notes / caveats**
- The on-disk `requests.db` holds public request data; with full payloads on it
  reveals account activity — treat the data dir as AMBER (spec §9). The
  disable-toggle ships (`set_full_payloads`).

---

## 2026-06-09 — `wallet_addInvokeTransaction` (sign-only)

**Did**
- Added `wallet-core::tx`: `starknet_keccak`, `get_selector_from_name`,
  `resolve_selector` (name or hex), `encode_calls` (Cairo 1 `__execute__`
  layout), `invoke_v3_hash`, and `sign_invoke_v3` → `SignedInvoke`. Re-exported
  `Call`, `InvokeV3Params`, `ResourceBounds`, `DaMode`.
- Wired `wallet_addInvokeTransaction` (sign-only) through `wallet-rpc`:
  param parsing (calls, nonce, resource_bounds), scope enforcement, a
  decoded-calls + fee approval prompt, and the sign-only response
  (`{transaction_hash, signature, signed_transaction, submitted:false}`).
  `submit:true` returns `-32601` (Phase 2). Removed it from the deferred list.
- 6 new `wallet-core` tests + 5 new dispatch tests. Workspace: **39 green,
  clippy clean**.

**Decisions**
- **Did NOT add `starknet-rs`.** Checking source showed the only missing pieces
  were a selector helper and the multicall encoder (krusty's
  `compute_invoke_v3_hash` already takes flat calldata). starknet-rs ships its
  own `starknet-types-core`; any skew from krusty's `0.2.0` would yield two
  incompatible `Felt` types and break the handoff into the hash. Implemented the
  two small primitives in-house instead. (This reversed the earlier tentative
  lean toward starknet-rs — the version-skew risk decided it.)
- **Selector correctness pinned by a golden KAT**: `get_selector_from_name(
  "transfer")` == the canonical ERC20 selector. Confirms keccak + 250-bit
  masking.
- **Sign-only, caller-supplied fees.** Auto fee-estimation needs node
  connectivity (Phase 2), so for now the caller provides nonce + resource
  bounds; the prompt shows them.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 39 passing tests, no clippy warnings.

**Next / Resume**
- Persistent request log (SQLite). See `status.md` → Resume here.

**Notes / caveats**
- **Invoke-hash end-to-end acceptance is unproven**: selector is golden-tested
  and krusty's V3 hash is parity-tested upstream, but "accepted by a Sepolia
  node" needs a golden tx-hash vector or a live submission — flagged for the
  security-reviewed test plan, not asserted here.
- Broadcast (`submit:true`) and fee estimation remain Phase 2.

---

## 2026-06-09 — Phase 1 (most): `wallet-rpc` service built & green

**Did**
- Created the `wallet-rpc` crate: JSON-RPC 2.0 types, error→code mapping, a
  pairing/bearer-token `ClientStore` (stores only token hashes), a blocking
  `Approver` broker (`AutoApprover` for tests, `ChannelApprover` for the UI), a
  `WalletSession` over `wallet-core`, an in-memory request log, the `dispatch`
  router, and a loopback axum server with `port.lock` discovery.
- Implemented the read-only methods, `wallet_signTypedData`,
  `wallet_deploymentData`, and the `companion_*` methods (pairing, status,
  list/create-agent-account). Deferred methods return `-32601`.
- Added small `wallet-core` helpers needed by the service: `deployment_data`,
  `sign_typed_data` (SNIP-12), `DeploymentData`.
- 13 new tests (12 dispatch + 1 HTTP smoke); whole workspace: **28 green,
  clippy clean**.

**Decisions**
- `dispatch()` is transport-agnostic (takes token + parsed request) so the
  whole service is testable without HTTP; `server.rs` is a thin axum wrapper.
- Approval is **lock-free across the await**: resolve the in-scope account under
  a brief session lock, drop it, await the user decision, then re-lock to sign.
  Avoids holding the session mutex across user-think-time.
- Scope is enforced *before* the prompt: a caller can't see or sign with an
  out-of-scope account (`Forbidden`).
- Deferred-but-spec'd methods return not-implemented rather than 404, so the gap
  is explicit to callers.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 28 passing tests, no clippy warnings.

**Fixed during the session**
- A failing test (`sign_typed_data_rejected_maps_to_113`) was a **test bug**:
  `AutoApprover` applies one decision to every prompt, so a reject also rejected
  pairing. Added `state_rejecting()` using a method-aware `ChannelApprover`
  responder (which also exercises the real channel path).

**Next / Resume**
- `wallet_addInvokeTransaction` (sign-only) — needs a multicall calldata encoder
  (krusty has none) + V3 hash. See `status.md` → Resume here.

**Notes / caveats**
- Agent-account creation is **in-memory only** this phase; persisting it
  re-seals the vault (app-layer flow).
- BIP-39 passphrase assumed empty for now.
- Transport hardening (Origin/Host checks, custom header) not yet added beyond
  loopback binding.

---

## 2026-06-09 — Phase 0 crypto core built & green

**Did**
- Verified the `krusty-kms` public API directly against source (cloned, read
  `crates/kms`), pinned the dependency at commit `1e36829`.
- Created the Cargo workspace and the `wallet-core` crate with modules:
  `domain`, `keys`, `vault`, `accounts`, `error` (see
  [`docs/code/wallet-core.md`](../code/wallet-core.md)).
- Implemented two-domain derivation (user `m/44'/9004'/0'/0/i`, agent
  `m/44'/9004'/0x41'/0/j`), OZ counterfactual address calc, Stark signing, an
  Argon2id→AES-256-GCM vault, and an account registry with per-caller scoping.
- Wrote 15 tests (`tests/derivation.rs`, `tests/vault.rs`); all green; clippy
  clean.

**Decisions**
- Vault encryption uses mainstream audited crates (`argon2`, `aes-gcm`) rather
  than krusty's internal XChaCha20-Poly1305, to keep the at-rest format
  reviewable and independent of experimental code. krusty is used only for
  derivation/signing/address.
- `Felt` is `Copy` and can't be `Zeroize`d in place; private keys are kept
  short-lived plain `Felt`s for now. Noted krusty's `SecretFelt` as the future
  path for stronger memory hygiene (spec §5.2).
- Tests assert **structural** properties (determinism, domain isolation,
  well-formedness), not cross-wallet golden addresses — those need the
  portability test plan run under review.

**Verify**
```bash
cargo test && cargo clippy --all-targets
```
Expect 15 passing tests, no clippy warnings.

**Next / Resume**
- Build the `wallet-rpc` crate (Phase 1 second half). See `status.md` → Resume
  here.

**Notes / caveats**
- `krusty-kms` is experimental; not for Mainnet/real funds until audited.
- Portability + agent-isolation claims remain **unverified** until
  `spec/portability-test-plan.md` is executed.

---

## 2026-06-09 — Specification & test plan

**Did**
- Interviewed and produced the technical spec
  ([`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md)):
  architecture, security model, two-domain account model, full `wallet_rpc.json`
  method scope + `companion_*` extensions, sign-vs-broadcast semantics, phasing.
- Researched the `krusty-kms` API and Starknet derivation conventions; resolved
  the open derivation-path questions.
- Wrote the derivation portability test plan
  ([`spec/portability-test-plan.md`](../../spec/portability-test-plan.md)).

**Decisions** — captured in spec §2 (key decisions table). Notably: Tauri/Rust,
encrypted-vault+passphrase, loopback JSON-RPC, pairing+per-request approval,
sign-only default with opt-in broadcast, OZ accounts, two derivation domains.

**Next / Resume** — implement Phase 0 (crypto core). _(done in the entry above)_

---

## Entry template

Copy this for each new entry; place it directly under the divider below the
title, above the previous newest entry.

```markdown
## YYYY-MM-DD — <short title>

**Did**
- <what changed, concretely>

**Decisions**
- <non-obvious choices and why; link spec sections>

**Verify**
- <exact commands to confirm this state, + expected result>

**Next / Resume**
- <the next concrete action; keep in sync with status.md "Resume here">

**Notes / caveats**
- <anything the next person must know; security flags>
```
