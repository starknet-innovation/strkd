# `desktop` — Reference (Tauri app shell)

The macOS menu-bar app that hosts the wallet. It starts the loopback JSON-RPC
service, surfaces the service's approval prompts as menu-bar confirmation
dialogs, and gives the user a UI for onboarding, unlock, accounts, and the
request log.

- **Stack:** Tauri 2 + React 18 + TypeScript + Vite (mirrors `../dinner`).
- **Source:** `desktop/` (frontend) and `desktop/src-tauri/` (Rust shell).
- **Design rationale:** spec §3 (architecture), §8 (confirmation UX).

> ⚠️ **Verification boundary.** Unlike the headless crates, the GUI is **not**
> covered by automated tests. What's verified: the Rust shell **compiles**
> (`cargo build`), the frontend **type-checks + builds** (`tsc && vite build`),
> and clippy is clean. What's **not** verified by us: that the tray appears, the
> window shows, and the onboarding/unlock/approval flows behave at runtime —
> those require **running the app** on a machine with a display (see
> [`/verify`](../../README.md) / "run the app"). Treat this layer as
> *built, not yet run-verified*.

## Layout

```
desktop/
├── package.json, vite.config.ts, tsconfig*.json, index.html
├── app-icon.png              # source icon (icons/ generated from it via `tauri icon`)
├── src/                      # React frontend
│   ├── main.tsx, App.tsx, api.ts, styles.css
│   └── components/{Onboarding,Unlock,Accounts,ActivityLog,Agents,Connect,Settings,ApprovalDialog}.tsx
└── src-tauri/                # Rust shell (own crate, excluded from root workspace)
    ├── Cargo.toml, build.rs, tauri.conf.json
    ├── capabilities/default.json
    ├── icons/                # generated
    └── src/{main.rs, lib.rs, settings.rs}
```

## Rust shell (`src-tauri/src/lib.rs`)

Reuses `wallet-core` + `wallet-rpc` by path. On `setup`:

1. macOS `Regular` activation policy — the strkd icon shows in the Dock; a
   `RunEvent::Reopen` handler re-shows the window when the Dock icon is clicked.
2. Resolve the data dir; open the file-backed `RequestLog` and `VaultStore`
   (`vault.bin`, `requests.db`, `port.lock` — spec §10).
3. Build a `ServerState` with a `ChannelApprover` + file log + vault store, a
   **locked** session (onboarding/unlock fills it).
4. Start the loopback service (`bind_loopback` → binds `127.0.0.1`, writes
   `port.lock`).
5. Spawn the **approval bridge** (below).
6. Build the tray (Open / Quit); closing the window hides it.

### Approval bridge

The defining menu-bar feature. `wallet-rpc`'s `ChannelApprover` yields
`PendingApproval`s (each carries only public info + a `oneshot` responder). The
bridge task drains them, assigns an id, stores the responder in a shared map,
and emits an `approval-request` event to the frontend. The frontend shows
`ApprovalDialog` and calls `respond_approval(id, approved)`, which resolves the
`oneshot` the service is blocked on. Unanswered prompts **auto-reject after 60s**
(spec §8). Key material never enters the prompt.

**Non-invasive notification (no focus stealing):** on a new request the bridge
does **not** raise the window. It emits the event and updates three pending
indicators via `refresh_tray` (cleared on response/timeout): the **tray icon**
gets a red-dot overlay + "N pending" tooltip, and the **Dock tile** gets a
**badge count** (`WebviewWindow::set_badge_count`, macOS — needs the `Regular`
activation policy so a Dock tile exists). The frontend (`notify.ts`) also posts a
menu-bar **notification with Approve / Deny buttons**
(`@tauri-apps/plugin-notification`: `registerActionTypes` + `onAction` →
`respond_approval` on the front-of-queue request).

Indicator reliability, in order: the in-app **`ApprovalDialog`** is the
ground-truth prompt; the **Dock badge** + **tray red-dot** always reflect pending
count; the **OS notification** is best-effort — it needs notification permission
(System Settings → Notifications → strkd) and is most reliable from the
**installed, bundled app**, not `tauri dev`. All of this is **GUI, not
run-verified**.

> **No indicator at all is usually correct, not a bug.** A prompt (and therefore
> every indicator) only fires when an operation actually needs approval. Two
> recent features legitimately suppress it: (1) if the wallet has **auto-locked**,
> incoming requests are rejected with `-32001 LOCKED` *before* prompting; (2) if
> the caller holds an **active grant**, its own-account ops **auto-approve** with
> no prompt (funding still prompts). Either way the **Activity tab** records what
> happened (`error -32001` vs `approved`) — that's the place to diagnose "why
> didn't I get prompted?".

**Auto-lock.** A background task locks the wallet after `auto_lock_minutes` of
inactivity (Settings; default 15, **0 = never**). "Activity" is any RPC request
or an unlock (`ServerState::touch_activity`), so a busy granted agent keeps the
session alive while a truly idle wallet locks; the desktop's own status-polling
doesn't count. For fully-unattended agents, set 0.

### Icons

All icons derive from one source, `app-icon.png` (a Starknet-palette themed mark
— navy + salmon — **not** the official trademarked logo; drop in the real PNG
and re-run `npm run tauri icon app-icon.png` to swap). `tauri icon` regenerates
the app/Dock/window set **and** `icons/128x128.png`, which the **tray** also uses
(`include_bytes!`), so changing the source updates the menu-bar icon too. The
pending **red-dot** variant is overlaid on that same base **at runtime** (via the
`image` crate → `Image::new_owned`), so it always matches the current icon. Needs
tauri's `image-png` feature.

### IPC commands

| Command | Purpose |
|---|---|
| `status` | locked / needs_onboarding / network / version / service_url / account count |
| `generate(word_count)` | generate + stage a mnemonic, return it for one-time backup |
| `import(phrase)` | validate + stage an existing mnemonic |
| `finalize_setup(passphrase)` | encrypt staged mnemonic → write vault → unlock |
| `unlock(passphrase)` / `lock` | open/close the vault |
| `set_network(network)` | switch the active network (`"mainnet"`/`"testnet"`) — drives deploy/fund/sign + deploy-status |
| `get_settings` / `set_settings` | read/write RPC URLs + `auto_lock_minutes` (`config.json`); `set_settings` rebuilds the node at runtime |
| `list_clients` / `grant_permission(days)` / `revoke_permission` | Agents-tab permission control |
| `list_accounts` | registry accounts (empty while locked) |
| `add_user_account(label)` | derive next user account, persist (rollback on failure); **logged** to Activity |
| `balance(address)` | STRK balance in fri (string; `null` if no node) — works for undeployed accounts |
| `deploy_status(address)` | `Some(true/false)` if a node is set, else `None` |
| `deploy_account(address)` | already-deployed pre-check → estimate → sign DEPLOY_ACCOUNT → broadcast (needs node + a funded account); **logged** to Activity |
| `recent_log(limit)` | recent request-log entries |
| `list_clients` | paired clients + grant status |
| `grant_permission(client_id, days)` | grant auto-approval (clamped 1–90 days) |
| `revoke_permission(client_id)` | revoke a grant immediately |
| `respond_approval(id, approved)` | resolve a pending approval prompt |

## Frontend (`src/`)

`api.ts` wraps the IPC commands (`invoke`) and the `approval-request` event
listener. `App.tsx` routes by `status`: **onboarding** (no vault) → **unlock**
(locked) → **main** (Accounts / Activity / Agents / Connect / Settings tabs),
with a global `ApprovalDialog` shown whenever a prompt is queued. The **Agents**
tab (`Agents.tsx`) lists paired clients and grants/revokes a **time-bounded
auto-approval** window (1/2/3 months) per agent — while active, that agent's
own-account ops skip the prompt (funding still prompts). Shows remaining time;
revocable. A `useZoom` hook wires
**⌘/Ctrl +, −, 0** to scale the whole UI (persisted in `localStorage`); the base
font is 14px. The Accounts tab has **Testnet / Mainnet subtabs** that switch the
active network (`set_network`) — accounts (keys/addresses) are identical on both
networks (krusty's OZ class hash matches across them), so the same list shows
under each subtab; what changes is **deployment status** and where operations
run. The **default network is Sepolia (testnet)** (`ChainId::Sepolia`, hardcoded
at startup; not persisted across restarts yet). Each row shows its **STRK
balance** (when a node is configured) and a **copy-address** button, and — for
accounts the node reports **undeployed** on the active network — a **Deploy**
button (estimate → sign DEPLOY_ACCOUNT → broadcast; needs a node + a funded
account). Deploy UX guards against the common foot-guns: clicking **Deploy on a
zero-balance account wiggles** the button and explains (it pays its own fee)
rather than failing on-chain; after a successful broadcast the button stays in a
**"Deploying…" pending** state and **polls** until the node confirms deployment,
so it never reverts to "Deploy" and lets you submit a second nonce-conflicting tx
(the already-deployed pre-check in `deploy_account` is the backstop). Node-derived
data (balance + deployment status) refreshes **every ~10 s** (and on network
switch / after deploy); each fetch hits the node fresh at the `latest` block.

The **Connect** tab (`Connect.tsx`) shows the local endpoint and a
**copy-paste prompt** for the user to hand their agent — it points the agent at
`GET <url>/` for full usage (which the service serves from `wallet-rpc`'s
`usage.rs`). Mirrors the equivalent feature in `../dinner`.

The **Settings** tab (`Settings.tsx` + `settings.rs`) is the control panel for
the Starknet RPC endpoints (per network) that power broadcasting + fee
estimation. They persist to `config.json` and `set_settings` rebuilds the node
**at runtime via `ServerState::set_node`** — no restart. The URLs are IPC-only
(they may embed an API key) and never exposed over the wallet's HTTP service.

## Build / run

Two modes:

```bash
cd desktop
npm install

# Dev (hot reload). Runs Vite + the app as child processes; Ctrl-C can leave
# them around, so this is for active development, not daily use.
npm run tauri dev

# Build + install (recommended for actual use). Produces a real bundle:
npm run tauri build
#   → src-tauri/target/release/bundle/macos/strkd.app   (drag to /Applications)
#   → src-tauri/target/release/bundle/dmg/strkd_<ver>_<arch>.dmg
```

Launch the installed `strkd.app` like any app — its icon shows in the Dock and
it also lives in the menu bar (the tray); closing the window only hides it, and
clicking the Dock icon re-shows it (`Regular` activation policy + `Reopen`
handler). **Quit via the tray → Quit** (`app.exit(0)`), which
shuts down cleanly and releases the loopback port. The bound port is ephemeral
and `port.lock` is rewritten on each launch, so a stale lock self-heals; there's
no lingering-port problem the way an un-reaped `tauri dev` child can cause.

`npm run build` (tsc + vite) type-checks the frontend and produces `dist/`,
which the Rust `cargo build` needs (`generate_context!` reads `frontendDist`).
Icons are generated from `app-icon.png` via `npm run tauri icon app-icon.png`.

## Known gaps / follow-ups

- **Not run-verified** (see boundary above) — needs a human to launch and click
  through. This is the natural place to use the `/verify` or run-the-app flow.
- Default network is **Sepolia**, hardcoded; network switching is Phase 2.
- The app-icon is a generated placeholder, not final art.
- BIP-39 passphrase (distinct from the vault passphrase) is not surfaced in
  onboarding yet.
- No `companion_unlock` over RPC — unlock is UI-only (by design for now).
