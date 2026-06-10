# Documentation Index

The complete map of project documentation. Every doc file must be listed here —
if it isn't in this index, it doesn't exist as far as navigation is concerned
(see the [contribution rules](../README.md#contributing-to-the-documentation)).

## Design (the "why" and "what")

Lives in [`spec/`](../spec/). Authoritative for design decisions.

- [`spec/wallet-companion-spec.md`](../spec/wallet-companion-spec.md) — full
  technical spec: architecture, security model, account model, service API,
  phasing, risks.
- [`spec/portability-test-plan.md`](../spec/portability-test-plan.md) —
  derivation portability / agent-isolation test plan (run under security review).

## Code (the "how")

Lives in [`docs/code/`](./code/). Describes the implementation.

- [`code/architecture.md`](./code/architecture.md) — workspace layout, crates,
  the security boundary, dependencies.
- [`code/wallet-core.md`](./code/wallet-core.md) — reference for the
  `wallet-core` crate (derivation, signing, vault, registry).
- [`code/wallet-rpc.md`](./code/wallet-rpc.md) — reference for the `wallet-rpc`
  crate (JSON-RPC service, auth, approval broker, handlers).
- [`code/desktop.md`](./code/desktop.md) — reference for the `desktop` Tauri app
  (menu-bar shell, onboarding/unlock UI, approval bridge).

## Project management (the "where are we" and "how we work")

Lives in [`docs/project/`](./project/).

- [`project/status.md`](./project/status.md) — current state, phase board, and
  the **Resume here** pointer. Start here each session.
- [`project/progress-log.md`](./project/progress-log.md) — append-only history.
- [`project/workflow.md`](./project/workflow.md) — prescriptive process: how to
  log progress, pick up work, and the security gates.
- [`project/backlog.md`](./project/backlog.md) — deferred work (e.g. fund sweep,
  Phase 3) with context to pick it up later.

## Quick routes

| I want to… | Go to |
|---|---|
| Understand the product & design | spec/wallet-companion-spec.md |
| Understand the code | code/architecture.md → code/wallet-core.md |
| Know what's done / what's next | project/status.md |
| Resume development | project/status.md (Resume here) → latest progress-log entry |
| Learn how to contribute docs | root README → Contributing to the documentation |
