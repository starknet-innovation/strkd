# STRK20 (privacy pool)

strkd's support for the Starknet privacy pool, on wallet-API 0.10.4's
`wallet_strk20*` surface. The plan, decisions and phase-0 measurements are in
[`project/strk20-plan.md`](../project/strk20-plan.md); this page describes the code.

## Where it lives

| Piece | Path | What |
|---|---|---|
| Viewing key | `wallet-core::strk20_viewing_key` | Pool-scoped `user_sk`, bramble's derivation via krusty. Same seed → same notes in both wallets. |
| Owner key | `wallet-core::strk20_owner_viewing_key` | The `user_sk` strkd holds for an external owner contract, derived from a controller account (#48). strkd-only. |
| Owner records | `wallet-core::Strk20Owner`, `crates/wallet-rpc/src/strk20/owners.rs` | Enrolled owner contracts in the sealed registry, plus `companion_strk20AddOwner` / `ListOwners` / `RemoveOwner`. |
| Plans | `crates/wallet-rpc/src/strk20/plan.rs` | `companion_strk20Plan` / `DiscardPlan`, the SNIP-12 `Strk20Plan` hash (`plan_hash`), and the in-memory `PlanStore` (#48). |
| Protocol client | `crates/strk20` | Pure Rust port of the wallet side of StarkWare's SDK: hashes, decryption, client actions, the proof invocation, discovery, planner, `apply_actions`. No I/O. |
| RPC + desktop | `crates/wallet-rpc/src/strk20/` | The `wallet_strk20*` handlers and `companion_strk20Register`, the deposit provers (`remote.rs`), the AVNU private relay (`paymaster.rs`). |
| Desktop | `desktop/src/components/Privacy.tsx` | The Private tab; Tauri commands `strk20_*` run the same handlers as `Caller::Desktop`. |
| Settings | `prover::Strk20Config` | Per network: pool override, Starkscan key, deposit prover URL, AVNU key, paymaster URL. IPC-only, like the prover API key. |

## How an operation runs

1. **Discover** at the proving block (head − 12), after checking the account
   is deployed there (`starknet_getClassHashAt`); an account deployed in the
   last 12 blocks gets `-32006` "not deployed at reference block N", retry in
   about a minute, instead of a revert inside the prover. Then read registration, the user's
   channels to itself and each recipient, its outgoing-channel count, unspent
   notes. All through pool view calls on the wallet's node, decrypted locally
   (`strk20::discovery`), so no indexer sees `user_sk`.
2. **Plan** (`strk20::planner`): indices, note selection (largest first),
   change, the self-channel on register, phase order. A batch opening more than
   one channel is refused (`PRIVACY_LEAK`, 120).
3. **Approve**: one prompt naming the actions, who proves, and how it is
   submitted. The desktop user's click is the consent.
4. **Sign** the virtual invoke: sender = pool, calldata
   `compile_actions(user, user_sk, actions)`, the user's account key signing its
   V3 hash (`strk20::invocation`).
5. **Prove**:
   - register, transfer, withdraw → this machine, via
     `prover::prove_unrecorded` (the invocation holds `user_sk`, so nothing is
     written to proof storage);
   - deposit → a screening prover (`remote.rs`): the user's
     `deposit_prover_url`, else Starkscan's relay (user key, mainnet), else the
     bramble gateway (mainnet). It returns the screening attestation.
6. **Submit** `apply_actions(server_actions, attestation?)`:
   - register and deposits: from the user's account, with the ERC-20 approvals
     and the pool fee, fee estimated with the proof attached;
   - transfers and withdrawals: the AVNU `sponsored_private` relay, whose fee
     is a withdraw added to the batch; without an AVNU key,
     `wallet_strk20PrepareInvoke` returns `{call, proof}` for submission from
     any account (`wallet_addInvokeTransaction` does it).

**Fees.** The pool charges a flat fee per `apply_actions` (`get_fee_amount`,
in STRK: 2.5 STRK on mainnet in October 2026, 0 on strkd's Sepolia test pool).
Submissions from the account approve it alongside the deposits; the AVNU relay
passes it on in its fee withdraw. A deposit is checked against the account's
public balance before the screening prover is called.

**Prover errors.** A local proving failure comes back as `163` (unknown error) with the
prover's own error, bounded to about 4,000 chars (head and tail kept, cut
between words) and scrubbed of the viewing key. When the virtual transaction
reverts, the message carries the full revert reason
(`prover::snip36::classify_prover_error`).

## External owners (#48, in progress)

A deployed contract that advertises SRC5 `is_custom_signature_valid`
(`0x2eb9…ded`) can own notes. The pool then asks the contract to approve each
spend (`assert_valid_signature`, check 1). The design is in
[`project/design-48-strk20-external-owner.md`](../project/design-48-strk20-external-owner.md).
Parts 1 and 2 of 3 are built (enrolment, then the plan step):

- **Owner key.** strkd signs
  `starknet_keccak("strkd.strk20.owner.v1:<chain_id>:<pool>:<owner>")` with
  the controller account's key (RFC 6979) and folds `Poseidon(r, s)`, using
  krusty's `sign_stark_hash` and `fold_strk20_viewing_key`. The domain tag
  keeps this key apart from the controller's own STRK20 key. Like that key, it
  is derived per call and never persisted, logged or returned.
- **Records.** `Strk20Owner { owner, chain, pool, controller, client_id, label }`
  is kept in `Registry.strk20_owners`, sealed and persisted like agent accounts.
  Scoping follows the account rule: an agent sees the owners it enrolled, and an
  App client or the desktop sees the user's.
- **Enrolment.** `companion_strk20AddOwner` runs these checks in order:
  1. The owner is deployed (`getClassHashAt`), else `OWNER_NOT_DEPLOYED`.
  2. `supports_interface(0x2eb9…ded)` returns `true`, else
     `OWNER_NO_CUSTOM_VALIDATION`. A revert also counts as unsupported.
  3. If the owner is already registered with the pool (`get_public_key`), the
     registered key is the derived one, else `OWNER_KEY_MISMATCH`.

  It then always prompts, even under a grant, and stores the record. The result
  includes the viewing public key and `registered`.
- **`Ctx`.** The STRK20 context has a subject: an account, or an enrolled owner
  with its controller. `owner_address` resolves an owner in scope (else
  `OWNER_NOT_ENROLLED`, `-32002`). Passing `owner_address` together with
  `account_address` is a 114. The existing `wallet_strk20*` and
  `companion_strk20Register` methods refuse owners with `-32601` for now.
- **Plan (part 2).** `companion_strk20Plan { owner_address | account_address,
  register? | actions?, reveal_actions?, ttl_secs?, chainId? }` runs the first
  half of the one-shot pipeline and stops before signing:
  1. Refuses deposits (`DEPOSIT_UNSUPPORTED`, `-32601`), checks the plan caps,
     and for `register: true` refuses an already registered subject
     (`ALREADY_REGISTERED`).
  2. `plan_at_reference` (shared with `prepare()`): `B = head − 12`, the
     subject is deployed at `B` (`OWNER_NOT_DEPLOYED` for an owner), an owner
     still advertises SRC5 at `B` (`OWNER_NO_CUSTOM_VALIDATION`), discovery at
     `B` and at `latest` must agree, then `planner::plan` with `OsRandomness`
     drawn once, and the `UserLinkage` check.
  3. `plan_hash` (below), one gated prompt (a grant covers it) naming the
     subject, the controller, the chain, the intent, the action count, the pool
     and, for an owner, that its contract must approve the plan on-chain first.
  4. Stores the plan and returns `plan_id`, `plan_version: 1`, `expires_at`
     (Unix ms), `plan_block`, `plan_hash`, `actions_hash`, `typed_data` and a
     summary (transfers, withdrawals, action count, notes spent, change, pool
     fee). Nothing is signed and nothing is proved.
- **`plan_hash`.** `actions_hash = poseidon_hash_many(serialize_actions(actions))`,
  which is `compile_actions` calldata from index 2 on (the `Span` length prefix
  included; `owner` and `user_sk` excluded). The typed data is
  `StarknetDomain { name: "strkd.Strk20Plan", version: "1", chainId, revision: "1" }`
  (all `shortstring`) and `Strk20Plan { pool: ContractAddress, owner:
  ContractAddress, actions_hash: felt }`, hashed with
  `typed_data_message_hash(json, owner)`. `"version": "1"` is the felt 1, not
  the shortstring `0x31`. `plan::plan_hash` is the only implementation; its
  unit test pins PR #53's vector (`plan_hash = 0x062a76d3…3301`), and
  `companion_typedDataHash` on the returned `typed_data` reproduces it.
- **`reveal_actions`.** A second prompt through `Caller::approve_always` (a
  grant never covers it) says the actions hold channel keys, which disclose
  every past and future payment on those channels, irrevocably, and cannot
  spend. Approved: the result gains `client_actions` (typed JSON) and
  `client_actions_felts`. Refused: the plan still comes back, with
  `actions_revealed: false`. The request log replaces both fields with
  `"<redacted>"` (`dispatch::redact_for_log`).
- **Plan store.** `PlanStore` lives inside the unlocked `WalletSession`, so any
  lock (from RPC idle, the desktop, or tests) drops it, and a restart starts
  empty. Plans are keyed by a 128-bit random hex `plan_id` and bound to the
  client that made them; any other client gets `PLAN_NOT_FOUND` (114), the same
  as for an unknown, expired or discarded id. Caps: one live plan per (subject,
  chain, pool), 4 per client, 32 in all (`PLAN_LIMIT`, `-32006`). The TTL is
  1 h by default and 24 h at most; expired plans are swept on every access. A
  stored plan keeps the subject, controller, chain, pool, intent, actions,
  felts, `plan_hash`, `plan_block`, the discovered state with `user_sk`
  zeroed, the prover kind and submission route the prompt named, and the
  prompt. Dropping a plan overwrites its felts (best effort, no `unsafe`).
  `companion_strk20DiscardPlan { plan_id }` drops one without a prompt.
- **Accounts.** Plans work for strkd accounts too (no SRC5 check). For them
  `owner` is the account, and strkd will sign at prove time.
- **Not yet.** Prove (`companion_strk20Prove`), the prove-time checks,
  `submit: "none" | "account"` and client-bound, unrecorded proving jobs. These
  come in part 3.

## Verification

- `crates/strk20/tests/fixtures/strk20-vectors.json` is checked by the crate
  and re-derived by `conformance/strk20.mjs` (starknet.js only, in CI). The
  official SDK reproduced it too, invocation included.
- `wallet-core/tests/strk20.rs` matches the viewing keys live pools registered,
  and pins known-answer vectors for the owner key. The vectors come from the
  test seed only, and their message hashes were reproduced independently.
- Live on Sepolia, against strkd's test pool (mainnet class, test screener):
  `crates/strk20/examples/sepolia_e2e.rs` (crate only) and
  `crates/wallet-rpc/examples/strk20_live.rs` (the RPC handlers). Results are
  recorded in the plan.

## Not yet

- Open notes (`"OPEN"`), `invoke`, `shadow_account_invoke` and
  `wallet_strk20ShadowAccountCommitment` (`-32601`).
- Live runs of Starkscan, the bramble gateway and the AVNU relay: mainnet-only
  or key-gated.
- Deposits batched with private actions (AVNU's `invoke_and_apply_action`
  route).
