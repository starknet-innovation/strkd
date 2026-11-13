# STRK20 for an external contract owner, with a plan/prove split — design note

**Status:** Design agreed 2026-11-13 (§2). Nothing is built.
**Issue:** [#48](https://github.com/starknet-innovation/strkd/issues/48)
**Relates to:** [`strk20-plan.md`](./strk20-plan.md), [`../code/strk20.md`](../code/strk20.md),
[`../code/wallet-rpc.md`](../code/wallet-rpc.md), and the #49 note on channel-key disclosure
(`design-49-strk20-channel-disclosure.md`, drafted in parallel).

Line references are to `main` at `c1163ee`. Pool references are to the privacy contracts vendored
in bramble (`bramble/vendor/starknet-privacy`, `b40bf10`, 2026-09-15). Whether the deployed pool
class has the same custom-validation path is still to be checked (§10).

---

## 1. Background

**The pool.**
- Every STRK20 operation is a virtual invoke whose sender is the pool. Its calldata is one call,
  `pool.compile_actions(user_addr, user_sk, client_actions)`
  (`crates/strk20/src/invocation.rs:35-56`).
- The pool's `__execute__` runs `compile_actions` and then
  `assert_valid_signature(user_addr, calls, tx_info)` (`privacy.cairo:195-212`). That function
  accepts the first of three checks that passes (`utils.cairo:371-420`):
  1. **Custom.** The owner advertises SRC5 id
     `0x2eb9faa4ce06e09879e93803798b87d22877b73964334356bf9e0d8cc22ded`
     (`selector!("is_custom_signature_valid")`, `utils.cairo:39`). Its
     `is_custom_signature_valid(calls, [], tx_info.signature)` returns `VALIDATED`.
  2. `is_valid_signature(tx_hash, signature)`. This is what strkd accounts use today.
  3. `is_valid_signature(SNIP-12 CallSet hash of calls, signature)`.
- Before any of these checks, the pool has asserted that `calls` is a single call to itself, with
  selector `compile_actions` (`utils.cairo:355-371`). The owner therefore sees `user_sk` in the
  calldata, but only during virtual execution, which runs inside the prover.
- `apply_actions` accepts a proof whose base block `B` satisfies
  `B < current ≤ B + proof_validity_blocks` (`privacy.cairo:830-837`). On strkd's Sepolia pool,
  `proof_validity_blocks` is 450.

**strkd today** (`crates/wallet-rpc/src/strk20/mod.rs`). `prepare()` (`:369-457`) does all of this
in one call:
1. Resolves a strkd account from the caller's scope (`ctx()`, `:214-240`).
2. Derives `user_sk` from that account (`:247-249` → `session.rs:236-244` →
   `wallet-core/src/keys.rs:58-68`).
3. Picks `B = head − PROVING_LAG (12)` (`:47`, `:379-380`).
4. Discovers state at `B` and at `latest`, and refuses if `same_state` differs (`:387-398`,
   `:365-367`).
5. Plans with `OsRandomness` (`:399`).
6. Prompts once (`:408-428`).
7. Reads the pool nonce at `B`, builds the `ProofInvocation` and signs it with the account key
   (`:430-438`).
8. Proves (`:440-453`) and builds `apply_actions` (`:454-455`).

Because one call plans, signs and proves, the actions never exist without the proof. An owner
contract has nowhere to approve them.

## 2. Decisions (2026-11-13)

| # | Decision |
|---|---|
| D1 | **Viewing key:** derived from the controller account's key and bound to the owner address (§3.2). Bramble compatibility is not required: only strkd sees these notes. |
| D2 | **Plans live in memory only:** 1 h by default, 24 h at most, wiped on lock and on restart. The format and API leave room to add persistence later without breaking changes (§5.4). |
| D3 | **Disclosure:** a plan returns `plan_hash` and a human-readable summary. The full `client_actions` come back only with `reveal_actions: true`. That flag triggers its own explicit prompt about channel-key disclosure (§5.6). |
| D4 | **Approval hash:** SNIP-12 typed data over strkd's own versioned struct `Strk20Plan`, verified in `is_custom_signature_valid` (§5.3). Owners must advertise SRC5. Owners without it are out of scope (§2.1). |
| D5 | **Submission:** `submit: "none"` (default: return the call and proof) or `"account"` (a strkd account pays, and the prompt notes the public link). The AVNU relay comes later. |
| D6 | **Prompts:** enrolment always prompts, and a grant never covers it. A plan prompts like today's STRK20 calls, so a grant can cover it. Prove prompts only if the prover or submission route changed. `reveal_actions` always prompts. |
| D7 | **Deposits for owners:** phase 3. |
| D8 | **Owners registered with a key strkd didn't derive:** enrolment refuses with a clear error. There is no import path. |
| D9 | **Prove is asynchronous.** It returns a `job_id`, polled through `companion_proveStatus`. Results have a TTL and are wiped on lock (§5.7). |
| D10 | **Errors:** existing codes stay. Errors gain an optional machine-readable `data.reason` (§7). |

### 2.1 Goals and non-goals

**Goals**
- Register, transfer and withdraw for a deployed contract that advertises SRC5
  `is_custom_signature_valid`. strkd holds the owner's viewing key, and the key never leaves strkd.
- The caller supplies the virtual tx's signature felts, and the contract reads them however it
  likes.
- A plan step returns `plan_hash` without proving. A later prove step proves against a reference
  block at or after the owner's on-chain approval.
- The plan/prove split also works for ordinary strkd accounts. For those, strkd signs at prove
  time.

**Non-goals**
- **Owners without SRC5.** These would go through pool checks 2 or 3. Check 2 signs a tx hash that
  contains `user_sk` and the pool nonce, so only strkd can compute it, and it can change between
  approval and proof. Check 3 hashes the `calls`, which also contain `user_sk`. Neither gives a
  hash that an approver can check without the key. Neither is the explicit, SRC5-gated interface
  the pool defines for contract owners.
- Deposits for owners (D7). Importing viewing keys (D8). The AVNU relay (D5).
- Writing or auditing owner contracts. This note gives strkd's side and a reference check.
- Open notes, `invoke`, and `subaccount_invoke`. They are still unsupported (`mod.rs:289`).

## 3. Owner model

### 3.1 Owner records

An owner is a record in the sealed registry. Like agent accounts, the vault is re-sealed
atomically when the record changes (`wallet-rpc.md`, "Durable registry changes").

```
Strk20Owner { owner, chain, pool, controller: (domain, index, address), client_id: Option, label }
```

- The record holds no secrets.
- **Scope.** It follows the account rule (`mod.rs:214-240`). An agent sees only the owners it
  enrolled. An App client or the desktop sees the user's owners.
- **Parameters.** Methods take `owner_address` instead of `account_address`. Passing both is a
  114 error.
- **Code change.** `Ctx` (`mod.rs:194-201`) becomes an owner address plus a signer:
  `Account(AccountRef)` or `External { controller }`.

### 3.2 Viewing key (D1)

Today's key comes from signing `starknet_keccak("<chain>:<pool>")` with the account key
(RFC 6979) and folding `Poseidon(r, s)` into `[1, n/2)` (krusty `derive_scoped_strk20_viewing_key`).
krusty's `canonical_scope` only accepts a canonical felt as the pool argument, so the owner
cannot be appended to it. Instead, add a sibling function, in krusty or in `wallet-core`, built
on krusty's public `sign_stark_hash` and `fold_strk20_viewing_key`:

```
msg     = starknet_keccak("strkd.strk20.owner.v1:<chain_id>:<pool>:<owner>")   // canonical 0x hex
user_sk = fold(sign_rfc6979(controller_sk, msg))
```

- **Domain separation.** The tag keeps this key apart from the account's own STRK20 key.
- **Controller.** The controller is the caller's scoped account: the agent's own account, or a
  user account for App clients.
- **Recovery.** Seed plus owner address, plus knowing which controller was used. Enrolment
  records the controller.
- **Handling.** The key is derived per call in the unlocked session. It is never persisted,
  logged or returned. Error text goes through `scrub` (`mod.rs:78`).

### 3.3 Enrolment

`companion_strk20AddOwner { owner_address, account_address?, label?, chainId? }`:

1. Check the owner is deployed, else `OWNER_NOT_DEPLOYED`.
2. Call `supports_interface(0x2eb9…ded)` at `latest`. Anything but `true` fails with
   `OWNER_NO_CUSTOM_VALIDATION`.
3. Derive the key. If the owner is already registered with the pool (`discovery::public_key`), the
   registered public key must equal the derived one. Otherwise refuse with `OWNER_KEY_MISMATCH`
   (D8). The message says the owner was registered with a key this controller does not derive
   and cannot be enrolled.
4. Prompt. This always prompts and a grant never covers it, like `companion_requestGrant`. The
   prompt shows:
   - the full owner address, chain, pool and controller;
   - "strkd will derive and hold a private key for this contract's private notes";
   - "strkd cannot check who controls this contract; its own signature check decides every spend".
5. Persist the record and return
   `{ owner_address, controller, pool, registered, viewing_public_key }`.

Two companion methods: `companion_strk20ListOwners` (no prompt) and
`companion_strk20RemoveOwner` (prompts). Removing an owner only forgets the record. The key can be
derived again.

## 4. Signature model

- **Owners.** strkd does not sign for an owner. The virtual tx's `signature` is the caller's
  `signature: [felt…]`, passed as given (`invocation.rs:95-116` takes a signature slice). It may be
  empty, for example when the contract only checks an on-chain approval.
- **Timing.** The signature is not part of the V3 hash, so it is supplied at prove time.
- **What strkd still controls:**
  - whether the owner's key is used (enrolment, scope, plan prompt);
  - the actions themselves, which strkd plans and never takes from the caller;
  - the bounds on the reference block;
  - where proving happens;
  - where the result goes.
- **What strkd does not check:** whether the signature is valid. Only the contract can do that.
- **Failing early.** SRC5 is checked at enrolment and again at `B`. strkd never dry-runs
  `is_custom_signature_valid` through the node: that would send `client_actions`, channel keys
  included, to the RPC provider. A rejected signature surfaces when the prover executes the
  virtual tx, as `INVALID_SIGNATURE`, and strkd reports `OWNER_SIGNATURE_REJECTED`.
- **strkd accounts.** For these, prove signs as today with `sign_hash_for(invocation.hash())`
  (`mod.rs:436-438`). A caller-supplied `signature` is then a 114 error.

## 5. Plan/prove API

### 5.1 Methods

| Method | Params | Result | Prompts |
|---|---|---|---|
| `companion_strk20Plan` | `{ owner_address \| account_address, actions?, register?, reveal_actions?, ttl_secs?, chainId? }` | `{ plan_id, plan_version: 1, expires_at, plan_block, chain_id, pool, owner, plan_hash, typed_data, summary, client_actions?, client_actions_felts? }` | yes (§5.6) |
| `companion_strk20Prove` | `{ plan_id, block_number?, signature?, submit?: "none" \| "account", submitter_address? }` | `{ job_id, status: "queued" }` | only if the route changed |
| `companion_proveStatus` (existing) | `{ job_id }` | job; on success `result` = `{ call, proof }` or `{ transaction_hash }` | no |
| `companion_strk20DiscardPlan` | `{ plan_id }` | `true` | no |

- **Actions.** `actions` uses the existing `STRK20_ACTION` parser (`parse_actions`,
  `mod.rs:277-296`). `deposit` is refused for owners until phase 3. `register: true` plans
  `SetViewingKey` and the self-channel.
- **Returned fields.**
  - `typed_data` is the full SNIP-12 object behind `plan_hash`. Anyone can recompute the digest
    with the existing `companion_typedDataHash` (`dispatch.rs:732`).
  - `client_actions` (typed JSON) and `client_actions_felts` (`serialize_actions`,
    `actions.rs:162-168`, equal to `calls[0].calldata[2..]`) come back only with `reveal_actions`.
- **Phase 2.** The existing one-shot methods gain `owner_address` and `signature`. Each runs plan
  then prove internally, which only suits contracts whose signature does not depend on the
  actions.

### 5.2 What is fixed when

| Fixed at plan | Fixed at prove |
|---|---|
| Owner, controller, chain, pool | Reference block `B` |
| `client_actions`, including every index, channel key, salt and random value | Pool nonce, read at `B` (`mod.rs:430-435`) |
| `plan_hash` | Caller `signature` |
| `plan_block`, plus a snapshot of discovered state | Prover, submission route |

Randomness comes from `OsRandomness` once, at plan time. It is stored and never redrawn, so the
approved list cannot change.

### 5.3 Approval hash: SNIP-12 `Strk20Plan` (D4)

The owner approves a SNIP-12 (revision 1) message hash, which strkd's existing
`typed_data_message_hash` computes (`wallet-core/src/keys.rs:158`):

```
domain  = StarknetDomain { name: 'strkd.Strk20Plan', version: '1', chainId, revision: '1' }
type    = Strk20Plan { pool: ContractAddress, owner: ContractAddress, actions_hash: felt }
message = { pool, owner, actions_hash = poseidon_hash_many(client_actions_felts) }
plan_hash = SNIP-12 message hash with account_address = owner
```

- **Bindings.** The chain is bound through `domain.chainId`. The owner is bound both in the
  struct and as the SNIP-12 account address. The pool is bound in the struct.
- **Exclusions.** The hash leaves out `user_sk`, the pool nonce and the block. An approval
  therefore survives the choice of reference block and can be checked without the key.
- **Versioning.** The domain `name`/`version` is fixed. A format change bumps `version` and
  `plan_version` together.

Reference check in the contract. This is a sketch, not audited. It uses the OpenZeppelin SNIP-12
helpers (`OffchainMessageHash`, `StructHash`, `SNIP12Metadata`):

```cairo
const STRK20_PLAN_TYPE_HASH: felt252 = selector!(
    "\"Strk20Plan\"(\"pool\":\"ContractAddress\",\"owner\":\"ContractAddress\",\"actions_hash\":\"felt\")"
);
#[derive(Copy, Drop, Hash)]
struct Strk20Plan { pool: ContractAddress, owner: ContractAddress, actions_hash: felt252 }
impl StructHashImpl of StructHash<Strk20Plan> {
    fn hash_struct(self: @Strk20Plan) -> felt252 {
        PoseidonTrait::new().update_with(STRK20_PLAN_TYPE_HASH).update_with(*self).finalize()
    }
}
impl SNIP12MetadataImpl of SNIP12Metadata {
    fn name() -> felt252 { 'strkd.Strk20Plan' }
    fn version() -> felt252 { '1' }
}

fn is_custom_signature_valid(self: @ContractState, calls: Span<Call>,
                             additional_data: Span<felt252>, signature: Span<felt252>) -> felt252 {
    let call = calls[0];
    let me = get_contract_address();
    if *call.to != self.pool.read() || *call.selector != selector!("compile_actions") { return 0; }
    let data = *call.calldata;
    if *data[0] != me.into() { return 0; }
    // data[1] is user_sk: never read it.
    let plan = Strk20Plan { pool: *call.to, owner: me,
                            actions_hash: poseidon_hash_span(data.slice(2, data.len() - 2)) };
    let h = plan.get_message_hash(me);      // uses get_tx_info().chain_id
    if self.approved_plans.read(h) { VALIDATED } else { 0 }
}
```

- **Approval timing.** The approval is read from storage at `B`, so `B` must be at or after the
  approval block `A`.
- **Strict fallback.** The pool falls back to `is_valid_signature` (checks 2 and 3,
  `utils.cairo:399-419`). The contract must keep that function strict. A permissive one bypasses
  the approval.
- **Test vector required.** A Rust↔Cairo SNIP-12 vector, also checked in starknet.js under
  `conformance/`, is required before release. strkd's SNIP-12 digest has been wrong before (two
  krusty bugs: prefix encoding and shortstring parsing; fixed in krusty #36 and strkd #8 for
  issue #7).

### 5.4 Plan storage (D2)

- **Where.** A `PlanStore` on `ServerState`, in memory, keyed by `plan_id` (128-bit random hex).
- **Binding.** Each plan is bound to the `client_id` that made it. Other clients get
  `PLAN_NOT_FOUND`, the same answer as for an unknown id.
- **Contents.** Owner, controller, chain, pool, actions, `plan_hash`, `plan_block`, the discovered
  `State` without `user_sk`, and the prompt text. Channel keys are among them, which is why the
  store is wiped on lock (`session.rs:143-145`) and on restart.
- **Expiry.** `ttl_secs` defaults to 3600, with a maximum of 86400 that never outlives the session.
  Expired plans are swept when the store is next touched.
- **Caps.** One live plan per `(owner, pool)`, because concurrent plans claim the same slots. At
  most 4 plans per client and 32 in total, else `PLAN_LIMIT`.
- **Room for persistence.** `plan_id` is opaque, and `expires_at` and `plan_version` are explicit.
  A lost plan is simply `PLAN_NOT_FOUND`. Persistence later would only lengthen lifetimes. It
  would use deterministic randomness (`PRF(user_sk, plan_seed)`), so a plan can be rebuilt and
  checked against its stored `plan_hash` without storing secrets.

### 5.5 Checks at prove time

These run synchronously, before a job is queued.

1. The plan exists, belongs to this client, has not expired, and `plan_hash` recomputes from the
   stored actions.
2. `B` defaults to `head − 12`. It must satisfy all of:
   - `B ≤ head − PROVING_LAG`, else `BLOCK_TOO_RECENT`, with the number of blocks to wait;
   - `B ≥ plan_block`, else `BLOCK_BEFORE_PLAN`;
   - `head − B < get_proof_validity_blocks() − margin`, else `BLOCK_TOO_OLD`, so the proof does
     not expire before it lands.
3. SRC5 is still advertised at `B`, else `OWNER_NO_CUSTOM_VALIDATION`.
4. Rediscover at `B` and at `latest`. The plan's slots must be untouched, else `PLAN_STALE`.
   Phase 1 reuses `same_state` (`mod.rs:365-367`) against the snapshot. That check is strict: any
   new incoming note fails it. Phase 2 narrows it to what the plan uses:
   - the notes it spends are still unspent;
   - the outgoing-channel count is unchanged;
   - the next note index of each subchannel it writes to is unchanged.
5. Prompt only if the route changed (§5.6).
6. Read the pool nonce at `B`. Build the `ProofInvocation` from the stored actions, never
   re-planning. Attach the signature, then queue the job (§5.7).

The plan is consumed once the job is queued. If the job fails, the caller makes a new plan.

### 5.6 Prompts (D3, D6)

- **Plan.** Prompts through `Caller::approve` (`mod.rs:182-192`), with text built the way
  `prepare()` builds it (`:408-428`), plus "for contract `<owner>` (`<label>`); the proof is built
  later". A grant can cover it (`dispatch.rs:534-551`).
- **`reveal_actions: true`.** Triggers a second, separate prompt that a grant never covers. It
  says that the full action list includes the channel keys of the channels this batch touches. A
  channel key reveals every past and future note on its channel, with tokens, amounts and timing.
  The disclosure cannot be revoked: the key is fixed for the sender/recipient pair. This is the
  #49 note's finding (`design-49-strk20-channel-disclosure.md` §3). Refusing this prompt still
  returns the plan, without the actions. Revealed actions are redacted from the request log
  (`log.rs` full payloads), as #49 proposes for channel keys.
- **Prove.** No prompt if the prover kind and submission route are the ones the plan prompt named.
  Otherwise it prompts, for example after a switch to a remote prover, which would see `user_sk`,
  or for `submit: "account"` when the plan said "returned unsubmitted". The `"account"` prompt
  notes that submitting from that account links it publicly to the owner and the pool.
- **Enrol.** Always prompts (§3.3).

### 5.7 Proving jobs and result retention (D9)

`companion_prove` today queues a job with `prover::enqueue_prove` (`dispatch.rs:1406-1419`), and
`companion_proveStatus` returns the job (`dispatch.rs:1422-1429`, `prover/src/jobs.rs:120`).
Three things in that path must change for STRK20:

- **Persistence.** `enqueue_prove` saves the payload to proof storage (`prover/src/lib.rs:111`).
  The STRK20 payload holds `user_sk`. Add `enqueue_prove_unrecorded`: the background
  counterpart of `prove_unrecorded` (`lib.rs:120`), which saves nothing and keeps only the result.
- **Job ids are sequential (`p{n}`, `jobs.rs:76`) and any paired client can poll any job.**
  Tag STRK20 jobs with the client that queued them. `companion_proveStatus` answers
  "unknown job" to every other client.
- **Retention.** A STRK20 job's result (`{call, proof}`, or `{transaction_hash}` after
  `submit: "account"`) is kept for 1 h after it finishes. The result is then dropped and the
  status kept. Results are wiped on lock. The activity feed shows only the label and status, as
  today.

Job errors carry the reason as a prefix (for example `OWNER_SIGNATURE_REJECTED: …`) and pass
through `scrub`.

**Remote provers.** The behaviour is unchanged. A non-native prover sees the whole invocation,
`user_sk` included. The plan prompt names the prover, and prove prompts again if it changed.

## 6. Security and privacy

These are design claims. Each needs verification before anyone relies on it.

- **Where the viewing key goes.**
  - It sits in the virtual tx calldata (`invocation.rs:43-44`). Local proving keeps it on the
    machine. A remote prover sees it.
  - The owner contract sees it during virtual execution. Writes and events in the virtual block
    never reach L1. The pool rejects any extra L2→L1 message (`privacy.cairo:842`).
  - Nothing is simulated through the node. The proving payload is never persisted (§5.7).
- **A leaked key costs privacy, not funds.** The contract's check decides every spend, so a
  leaked `user_sk` exposes the owner's notes but cannot spend them. That holds only if both
  `is_custom_signature_valid` and the fallback `is_valid_signature` are strict.
- **Channel keys are off by default.** By default the caller gets only `plan_hash` and a summary.
  With `reveal_actions`, the cost is the irrevocable channel disclosure set out in #49.
- **Approvers trust the summary unless actions are revealed.** Without `reveal_actions`, an
  approver has only strkd's summary and cannot recompute `actions_hash`. With them, the approver
  can recompute it with `companion_typedDataHash` or an independent SNIP-12 tool.
- **Replay.**
  - A proof is single-use: nullifiers and write-once slots make a second `apply_actions` revert.
  - `plan_hash` binds the chain, pool and owner, and fresh randomness makes identical intents
    hash differently.
  - The contract cannot consume an approval from a view function, so it relies on the pool's
    single use. It may also expire approvals.
- **Plan tampering.** Prove takes only a `plan_id`. strkd rebuilds the invocation from its own
  stored plan and re-checks `plan_hash`. Plan ids are unguessable and bound to the client.
- **Spoofed approval UX.**
  - Enrolment shows the full address and does not trust the label.
  - Summaries are built from the actions, not from caller text.
  - strkd cannot stop an intermediary from showing the on-chain approver a different action list.
    Approvers who need certainty use `reveal_actions` and recompute the hash.
- **Denial of service.** Plans and job results are capped, expire, and are wiped on lock. Each
  owner can have only one plan at a time.
- **Linkage.**
  - Registering an owner is public.
  - The on-chain approval links the contract, by timing, to an upcoming pool action.
  - `submit: "account"` links that account to the owner. The prompt says so.

## 7. Errors (D10)

Existing codes stay (`error.rs:56-71`). `RpcErrorObject` (`jsonrpc.rs:20-23`) gains an optional
`data: { reason }`. The change is additive, and clients that ignore `data` see no difference. The
same reason also prefixes the message.

| `data.reason` | Code | When |
|---|---|---|
| `OWNER_NOT_DEPLOYED` | -32006 | No class at the owner address (enrolment or prove) |
| `OWNER_NO_CUSTOM_VALIDATION` | -32006 | SRC5 not advertised, at enrolment or at `B` |
| `OWNER_KEY_MISMATCH` | -32006 | Owner registered with a key this controller does not derive (D8) |
| `OWNER_NOT_ENROLLED` | -32002 | `owner_address` is unknown or outside the caller's scope |
| `PLAN_NOT_FOUND` | 114 | Plan unknown, expired, discarded, consumed, or another client's |
| `PLAN_LIMIT` | -32006 | A per-owner, per-client or global plan cap was reached |
| `PLAN_STALE` | -32006 | The plan's notes or slots moved: make a new plan and get it re-approved |
| `BLOCK_TOO_RECENT` | -32006 | `B > head − 12` |
| `BLOCK_BEFORE_PLAN` | -32006 | `B < plan_block` |
| `BLOCK_TOO_OLD` | -32006 | The proof would expire before it lands |
| `OWNER_SIGNATURE_REJECTED` | job error | Execution failed `INVALID_SIGNATURE`: no approval at `B`, or a bad signature |
| `PROVING_FAILED` | job error | Any other prover failure (scrubbed) |
| `DEPOSIT_UNSUPPORTED` | -32601 | Deposit action for an owner (until phase 3) |
| (existing) | 113 / 118 / 119 / 120 | Refused, not registered, insufficient private balance, privacy leak |

## 8. Testing

Test seeds only (the public BIP-39 vector). Never real keys. No mainnet.

**Unit and dispatch tests** (`cargo test`):
- Owner-key known-answer vector. It must differ from the account's own key and between owners. It
  is reproduced in `conformance/` with starknet.js.
- SNIP-12 `Strk20Plan` vector, checked in Rust, in starknet.js, and in Cairo through the OZ
  helpers (a small Cairo fixture or the live contract below).
- `PlanStore`: TTL, caps, one plan per owner, client binding, wipe on lock, hash re-check.
- STRK20 jobs: nothing persisted, other clients get "unknown job", result dropped after the TTL,
  results wiped on lock.
- Staleness: synthetic `State` pairs for each kind of slot.
- Dispatch with a mock node and a stub prover:
  - enrolment errors (no SRC5, key mismatch);
  - plan → prove → status happy path, with the signature passed through verbatim;
  - `reveal_actions` prompt and redaction from the log;
  - bounds on `B`;
  - re-prompt when the route changed.
- `usage.rs` entries. `tests/server.rs` asserts that the catalogue lists every method.

**Sepolia live run.** An `examples/strk20_owner_live.rs`, run manually and only when the
maintainer decides, because it submits transactions:
1. Deploy a minimal approval-owner contract (SRC5, `approve_plan(hash)` callable by a test
   account, and the check in §5.3) on the Sepolia test pool (fee 0).
2. Enrol it. Plan `register`. Approve `plan_hash` at block `A`.
   - Prove with `B < A`: expect `OWNER_SIGNATURE_REJECTED`.
   - Prove with `B ≥ A` and `submit: "account"`.
3. Transfer from a test account to the owner. Then plan, approve and prove a withdraw for the
   owner.
4. Negative cases: a stale plan, an expired proof window, a second plan for the same owner, a
   pre-registered owner.

## 9. Phasing

1. **Phase 1.**
   - Owner records, enrolment and key derivation; generalised `Ctx`; `PlanStore`.
   - `companion_strk20Plan` / `Prove` / `DiscardPlan` for register, transfer and withdraw, with
     `reveal_actions`.
   - `submit: "none" | "account"`.
   - Unrecorded, client-bound proving jobs with retention.
   - `same_state` staleness, `data.reason`, `usage.rs` and docs.
2. **Phase 2.**
   - `owner_address` and `signature` on the existing one-shot methods.
   - The narrow staleness check.
   - Desktop: owners list and plan prompts.
   - AVNU relay (`submit: "relay"`, with the fee withdraw included in the plan).
3. **Phase 3.** Deposits for owners. Persisted plans (deterministic randomness), if approvals turn
   out to need more than 24 h.

## 10. Still open: verify before relying on this

- Does the deployed pool class (`0x6d163f2b…cf83`, Sepolia and mainnet) contain the SRC5
  custom-validation path from the vendored source?
- Does the pool's nonce stay constant? It only affects the invocation hash, not `plan_hash`, but
  check that the pool never sends real transactions.
- Does the native prover fail fast on `INVALID_SIGNATURE`, during execution and before the
  expensive proving phase?
- Is `get_tx_info().chain_id` inside the virtual OS execution the real chain id? The contract's
  SNIP-12 domain depends on it.
- A Rust↔Cairo↔starknet.js SNIP-12 vector for `Strk20Plan` (see the krusty #36 history).
- AVNU relay: quote lifetime and how to fix the fee withdraw at plan time (phase 2).
