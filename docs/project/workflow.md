# Development Workflow & Project Management

This file is **prescriptive**. It defines how we track work, log progress, and
hand off so anyone (or any future session) can pick up exactly where the last
one stopped. Follow it; don't invent a parallel process.

## The three project-management artifacts

| File | Mutability | Answers | Rule |
|---|---|---|---|
| [`status.md`](./status.md) | **Overwrite** — always reflects *now* | "Where are we? What's left?" | Single source of truth for current state. |
| [`progress-log.md`](./progress-log.md) | **Append-only** — never edit past entries | "What happened, and why?" | Immutable history. One entry per work session. |
| [`workflow.md`](./workflow.md) (this file) | Rarely changes | "How do we work?" | The process itself. |

The mental model: **`status.md` is the snapshot, `progress-log.md` is the
film.** Keep them in sync — every log entry that changes reality must be
reflected in a `status.md` edit in the same session.

The roadmap of *what* we're building (the phases) lives in the spec
([`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md) §13), not
here. `status.md` tracks our position against that roadmap; it does not redefine
it.

---

## How to pick up work (start of a session)

Do this in order, every time:

1. Read the [root README](../../README.md) if you're new to the repo.
2. Read [`status.md`](./status.md) — the **"Resume here"** section at the top
   tells you the next concrete action.
3. Read the **most recent** entry in [`progress-log.md`](./progress-log.md) for
   the *why* behind the current state and any caveats.
4. Open the relevant spec section (the status item links to it).
5. Verify the build is where the log says it is before changing anything:
   ```bash
   cargo test && cargo clippy --all-targets
   ```
   If that doesn't match the last log entry's claim, investigate before
   building on top.

## How to log progress (end of a session, or after a meaningful unit of work)

1. **Append** a new entry to [`progress-log.md`](./progress-log.md) using the
   template at the bottom of that file. Newest entry goes at the **top** of the
   log section. Never edit or delete older entries — correct them with a new
   entry instead.
2. **Update** [`status.md`](./status.md): move completed items to Done, adjust
   In Progress / Remaining, and rewrite the **"Resume here"** pointer so the
   next person's step 2 is unambiguous.
3. Make sure both changes ship together. A code change without a log entry is
   incomplete work.

## Definition of Done (per task)

A task is not "done" until **all** of these hold:

- [ ] Code compiles: `cargo build`.
- [ ] Tests pass: `cargo test` (add tests for new behavior).
- [ ] Lint clean: `cargo clippy --all-targets` (no warnings).
- [ ] Code documentation updated (see
      [docs contribution rules](../../README.md#contributing-to-the-documentation)).
- [ ] `status.md` updated and `progress-log.md` appended.
- [ ] Any security-sensitive change is flagged for human review (see below).

## Tasks, phases, and granularity

- The build is organized into **phases** (Phase 0–3 + Hardening) defined in
  spec §13. Don't reorder phases without updating the spec.
- Break a phase into **tasks** small enough to finish and verify in one sitting.
  Track them as checklist items in `status.md` under the active phase.
- One task → one logical change → one progress-log entry. Keep tasks shippable
  on their own.

## Security gates (non-negotiable)

This is a key-custody wallet. Some steps are **blocked on human review** and
must never be silently marked done by a contributor or an agent:

- **Only ever use throwaway/test BIP-39 seeds** in code, tests, and examples
  (the public all-zeros vector `abandon … about`). Never a real mnemonic, real
  private key, or any seed controlling funds.
- The **derivation portability claim** (user accounts importable into
  Argent/Braavos) and the **agent-isolation vector** require executing
  [`spec/portability-test-plan.md`](../../spec/portability-test-plan.md) on
  isolated infrastructure under security-team sign-off. Code passing structural
  tests is **not** sufficient to claim portability.
- **Mainnet** use is gated on an independent audit of the crypto path
  (`krusty-kms` is experimental). See spec §14.

When work touches these areas, the progress-log entry must say so explicitly and
the `status.md` item stays in a **Blocked (needs review)** state, not Done.

## Conventions

- Dates are absolute (`YYYY-MM-DD`), never "yesterday"/"last week".
- Link, don't duplicate: reference the spec/code docs rather than restating
  them, so there's one source of truth per fact.
- Keep `status.md` short enough to read in under a minute. Detail belongs in the
  log or the spec.
