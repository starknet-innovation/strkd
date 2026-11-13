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
| Prove | `crates/wallet-rpc/src/strk20/prove.rs` | `companion_strk20Prove`: the prove-time checks, the caller's signature (owners) or strkd's (accounts), and the unrecorded proving job (#48). |
| Protocol client | `crates/strk20` | Pure Rust port of the wallet side of StarkWare's SDK: hashes, decryption, client actions, the proof invocation, discovery, planner, `apply_actions`. No I/O. |
| RPC + desktop | `crates/wallet-rpc/src/strk20/` | The `wallet_strk20*` handlers and `companion_strk20Register`, the deposit provers (`remote.rs`), the AVNU private relay (`paymaster.rs`). |
| Desktop | `desktop/src/components/Privacy.tsx` | The Private tab; Tauri commands `strk20_*` run the same handlers as `Caller::Desktop`. |
| Settings | `prover::Strk20Config` | Per network: pool override, Starkscan key, deposit prover URL, AVNU key, paymaster URL, and (testnet only) a test pool's screener key. IPC-only, like the prover API key. |
| Pool settings + local screener | `crates/wallet-rpc/src/strk20/screener.rs`, `strk20::screening` | Settings validation, the Settings → STRK20 pool check, and testnet deposit attestations signed with a self-deployed pool's screener key. |

## Which pool

Each network uses the pool set in Settings → STRK20, else its canonical pool
(`MAINNET_POOL`, `SEPOLIA_POOL`). `chain_and_pool()` resolves it once per
call, so every method (register, balances, prepare, invoke, and the #48 plan
and owner methods) reads, proves against and submits to the same pool, and the
viewing key is derived for that pool. Callers can't choose it: a
`pool_address` param only asserts the expected pool, and a mismatch is `114`
before anything runs (a paired agent can't point strkd at an arbitrary
contract). Approval prompts, including plan, prove and enrol, name the pool
and flag a non-canonical one as `NON-CANONICAL pool 0x…, set in Settings →
STRK20`. The local screener applies to the one-shot deposit methods only;
`companion_strk20Plan` still refuses deposits (`DEPOSIT_UNSUPPORTED`). The desktop's
**Check pool** reads the class hash (warns unless it is the known class
`0x6d163f2b…cf83`), `get_version`, `get_fee_amount` and
`get_screener_public_key` (`screener::pool_info`).

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
   - deposit → with a **testnet screener key** set (below), this machine;
     otherwise a screening prover (`remote.rs`): the user's
     `deposit_prover_url`, else Starkscan's relay (user key, mainnet), else the
     bramble gateway (mainnet). The remote prover returns the screening
     attestation.
6. **Submit** `apply_actions(server_actions, attestation?)`:
   - register and deposits: from the user's account, with the ERC-20 approvals
     and the pool fee, fee estimated with the proof attached;
   - transfers and withdrawals: the AVNU `sponsored_private` relay, whose fee
     is a withdraw added to the batch; without an AVNU key,
     `wallet_strk20PrepareInvoke` returns `{call, proof}` for submission from
     any account (`wallet_addInvokeTransaction` does it).

**Local screener (testnet only).** A pool's constructor takes its screener
public key; the pool accepts a deposit only with a `DepositorValidation`
attestation that key signed: SNIP-12 revision 1, domain `('Screening', 2,
chain_id, 1)`, the screener's public key in the account slot, message
`(depositor, issued_at)` (`packages/privacy/src/snip12.cairo`). The
attestation is not bound to the proof. For a pool you deployed yourself, set
its screener private key in Settings → STRK20 (Sepolia only): the deposit is
proved on this machine and strkd signs the attestation right after proving,
dated at the latest block's timestamp. `strk20::screening` reproduces
starknet-privacy's `fixtures/screening-vectors.json` exactly. Guards: refused
on mainnet when saving and at deposit time; checked against the pool's
`get_screener_public_key` before the prompt; never logged or returned (the
settings `Debug` redacts it, the desktop reads it back as `<stored>`). It is
stored unencrypted in the prover's `settings.json` (now `0600`), next to the
API keys: it is a test key for a test pool, not wallet key material. strkd's
Sepolia test pool `0x03016a46…6323` uses the public test key `0xCAFEBABE`.

**Fees.** The pool charges a flat fee per `apply_actions` (`get_fee_amount`,
in STRK: 2.5 STRK on mainnet in October 2026, 0 on strkd's Sepolia test pool).
Submissions from the account approve it alongside the deposits; the AVNU relay
passes it on in its fee withdraw. A deposit is checked against the account's
public balance before the screening prover is called.

**Prover errors.** A local proving failure comes back as `163` (unknown error)
with a `data.reason` (`PROVING_FAILED`, `INVALID_SIGNATURE`, `WIPED_ON_LOCK`) and
a fixed message picked by the prefix strkd itself wrote
(`strk20::safe_prover_error`): "the batch reverted…", "too large…", "RPC
throttled…", and so on. The prover's own text, and so the revert reason, is
never returned or logged: the virtual transaction carries the viewing key and
channel keys, and contract code it runs chooses the revert text (#48 review).
To debug a revert, run `snip36 prove virtual-os` by hand with a test seed.
Locking the wallet stops a local prove that is still running.

## External owners (#48)

A deployed contract that advertises SRC5 `is_custom_signature_valid`
(`0x2eb9…ded`) can own notes. The pool then asks the contract to approve each
spend (`assert_valid_signature`, check 1). The design is in
[`project/design-48-strk20-external-owner.md`](../project/design-48-strk20-external-owner.md).
All three parts of phase 1 are built (enrolment, the plan step, the prove step):

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
  includes the viewing public key and `registered`. The prompt says the
  contract's code is trusted with the owner's viewing key and the channel keys
  of every batch: the pool passes the whole `compile_actions` call to
  `is_custom_signature_valid` during virtual execution, so a malicious contract
  could leak them (in its revert text, which strkd withholds, or in its own
  storage writes and events, which stay in the virtual block). Enrol only
  contracts whose code you trust. The label keeps no control or Unicode format
  characters (bidi overrides and isolates, zero-width, BOM, tags:
  `dispatch::display_text`); pairing client names get the same treatment.
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
  as for an unknown, expired or discarded id. Caps: one live plan per (client,
  subject, chain, pool), 4 per client, 32 in all (`PLAN_LIMIT`, `-32006`). The
  one-plan cap is per client so one client can't block another (or learn that
  it holds a plan); two clients' plans for one subject race on the same notes,
  and the second to land fails (`PLAN_STALE`, or on-chain). The TTL is
  1 h by default and 24 h at most; expired plans are swept on every access. A
  stored plan keeps the subject, controller, chain, pool, intent, actions,
  felts, `plan_hash`, `plan_block`, the discovered state with `user_sk`
  zeroed, the prover (kind, plus the URL for a remote prover) and submission
  route the prompt named, and the
  prompt. Dropping a plan overwrites its felts (best effort, no `unsafe`).
  `companion_strk20DiscardPlan { plan_id }` drops one without a prompt.
- **Accounts.** Plans work for strkd accounts too (no SRC5 check). For them
  `owner` is the account, and strkd will sign at prove time.
- **Prove (part 3).** `companion_strk20Prove { plan_id, block_number?,
  signature?, submit?: "none" | "account", submitter_address? }`. Every check
  runs before anything is queued, so a bad request fails at once with a
  `data.reason`:
  1. Params: owners need `signature` (`SIGNATURE_REQUIRED`, 114; `[]` is
     allowed); accounts must not pass one (`SIGNATURE_NOT_ALLOWED`, 114).
     `submit: "relay"` is `RELAY_UNSUPPORTED` (`-32601`); `submitter_address`
     needs `submit: "account"`.
  2. The plan (`PlanStore::get`, client-bound, else `PLAN_NOT_FOUND`).
  3. The subject again, through `ctx()` in the caller's scope: an owner removed
     since, or re-enrolled with another controller, is `OWNER_NOT_ENROLLED`
     (`-32002`); a changed pool setting is `PLAN_STALE`.
  4. `B` (`reference_block`): default `head − 12`; `B ≤ head − 12`
     (`BLOCK_TOO_RECENT`, with the blocks to wait; also when the default is
     still below `plan_block`), `B ≥ plan_block` (`BLOCK_BEFORE_PLAN`), and
     `head − B < get_proof_validity_blocks − 120` (`BLOCK_TOO_OLD`). The owner's
     on-chain approval must be in state at `B`: wait ~12 blocks after it lands,
     or pass `block_number` at or after its block.
  5. Deployed at `B` (`OWNER_NOT_DEPLOYED`) and, for owners, SRC5 at `B`
     (`OWNER_NO_CUSTOM_VALIDATION`).
  6. Discovery at `B` and at `latest` with a fresh viewing key; both must equal
     the plan's snapshot (`same_state`, strict), else `PLAN_STALE` and the plan
     is dropped. `serialize_actions(actions)` must equal the stored felts and
     re-hash to `plan_hash`, else `PLAN_HASH_MISMATCH` (163, cannot happen
     through the API).
  7. A prompt only if the prover differs from the plan's (kind, or a remote
     prover's URL: `prover_identity`) or
     `submit: "account"` (§5.6). The account prompt names the submitter, the
     pool fee it pays, and the public link it makes to the owner.
  8. The pool nonce at `B`, `ProofInvocation::new` from the stored actions,
     then the signature: an owner's is the caller's felts verbatim (strkd never
     calls `sign_hash_for` for an owner); an account's is
     `sign_hash_for(account, invocation.hash())`. The plan is taken (consumed)
     in the same session lock.
  9. `prover::enqueue_prove_unrecorded` queues the job, owned by the client,
     and returns `{ job_id, status: "queued", plan_id, plan_hash,
     plan_created_at, block_number, subject, submit, submitter_address? }`.

  The job's `finish` step parses the proof (`remote::parse_result`), builds
  `apply_actions`, and either returns `{ call, proof }` (the same shape as
  `wallet_strk20PrepareInvoke`) or, for `submit: "account"`, submits through
  `submit_from` from the submitter (default: the owner's controller, or the
  account itself) and returns `{ transaction_hash, submitter_address, call }`.
  It does not submit if the wallet locked while the job ran: the lock cancels
  the prove (the native prover's process group is killed and its temp dir
  removed), and a job already submitting re-checks, under the session lock
  right before signing and again before broadcasting, that it was not wiped and
  that the submitter is still in the client's scope (`SubmitGuard`:
  `WIPED_ON_LOCK`, `SUBMITTER_NOT_IN_SCOPE`). Errors carry `error_reason`:
  `OWNER_SIGNATURE_REJECTED` (owner) or `INVALID_SIGNATURE` (account) when the
  prover's error shows the pool's `INVALID_SIGNATURE` (text or its hex short
  string), `PROVING_FAILED` otherwise, `WIPED_ON_LOCK`, or a submission
  reason. Prover errors become fixed text (see **Prover errors**); submission
  errors go through `scrub`. `companion_proveStatus` for a STRK20 job is
  logged without its `result` and `error` (`dispatch::redact_for_log`).
- **Submission from another account.** `submit_from_account` is now a wrapper
  around `submit_from(SubmitTarget, submitter, prepared, deposits)`, which owns
  its handles (session, node, chain, pool) so a background job can submit. The
  submitter signs and pays the gas and the pool's fee.

## Verification

- `crates/strk20/tests/fixtures/strk20-vectors.json` is checked by the crate
  and re-derived by `conformance/strk20.mjs` (starknet.js only, in CI). The
  official SDK reproduced it too, invocation included.
- `wallet-core/tests/strk20.rs` matches the viewing keys live pools registered,
  and pins known-answer vectors for the owner key. The vectors come from the
  test seed only, and their message hashes were reproduced independently.
- `wallet-core/tests/fixtures/strk20-plan-vector.json`: the SNIP-12
  `Strk20Plan` approval-hash vector for contract owners (#48, design note
  §5.3). Checked in Rust (`tests/strk20_plan_vector.rs`), starknet.js
  (`conformance/verify.mjs`) and Cairo/OpenZeppelin.
- Live on Sepolia, against strkd's test pool (mainnet class, test screener):
  `crates/strk20/examples/sepolia_e2e.rs` (crate only) and
  `crates/wallet-rpc/examples/strk20_live.rs` (the RPC handlers; its
  `prepare-deposit` mode proves a locally screened deposit and only estimates
  the submission). Results are recorded in the plan.
- `crates/strk20/tests/screening.rs` checks attestations against
  starknet-privacy's screening vectors; `crates/wallet-rpc/tests/strk20_pool.rs`
  covers pool selection, prompts, the `pool_address` assertion and the local
  screener end to end against a mock node.

## Contract owners (#48)

A note owner can be a contract that approves plans on-chain instead of an
account that signs: [`project/design-48-strk20-external-owner.md`](../project/design-48-strk20-external-owner.md).
The reference owner contract, with the approval check, the strict
`is_valid_signature` fallback, tests and sncast steps, is
[`contracts/strk20-owner-example`](../../contracts/strk20-owner-example/README.md).
Not audited. The strkd side (enrolment, plan/prove) is not built yet.

## Not yet

- Open notes (`"OPEN"`), `invoke`, `shadow_account_invoke` and
  `wallet_strk20ShadowAccountCommitment` (`-32601`).
- Live runs of Starkscan, the bramble gateway and the AVNU relay: mainnet-only
  or key-gated.
- Deposits batched with private actions (AVNU's `invoke_and_apply_action`
  route).
