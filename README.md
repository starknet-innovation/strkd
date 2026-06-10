# strkd — Starknet Wallet Companion

A desktop **menu-bar companion wallet** for Starknet. It stores a seed safely,
derives multiple accounts from it, and exposes a local service that other
programs on the same machine — AI agents, desktop apps — can call to request
wallet operations. The wallet never returns private keys; it returns signed
payloads (and can broadcast on request). Every signing or state-changing request
pops a menu-bar confirmation, and all requests are logged.

It speaks the standard Starknet
[`wallet_rpc.json`](https://github.com/starkware-libs/starknet-specs/blob/master/wallet-api/wallet_rpc.json)
API plus a small `companion_*` extension namespace, and delegates cryptography to
[`krusty-kms`](https://github.com/starknet-innovation/krusty-kms).

> ⚠️ **Experimental. Not for real funds.** `krusty-kms` is flagged experimental
> by its authors and the crypto path is unaudited. Use **throwaway test seeds
> only** — never a real mnemonic or a seed controlling funds. See the security
> notes in the [spec](./spec/wallet-companion-spec.md) §14.

## Status

All three layers are built. The crypto core (`wallet-core`) and the loopback
service (`wallet-rpc`) are green — **92 tests, clippy clean** — covering
derivation, signing, the vault, the full `wallet_*` + `companion_*` handlers,
auth, the approval broker, permission grants, and the request log. The Tauri
menu-bar app (`desktop/`) compiles and its frontend builds, but the **GUI is not
yet run-verified** (tray, dialogs, notifications, auto-lock, Dock icon need a
human launch). Broadcasting + fee estimation work against a node configured in
the Settings tab; Sepolia reads/estimates are live-verified, while the broadcast
hop still needs a funded-account submit. Next up: **Phase 3** (Tongo / STRK20).

For the authoritative, always-current state, see
**[`docs/project/status.md`](./docs/project/status.md)**.

## Build & test

The headless core (`wallet-core` + `wallet-rpc`):

```bash
cargo build
cargo test                  # core + service test suite
cargo clippy --all-targets  # lint
```

The desktop menu-bar app (**Tauri 2 + React + TypeScript + Vite**), under
`desktop/`:

```bash
cd desktop
npm install
npm run tauri dev           # dev (hot reload; child processes — for development)
npm run tauri build         # build an installable bundle (recommended for use)
#   → src-tauri/target/release/bundle/macos/strkd.app  (drag to /Applications)
```
Install the built `strkd.app` and launch it like any app — it shows in the Dock
and lives in the menu bar (the tray); closing the window only hides it, and
clicking the Dock icon re-shows it. Quit cleanly via the tray → Quit. Set the
Starknet RPC endpoint in the app's **Settings** tab to enable broadcasting + fee
estimation.

Requires a Rust toolchain (+ Node for the desktop app). The first core build
fetches `krusty-kms` from git (pinned commit), so it needs network access. The
desktop crate is its own Cargo crate, excluded from the root workspace, and
reuses the core crates by path.

> The headless core is covered by automated tests. The desktop GUI is **built
> but not run-verified** — confirming the tray/onboarding/approval flows means
> running the app on a machine with a display.

---

## Documentation

All documentation is indexed in **[`docs/index.md`](./docs/index.md)** — that is
the single entry point; start there if you're unsure where to look. The docs are
organized into three areas by the question they answer:

| Area | Location | Answers | Start with |
|---|---|---|---|
| **Design** | [`spec/`](./spec/) | *Why* and *what* we're building | [`spec/wallet-companion-spec.md`](./spec/wallet-companion-spec.md) |
| **Code** | [`docs/code/`](./docs/code/) | *How* the implementation works | [`docs/code/architecture.md`](./docs/code/architecture.md) (then `wallet-core.md`, `wallet-rpc.md`, `desktop.md`) |
| **Project** | [`docs/project/`](./docs/project/) | *Where we are* and *how we work* | [`docs/project/status.md`](./docs/project/status.md) |

### How to find what you need

- **"What is this and how is it designed?"** → the spec, starting at
  [`spec/wallet-companion-spec.md`](./spec/wallet-companion-spec.md).
- **"How does the code work / where is X implemented?"** →
  [`docs/code/architecture.md`](./docs/code/architecture.md) for the map, then
  the per-crate reference (e.g.
  [`docs/code/wallet-core.md`](./docs/code/wallet-core.md)).
- **"What's done, what's left, what should I do next?"** →
  [`docs/project/status.md`](./docs/project/status.md) (the **Resume here**
  section at the top).
- **"Why is the code in its current state?"** →
  [`docs/project/progress-log.md`](./docs/project/progress-log.md).

### Project management

How development is tracked and handed off — what's been done, what remains, how
to log progress, and how to pick up where the last session stopped — is governed
by **[`docs/project/workflow.md`](./docs/project/workflow.md)**. Read it before
doing project work; it is prescriptive. In short:

- [`status.md`](./docs/project/status.md) is the mutable **snapshot** of now.
- [`progress-log.md`](./docs/project/progress-log.md) is the append-only
  **history**.
- [`workflow.md`](./docs/project/workflow.md) is the **process** (and the
  security gates).

---

## Contributing to the documentation

Keep docs navigable by following these rules. They are not optional — drift here
makes the whole tree untrustworthy.

1. **Three homes, by purpose.** Design → [`spec/`](./spec/). Code → [`docs/code/`](./docs/code/).
   Project management → [`docs/project/`](./docs/project/). Put a doc where its
   *question* belongs, not where it's convenient.
2. **One topic per file; one fact in one place.** Don't restate the spec in a
   code doc or vice versa — **link** to the source of truth. Duplication is the
   main thing we're preventing.
3. **Index everything.** Every doc file must be listed in
   [`docs/index.md`](./docs/index.md). Adding a doc without indexing it is an
   incomplete change. When you add a whole new *area*, add a row to the
   Documentation table above too.
4. **One crate → one reference.** When a new crate is built, add
   `docs/code/<crate>.md` and link it from
   [`docs/code/architecture.md`](./docs/code/architecture.md) and the index.
5. **Naming & links.** Files are `kebab-case.md`. Use **relative** links between
   docs so they work on disk and in any viewer. Use absolute `YYYY-MM-DD` dates.
6. **Docs ship with the change.** Code changes update the relevant code doc in
   the same change; project state changes update `status.md` and append to
   `progress-log.md` (per [`workflow.md`](./docs/project/workflow.md)).
7. **Don't break the boundary in prose.** When documenting anything that touches
   keys, reinforce the security boundary; never include real seed/key material
   in an example.

If you're unsure where something goes, it almost always belongs in exactly one
of the three areas above — pick by the question it answers, then link it from the
index.
