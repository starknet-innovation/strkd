# Bramble Convergence — Plan

**Status:** Decisions settled 2026-09-10; execution not started
**Relates to:** [`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md) §6 (account model), §14 (risks),
[`spec/portability-test-plan.md`](../../spec/portability-test-plan.md), [`backlog.md`](./backlog.md)

[Bramble](https://github.com/starknet-innovation/mc-wallet) is the Starknet browser-extension
wallet built by the same team. This plan records what the two projects already share, what
diverges, and the decisions taken on 2026-09-10 about closing the gap.

---

## 1. Direction

**strkd stays a separate implementation and converges on conventions.** It keeps its Rust wallet
core and native signing path; it aligns with bramble on the account model, the crypto dependency,
and the visual language.

**Scope this cycle is narrow — agent companion plus on-device proving — but architected to grow.**
Where a choice makes a future general-purpose desktop wallet cheaper without costing much now, take
it. That is the reasoning behind the account-contract seam (§5.2) landing before Argent needs it.

The two products are not competing. An MV3 extension cannot ship a bundled on-device prover or
expose a loopback service to local agents; strkd can and does. Bramble has no agent, MCP, or
loopback surface anywhere in its tree, and declares its platform as web. The durable split is
**strkd = the local/heavy/agent tier, bramble = the browser tier, one account model underneath.**

## 2. What already matches

Verified by reading both codebases on 2026-09-10. None of this was coordinated — it falls out of
both wallets depending on `krusty-kms`.

| Area | Finding |
|---|---|
| Key derivation | Bit-identical. Both derive `m/44'/9004'/account'/0/index` through the same krusty function with `STARKNET_COIN_TYPE = 9004`. Same mnemonic yields the same private keys. |
| Wallet API | Method-for-method identical: all 12 `wallet_*` methods, both targeting wallet API `0.10.4-rc.0`. strkd adds `companion_*` on top; bramble adds nothing outside the standard. |
| OZ account class | Both resolve to OpenZeppelin `AccountUpgradeable` 3.0 `0x01d1777d…` with constructor calldata `[public_key]`. |
| Argent class hash | krusty's pinned Argent 0.4 class hash already equals bramble's. |

## 3. What diverges

| # | Divergence | strkd | bramble |
|---|---|---|---|
| D1 | OZ address salt | `SaltPolicy::PublicKey` | `"0x0"` |
| D2 | Multi-account axis | varies `addressIndex` | discovery varies `accountIndex` |
| D3 | Account contracts | OZ only, raw `[r, s]` signatures | OZ + Argent 0.4 + Argent Multisig 0.2 |
| D4 | Privacy backend | Tongo (own implementation) | official Starknet Privacy SDK, canonical pool |
| D5 | krusty pin | git rev from 2026-07-06 | published npm `0.10.0` |
| D6 | Design system | ad-hoc CSS custom properties | tokens plus swappable skins |
| D7 | Vault KDF | Argon2id | PBKDF2-SHA256 |

D7 is **not** being closed: PBKDF2 is a WebCrypto constraint on bramble's side, strkd's KDF is the
stronger one, and vault files never need to be interchangeable — migration between wallets is by
recovery phrase.

### 3.1 D1: bramble pins zero deliberately; strkd chose the public key

> **Corrected 2026-09-11.** This section previously blamed the divergence on krusty shipping two
> OZ address entry points with different implicit salt defaults. That was wrong — written from a
> local checkout pinned at `b673e81` (2026-07-06). Commit `4639de5` (2026-08-10) changed the
> default to the public key; `main` and tags `v0.5.3`–`v0.11.0` all read
> `salt.unwrap_or(public_key)`, and the WASM docs say so. `krusty-kms#138` is closed as invalid.
> The decision below is unchanged; its justification is not.

Bramble passes `"0x0"` **explicitly** (`packages/platform/vault/src/accounts/account-contract.ts:155`),
so its addresses are salt-0 regardless of any library default. That argument was added in commit
`70acb4b6` (2026-08-28) when bramble adopted the published `0.10.0` package, replacing a call that
omitted the salt — the commit message says it is there to *"preserve the wallet established
OpenZeppelin and scoped STRK20 derivation vectors"*. In other words, bramble pinned the old
zero-salt behaviour so addresses it had already deployed survived krusty's default change. That is
what a wallet with funds on mainnet should do.

strkd passed `SaltPolicy::PublicKey` explicitly all along. So both wallets made deliberate,
defensible choices and simply made different ones; neither inherited an accident.

Note the consequence: aligning strkd to zero means **deliberately overriding krusty's current
default**, which moved to the public key inside a broad security-hardening PR. That is safe for
OpenZeppelin (see below) but makes the caveat load-bearing rather than incidental.

**Address squatting does not apply here.** The Starknet address commits to the constructor
calldata, and OZ's constructor takes `[public_key]`, so the key is inside the address preimage
whatever the salt. Substituting an attacker's key changes the address. Independently, both wallets
use `deployer_address = 0`, so deployment goes through `DEPLOY_ACCOUNT`, which runs
`__validate_deploy__` and cannot be submitted without the account's own signature. Depositing to an
undeployed salt-0 OZ address is safe.

The attack **is** real for classes whose constructor does not bind the owner, and for UDC
deployments with `unique = false`. If strkd ever adopts such a class, revisit the salt policy with
that in mind.

## 4. Decision record — 2026-09-10

| # | Decision | Rationale |
|---|---|---|
| A | Narrow now, general later | Agent focus this cycle; architectural choices keep a general wallet cheap to grow into. |
| B | Converge, stay separate | Keeps the native Rust signing path and the trust model in [spec §14](../../spec/wallet-companion-spec.md). Accepts duplicated protocol logic. |
| C | strkd adopts salt `0x0` | Security-neutral for OZ (§3.1), so cost decides: bramble holds real mainnet funds at salt-0 addresses and pins that explicitly, strkd was alpha with one user. |
| D | `accountIndex` primary, `addressIndex` kept | Matches bramble's recovery scan so a seed import surfaces the same accounts; `addressIndex` stays available underneath. |
| E | Agent branch moves `0x41` → `0x41474E54` | Decision D makes user accounts walk the same axis the agent branch sits on; `0x41` (65) is reachable by ordinary use. |
| F | Fix krusty release provenance, then pin | "Match bramble exactly" is not currently verifiable (§6.1). |
| G | Account-contract seam now, Argent later | The seam is cheap now and expensive to retrofit through the signing path. |
| H | Adopt tokens, skins, and component parity | The two wallets should read as one product. |
| I | Park privacy entirely | Tongo cannot express the standard STRK20 surface; see §5.6. |
| J | Sweep first, then hard reset | Recovers assets before the derivation change strands them. |
| K | strkd imports bramble's vectors | One-directional conformance; no bramble-side change required. |

## 5. Work

Phases are ordered by dependency. **Phase 1 must complete before Phase 2 touches derivation** —
that is the whole point of the sweep.

### 5.1 Phase 1 — Recover assets (blocking)

A temporary, user-initiated action that consolidates ERC-20 balances from every derivable account
into a single destination address, on both Sepolia and mainnet. It enumerates accounts, deploys
undeployed accounts that hold value, funds accounts that lack gas, then transfers.

This extends the narrower agent-sweep already sketched in [`backlog.md`](./backlog.md), which
covered one account's STRK back to the manager.

- **Temporary by construction.** Ships with its removal tracked; it is deleted after the cutover.
- **Highest-risk control in the wallet.** Destination shown in full, explicit confirmation, and a
  security review **before** it is pointed at mainnet — not after.
- **ERC-20 only.** No NFTs, no shielded balances.
- **Pre-condition on the cutover:** confirm no shielded Tongo balances remain. Private balances
  derive from the same seed under coin type 5454, so the Phase 2 derivation change makes anything
  still shielded unreachable. Unshield before, not after.

### 5.2 Phase 2 — Account-contract seam

A trait in `wallet-core` owning class hash, constructor calldata, salt policy, and **signature
serialization**, with OpenZeppelin as the only implementation. No behaviour change; pure refactor.

The signature-serialization split is the part that matters: strkd returns raw `[r, s]` everywhere,
which is correct for OZ and rejected by Argent, which needs `[1, 0, pubkey, r, s]`. Introducing that
seam later means threading it back through the whole signing path.

### 5.3 Phase 3 — Adopt bramble's address conventions

One coordinated breaking change, shipped in a single release:

- `SaltPolicy::PublicKey` → `SaltPolicy::Zero`.
- Primary account axis becomes `accountIndex`; account *N* is `m/44'/9004'/N'/0/0`.
- Agent branch becomes `m/44'/9004'/0x41474E54'/0/j` — one reserved account index holding all agent
  accounts, enumerated by `addressIndex`.

`0x41474E54` is `"AGNT"` in ASCII, decimal 1,095,192,148, well inside the hardened cap
`0x7FFFFFFF`. Self-documenting in hex and unreachable by realistic account enumeration.

Every existing strkd address changes. Per decision J there is no migration: bump the vault version,
refuse old vaults with a clear message, document the break in the spec and the progress log.

### 5.4 Phase 4 — Conformance vectors

Import bramble's derivation and address test fixtures and assert against them in strkd's CI, so the
conventions above are locked by a failing test rather than by prose.

This is the mechanism that addresses the actual risk of decision B. Both crypto defects strkd has
hit — the SNIP-12 digest (#7) and the declare class hash (#9) — were drift bugs found in
production. Convergence without a conformance gate only resets that clock.

One-directional: nothing here catches bramble drifting from strkd.

### 5.5 Phase 5 — Design system

Port bramble's `SPACE` / `RADIUS` / `FONT` / `TYPE` / `MOTION` scales and the `midnight` / `daylight`
skins into strkd as CSS custom properties, replacing the current ad-hoc palette, then rebuild
strkd's components to match bramble's control vocabulary.

Component *code* cannot be shared: bramble is framework-free HTML-string renderers, strkd is React.
The tokens port cleanly; the components are a reimplementation against the same contract.

### 5.6 Privacy — parked

Phase 3 / Tongo work is shelved and privacy leaves strkd's scope for now. PR #11 is not merged.

The reasoning is in strkd's own `strk20.rs` header: Tongo is a per-token encrypted balance with five
operations, while the spec models a note-based pool, so `invoke`, `subaccount_invoke` and `OPEN`
amounts are parsed and rejected and `wallet_strk20SubaccountCommitment` stays `-32601`. Bramble
implements that surface properly against the canonical mainnet pool through the official SDK.

Two independent privacy systems are not interoperable in any case — different protocols, different
contracts, value shielded in one is unreachable from the other. Shipping strkd's partial
implementation on the *standard method names* would additionally mean the same call means different
things in the two wallets. Parking avoids that without discarding the work.

## 6. Dependencies outside strkd

### 6.1 krusty — release provenance (blocks decision F)

`publish-npm.yml` triggers on pushes to `main`, not tags. Neither npm `0.10.0` nor `0.11.0` records
a `gitHead`, and the git tags do not correspond to the publishes: tag `v0.10.0` points at commit
`5ae1d98`, dated 2026-09-02, five days *after* npm 0.10.0 was published, and carries an Argent
calldata fix (#124) that the published package therefore cannot contain.

There is no public way to determine which commit produced the WASM package bramble pins. Bramble's
integrity-digest pin guarantees byte-stability for them but does not identify the source.

**Nobody can currently audit which cryptography the released wallet is running.** That is the more
consequential of the two krusty items. strkd holds its current pin until there is a release both
wallets can name.

### 6.2 krusty — inconsistent OZ salt default *(withdrawn)*

Filed as `krusty-kms#138` and **closed as invalid on 2026-09-11**: krusty's two OZ address entry
points already agree, both defaulting to the public key since `4639de5` (2026-08-10), and the WASM
docs are accurate. The report came from a stale local checkout. Nothing is owed by krusty here.

### 6.3 krusty — Argent constructor calldata

`ArgentAccount` built `[0, pubkey, 0]`; the correct encoding is `[0, pubkey, 1]` (`1` is Cairo's
`Option::None` for the guardian). Bramble computes this itself in TypeScript, pinned against the
vendored upstream Argent release, and does not use krusty's version. krusty deleted `ArgentAccount`
entirely in v0.11.0.

Recorded so the Argent work in §5.2 does not reintroduce it from krusty. No action needed unless
krusty restores the type.

### 6.4 bramble — reserve the agent index

Add `0x41474E54` as a reserved account index on bramble's side so the agent-branch separation is
enforced rather than assumed. Bramble scans `accountIndex` 0–19 by default but permits manual
indices up to `0x7FFFFFFF`, so nothing currently prevents a collision.

This is the only bramble-side code change in the plan.

## 7. Issue map

| Issue | Phase | Blocked by |
|---|---|---|
| [#14](https://github.com/starknet-innovation/strkd/issues/14) — temporary ERC-20 sweep | 1 | — |
| [#15](https://github.com/starknet-innovation/strkd/issues/15) — account-contract seam | 2 | — |
| [#16](https://github.com/starknet-innovation/strkd/issues/16) — adopt bramble's address conventions | 3 | #14, #15 |
| [#17](https://github.com/starknet-innovation/strkd/issues/17) — import bramble's vectors into CI | 4 | #16 |
| [#18](https://github.com/starknet-innovation/strkd/issues/18) — design tokens and skins | 5 | — |
| [#19](https://github.com/starknet-innovation/strkd/issues/19) — component vocabulary | 5 | #18 |
| [#20](https://github.com/starknet-innovation/strkd/issues/20) — park Tongo / STRK20 | — | — |
| [#21](https://github.com/starknet-innovation/strkd/issues/21) — re-pin krusty | — | krusty (§6.1 only; §6.2 withdrawn) |

Phases 1–4 are the critical path. Phase 5 and #20 run independently.

## 8. Documents this plan invalidates

- **[`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md) §6** — the account model
  changes: salt policy, primary index axis, and the agent constant. §6.1's portability claim should
  be restated against bramble, which is the concrete wallet strkd must match.
- **[`spec/portability-test-plan.md`](../../spec/portability-test-plan.md)** — pins the exact paths
  and `SaltPolicy::PublicKey`; all of it moves. The plan's real target becomes bramble rather than
  Argent and Braavos.

That test plan stays **RED-data work**: it involves a mnemonic and derived keys, runs on an isolated
machine with test-only seeds, and is executed by a human under security review. Nothing in this plan
changes that.
