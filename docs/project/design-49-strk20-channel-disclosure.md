# Design note — STRK20 note disclosure for third-party verification (#49)

_Status: options A and C built (2026-11-13, PR for #49); option D is future work; option B
rejected. The decisions are in [§13](#13-decisions-2026-11-13). Sections 1–12 are the analysis
as drafted, kept for the record; where they differ from §13, §13 wins. Issue:
[#49](https://github.com/starknet-innovation/strkd/issues/49). Related:
[#48](https://github.com/starknet-innovation/strkd/issues/48) (external contract owners,
plan/prove split)._

Code references are to this repo as of the draft (line numbers may have moved since), and to
`starknet-innovation/starknet-privacy` at `b2582c8` (`packages/privacy/src/…`, written
`privacy/…` below). The derivations this feature relies on (`compute_channel_key`,
`compute_channel_marker`, `compute_note_id`, `enc_note_packed_value`, `pack`) were re-checked
against `4026da7`, the commit of the deployed class, and are unchanged there (§13.4).

## 1. Problem

A sender pays someone privately through the pool. A third party (an app contract, or a
proof that app consumes) wants to confirm "S sent amount A of token T to R". It must not
need R's full viewing key. #49 proposes a prompted method that returns the key of S's
outgoing channel to R. The verifier would then use two pool views:

- `channel_exists(marker)` authenticates the sender;
- `get_note(note_id)` plus the amount hash recovers the amount.

#49 also asks for the index and salt of a note strkd just created.

## 2. Goals and non-goals

**Goals**
- A verifier can confirm sender, recipient, token and amount of one note against pool
  state, without R's viewing key and without trusting S's word.
- The approval prompt says exactly what is disclosed.
- The caller learns where its just-sent note is (token, index, note id).

**Non-goals**
- Disclosing `user_sk`, or anything that lets the holder spend or send.
- Proving that a note is still unspent. That needs the owner's key (§3.3).
- Open notes (`"OPEN"`) and `invoke` actions in the first step. Both are unsupported today
  (`crates/wallet-rpc/src/strk20/mod.rs:244-246`, `:267-269`).

## 3. What a channel key reveals

### 3.1 How the key and everything under it are derived

| Value | Formula | Where |
|---|---|---|
| channel key `ck` | `H(CHANNEL_KEY_TAG, S, S_sk, R, R_pk)` | `crates/strk20/src/hashes.rs:89-104`; `privacy/privacy.cairo:398-400` |
| channel marker | `H(CHANNEL_MARKER_TAG, ck, S, R, R_pk)` | `hashes.rs:113-128` |
| subchannel id (per token, index `k`) | `H(SUBCHANNEL_ID_TAG, ck, k, 0)` | `hashes.rs:130-132` |
| subchannel token | `enc_token − H(ENC_TOKEN_TAG, ck, k, 0, salt)` | `crypto.rs:65-71` |
| note id (token `T`, index `i`) | `H(NOTE_ID_TAG, ck, T, i, 0)` | `hashes.rs:146-148` |
| amount pad | low 128 bits of `H(ENC_AMOUNT_TAG, ck, T, i, 0, salt)` | `hashes.rs:150-155`, `crypto.rs:131-139` |
| nullifier | `H(NULLIFIER_TAG, ck, T, i, 0, owner_sk)` | `hashes.rs:157-168` |
| outgoing channel id | `H(OUTGOING_CHANNEL_ID_TAG, S, S_sk, n, 0)` | `hashes.rs:106-111` |

Two facts follow:

- **`ck` is fixed for the (S, R) pair.** Its inputs are S's address and viewing key and R's
  address and registered public key. None of them change: `S_sk` is a deterministic
  derivation, and registration is immutable (`docs/project/strk20-plan.md` §1). Every note S
  ever sends R, in any token, lives under this one key. There is no rotation and no
  revocation.
- **A note's salt is public.** It is the high 128 bits of the stored `packed_value`
  (`crypto.rs:93-122`; `privacy/objects.cairo:91-100`). It is also emitted in the
  `EncNoteCreated` event (`privacy/privacy.cairo:667`). Disclosing it adds nothing.

### 3.2 What a holder of `ck` (plus S, R) can do

| Capability | Yes/No | Evidence |
|---|---|---|
| Check the channel was opened by S | Yes | marker from `ck, S, R, R_pk`; only written by `open_channel`, which authenticates `S_sk` against S's registered key (`privacy.cairo:370-376`, `:404-420`), inside a virtual tx signed by S's account (`privacy.cairo:202-207`) |
| List every token S has sent R | Yes | scan `subchannel_id(ck, k)` for k = 0.. and decrypt (`discovery.rs:184-191`) |
| List every note on the channel, past | Yes | scan `note_id(ck, T, i)` for i = 0.. until an empty slot (`discovery.rs:192-195`) |
| Learn every amount | Yes | pad from `ck` and the public salt (`crypto.rs:113-142`) |
| Learn **future** notes and amounts | Yes | the key never changes; new notes use the next index (`planner.rs:242-254`) |
| Find the transaction that created each note | Yes | `EncNoteCreated.note_id` is an indexed event key (`privacy/events.cairo:82-88`) |
| Link those transactions' other public effects to S | Yes | the same tx carries the batch's `Withdrawal` events with `to_addr`, `token` and `amount` in the clear (`privacy/events.cairo:16-27`), such as the AVNU fee withdraw and any withdrawal S batched with the transfer |
| Learn whether R has spent a note | No | the nullifier needs R's key (`hashes.rs:157-168`, `privacy.cairo:617`) |
| Spend a note | No | `use_note` requires the owner's key matching the subchannel marker (`privacy.cairo:593-603`) |
| Create notes or subchannels on the channel | No | `create_enc_note` recomputes `ck` from `S_sk` (`privacy.cairo:726-728`); `open_subchannel` takes `ck` as input, but checks the marker against the authenticated sender (`privacy.cairo:436-445`) |
| Find S's other channels or recipients | No | they need `S_sk` (`hashes.rs:106-111`, `:89-104`) |
| Recover `S_sk` | No (preimage resistance of Poseidon) | — |

**Conclusion: the hypothesis holds.** A channel key reveals the full history and the full
future of S→R payments: tokens, amounts, count, timing. It also lets the holder tag S's
batches on-chain, which reveals any public withdrawals S made in them. It cannot be
revoked. It does not let anyone spend, send or see S's other channels. "Reveals only that
channel" is accurate. "Minimal disclosure" is not.

R also knows `ck`: R decrypts it from its channel info (`crypto.rs:45-54`). So R can disclose it
too. That is already narrower than R's full viewing key, which is the workaround #49 avoids.

### 3.3 Is there a narrower per-note secret?

There is no narrower secret in the pool's hash tree. Every per-note value hangs off `ck`:

- **Note id plus amount pad, without `ck`.** The verifier can subtract the pad and read
  *some* amount. It cannot check that the pad is right, because any pad yields any amount.
  It also cannot tie the note id to S or R, because the id is an opaque hash of `ck`. This
  check is unsound against a dishonest sender.
- **A signed claim from S** ("note X carries A to R"). This is S's word, not pool state, so
  it is unsound for the same reason.
- **Per-token scope.** Subchannel ids and note ids both take `ck` as input
  (`hashes.rs:130-148`). There is no per-token key.

The only way to confirm one note without disclosing `ck` is to keep `ck` as a **private
witness in a proof**. The proof checks the marker and the note against pool state and outputs
only `(S, R, T, A, note_id)`. The pool already has a mechanism for this, described in §4,
option D.

## 4. API options

| | A. Channel key | B. Per-note opening (no `ck`) | C. Note locator from the invoke | D. In-proof verification |
|---|---|---|---|---|
| Returns | `ck` and the marker inputs | note id, pad, amount | `{recipient, token, index, note_id, salt}` per created note | nothing secret; the app receives a proven result |
| Verifier can check sender and amount | Yes | **No** (unsound) | No: it only locates the note | Yes |
| Discloses | the whole S→R channel, forever (§3.2) | — | the note's position (the index counts prior S→R notes of that token) | `(S, R, T, A)` of one note, to the app |
| Work in strkd | small | — | small | large: `invoke` / compute actions, timing, a Cairo `privacy_compute` contract on the app side |
| Works with any verifier | Yes | — | needs A or D | only apps that implement `privacy_compute` |

**D in more detail.** The pool's `compute_and_invoke` action calls an arbitrary contract's
`privacy_compute([identity_key] ++ compute_additional_data)` *inside the user's virtual
execution*. It then forwards only the result to a public `privacy_invoke_with_computation`
call (`privacy/privacy.cairo:545-575`). The only restriction the pool places on the target is
that it is non-zero (`privacy/actions.cairo:235-240`). An app's `privacy_compute` could take
`ck, T, i` as private input, read `channel_exists` and `get_note` at the proof's base block,
recompute the amount, and return `(S, R, T, A, note_id)`. strkd would derive `ck` itself, so
`ck` never leaves the machine (local proving, as for transfers today). Constraints:

- The note must exist at the proving block. That block is head − 12
  (`mod.rs:47`, `:357-358`), so verification is a *later* batch, about a minute after the
  transfer, not the same batch.
- Each verification costs one proof and one `apply_actions` (about 78M L2 gas, plus the pool fee;
  plan §2b).
- strkd must support the `invoke` and compute actions, which are not implemented today
  (`mod.rs:267-269`).

### Recommendation

1. **Ship C now**, alongside the planned method. `wallet_strk20PrepareInvoke` and
   `wallet_strk20InvokeTransaction` return a `notes` array. The planner already holds every
   field (`planner.rs:242-249`), and `note_id = H(ck, T, i)` is computed inside strkd. The
   key itself is not returned.
2. **Ship A as `companion_strk20ChannelKey`**, but scope it and word it as a channel
   disclosure, not a note disclosure (§5). It is the only option that works with an
   arbitrary verifier today. Always prompt for it, even under an active grant (§8).
3. **Prefer D as the long-term answer.** Track it with the `invoke` work and #48. Once it
   exists, the docs and the agent guide (`usage.rs`) point verifiers to D first and to A
   only when D is unavailable.
4. **Do not build B.** It looks minimal, but it proves nothing.

## 5. API sketch

### `companion_strk20ChannelKey` (option A)

```
params:  { recipient, account_address?, chainId? }
returns: { channel_key, sender, recipient, recipient_public_key, marker, pool, chainId,
           subchannels: [{ token, note_count }] }
```

- `sender` is the scoped account (`ctx`, `mod.rs:192-218`). `recipient_public_key` is read
  from the pool at `latest`.
- `marker` is returned so a verifier can check `channel_exists(marker)` without rebuilding it.
  `subchannels` lists what is already exposed: the same information the key yields, offered
  so that the user sees it in the prompt.
- **Do not return the outgoing-channel index.** #49's sketch includes `index`. If that means
  the channel's slot in S's outgoing list, it reveals how many recipients S has. The note
  index belongs to option C. See open question 1.
- The method refuses with -32006 if the channel isn't open, because there is nothing to
  verify yet.

### Note locator (option C)

`wallet_strk20PrepareInvoke` and `wallet_strk20InvokeTransaction` add:

```
notes: [{ recipient, token, amount, index, note_id, salt }]
```

The array lists transfers only. Change notes on the self-channel are left out: they are the
user's own, and their ids reveal nothing useful to a verifier. `salt` is included for
convenience, though it is public on-chain (§3.1). From PrepareInvoke, the locator holds only
if the returned call is submitted before any other batch from this account to the same
recipient and token. The doc string says so.

## 6. Approval prompt wording

Option A, from `Caller::approve` (`mod.rs:161-169`):

> **Reveal your private payment channel to {R} (STRK20, {chain}, account {S}).**
> Whoever receives this key can see **every** private payment you have sent or will ever
> send to {R}: tokens, amounts, and the transactions that carried them, including any
> public withdrawals in those transactions. It cannot be revoked. It does not let anyone
> spend your funds or send from your account, and it does not reveal your payments to other
> recipients. Already on this channel: {n} note(s) in {k} token(s).

For option C, no new prompt is needed. The existing transfer prompt (`mod.rs:388-403`) covers
it, because the locator holds no key.

## 7. Storing recently sent notes

There is no need to persist locators to make option C work: they are returned in the
response. To verify later, or to cover a lost response, strkd can rediscover the notes from
chain state. `discover_channel` already finds each subchannel's `next_note_index`
(`discovery.rs:171-212`), and the creating tx can be found from the `EncNoteCreated` event
keyed by `note_id`.

If a "recent sends" list is wanted (for the Private tab, or for D's later batch), it should
store only non-secret locators: `{chain, pool, sender, recipient, token, index, note_id,
amount, tx_hash, created_at}`. Never `ck` or `user_sk`, in keeping with
`mod.rs:12-17`. It can live in the same per-network store as other non-secret settings, capped
(for example, the last 50) and clearable. The amount and the recipient are private data, so
the list is at least as sensitive as `wallet_strk20Balances`. Reading it is approval-gated
like that method (`mod.rs:568-579`).

## 8. Security and privacy analysis

- **Irreversible disclosure.** A leaked channel key can't be rotated without a new account
  or pool. For that reason, `companion_strk20ChannelKey` must **not** be auto-approved by a
  time-limited grant. `gated_approval` approves outright when a grant is active
  (`dispatch.rs:534-551`). Add a must-prompt path for this method.
- **Request log.** With full payloads on, the result JSON is stored in SQLite
  (`dispatch.rs:329-338`, `log.rs:51-54`). Redact `channel_key` from this method's
  result even then, as the viewing key is redacted from errors (`mod.rs:75-83`).
- **Agents.** A paired agent receiving the key can forward it anywhere. The prompt must name
  the client (it does: `dispatch.rs:546`). Scope stays per account (`mod.rs:197-208`).
- **Desktop.** If exposed in the Private tab, it gets the same wording and an explicit
  confirmation, because desktop calls skip the broker (`mod.rs:162-163`).
- **Linkage.** Option A lets the holder tag S's relayed batches (§3.2), weakening the
  AVNU relay's sender hiding for those batches. Option D does not.
- **Self-channel.** Disclosing S's channel to itself exposes all S's change and deposit notes.
  Refuse `recipient == account` (114).
- **Claims needing verification before release:**
  - that `EncNoteCreated` events and `Withdrawal` events for one batch land in one public
    transaction that any node can query by key (from the Cairo source, not yet checked
    on-chain);
  - that `privacy_compute` can call the pool's views from inside the virtual OS, and that the
    virtual OS places no restriction on the called class (option D);
  - that no pool upgrade changes `ck` derivation or adds a per-note key (the pool is
    upgradable; re-check against the deployed class `0x6d163f2b…cf83`);
  - that Poseidon preimage resistance is the only barrier between `ck` and `S_sk` (standard,
    but unaudited here).

## 9. Errors

| Case | Code |
|---|---|
| user refused | 113 |
| bad `recipient`, `recipient == account` | 114 |
| sender or recipient not registered | 118 (`Strk20NotRegistered`) |
| channel not open yet | -32006, "send to {R} first" |
| account not in the client's scope | -32002 |
| no node | -32005 |
| `invoke` / compute (option D) before it exists | -32601 |

## 10. Testing

**Unit (no network).**
- The known-answer vector for `ck`, the marker, the note id and the amount pad, added to
  `crates/strk20/tests/fixtures/strk20-vectors.json` and re-derived by
  `conformance/strk20.mjs`.
- `plan()` → locator: for a planned transfer, `notes[i].note_id == note_id(ck, T, index)`,
  and `encrypt_note_value(amount, salt, ck, T, index)` matches the packed value predicted from
  the plan (`crypto.rs:146-161`).
- A verifier simulation over a mock `PoolReader`: with `ck` it recovers every note on the
  channel, including one added after disclosure. That pins the disclosure scope as a
  regression test. Without `ck` (pad only), it shows that a forged pad yields an arbitrary
  amount.
- Handler tests: the prompt text contains "every" and "cannot be revoked"; an active grant
  still prompts; `channel_key` is absent from the full-payload log; `recipient == account` is
  refused.

**Live, Sepolia, test seed only. Never real keys.** Extend
`crates/wallet-rpc/examples/strk20_live.rs`. Use test-seed accounts #0 and #1 on strkd's
Sepolia test pool. Send #0→#1 and read the locator. Call `companion_strk20ChannelKey`, then
check `channel_exists(marker)` and `get_note(note_id)`, and decode the amount with a
standalone verifier script (starknet.js only, no strkd code). Send a second note and confirm
that the old key decodes it too. This step changes Sepolia state, so it needs explicit
sign-off. No mainnet run.

## 11. Relation to #48

- **The plan/prove split** (#48) gives option D its shape: plan a batch with a
  `compute_and_invoke` action carrying `ck` as private data, let the app approve, then prove.
  The locator (C) is the natural output of #48's *plan* step as well. One `notes` schema
  should serve both.
- **External contract owners.** If S is a contract that strkd drives (#48), S's `ck` is
  derived from the viewing key strkd holds for it. The same method applies, and the
  disclosure is just as broad. Who may approve that disclosure (the contract's logic, or the
  desktop user) is a #48 question.

## 12. Open questions

1. What is `index` in #49's sketch: the note index, or the outgoing-channel index? This
   note assumes the note index and declines to expose the channel index.
2. Who is the verifier in practice? If it is a proof the user generates locally, option A's
   disclosure stays on the user's machine, and the risk is mostly agent forwarding. If a
   third party proves it, the full channel leaks to them.
3. Can the response of `wallet_strk20InvokeTransaction` and `wallet_strk20PrepareInvoke`
   grow beyond wallet-API 0.10.4's `{transaction_hash}` / `STRK20_CALL_AND_PROOF` without
   breaking bramble or other clients? Or does the locator go behind a `companion_` flag?
4. Is a per-pair channel acceptable at all for the target apps, given that it cannot be
   revoked? Should strkd warn when a recipient is an app contract and suggest D?
5. Should option A be desktop-only (no agent access) until D exists?
6. Does StarkWare's SDK plan a per-note viewing grant or auditor-style disclosure that would
   make D unnecessary? Check `starknet-privacy` upstream.

_Security and privacy statements above come from reading the code and need review, plus the
live checks in §10, before anyone relies on them._

## 13. Decisions (2026-11-13)

Taken with the user on 2026-11-13, and built in the PR that fixes #49.

### 13.1 Scope

- **Build A and C. Do not build B or D.** B (a per-note opening without `ck`) is unsound
  (§3.3). D (in-proof verification through `compute_and_invoke`) stays the long-term answer,
  as future work, tracked with the `invoke` actions.
- No per-note "opening" of any kind.

### 13.2 Option C: `notes` locators

- `notes: [{ recipient, token, amount, index, note_id, salt }]` is an **additive** result
  field on `wallet_strk20PrepareInvoke`, `wallet_strk20InvokeTransaction`,
  `companion_strk20Plan` and the `companion_strk20Prove` job result (`{call, proof, notes}` or
  `{transaction_hash, submitter_address, call, notes}`). Existing fields are unchanged;
  bramble and other clients ignore unknown fields. This answers open question 3: no
  `companion_` flag.
- **One entry per transfer note.** The planner (`strk20::planner::Plan::notes`) records a
  `NoteLocator` for each note it creates for an `Intent::transfers` entry, in that order.
  **Change and deposit notes back to the subject are excluded**, so a deposit-only batch
  returns `notes: []`. An explicit transfer to oneself is included, because the caller asked
  for that note.
- **`index` is the note index** on the (sender, recipient, token) subchannel: 0 for the first
  note the sender ever sent that recipient in that token. This answers open question 1. The
  outgoing-channel index is never returned anywhere.
- `note_id = compute_note_id(ck, token, index)` and `salt` is the note's 120-bit salt (≥ 2).
  `amount`, `note_id` and `salt` are hex felts; `index` is a number.
- **No key in a locator.** `companion_strk20Plan` returns `notes` without `reveal_actions`,
  because nothing in it is secret: `note_id` and `salt` are public once the batch lands, and
  before that they are opaque hashes.
- From PrepareInvoke and Plan, a locator holds only if that batch lands before any other
  batch from the subject to the same recipient and token. The method docs say so.

### 13.3 Option A: `companion_strk20ChannelKey`

```
params:  { recipient, account_address? | owner_address?, chainId?, pool_address? }
returns: { channel_key, sender, recipient, recipient_public_key, marker, pool, chainId,
           subchannels: [{ token, note_count }] }
```

- **Who may call it.** Paired apps and agents, for subjects in their scope: their accounts,
  and the owner contracts they enrolled. Subject resolution reuses #48's `ctx()`. An agent
  asking for a user account or another client's owner gets -32002 (`OWNER_NOT_ENROLLED` for
  owners). Not desktop-only (open question 5): the always-on prompt is the gate.
- **Always prompts** through `Caller::approve_always`; a grant never covers it.
- **The prompt** names the subject (account, or owner contract and its controller), the
  recipient, the chain and the pool. A non-canonical pool is flagged through `pool_label`. It
  states that the key reveals **every** payment the sender has sent or will ever send the
  recipient, in every token, with amounts and the carrying transactions (including public
  withdrawals in them). It states that this cannot be revoked, that it does not allow
  spending, and that it does not reveal payments to other recipients. It ends with the note
  and token counts already on the channel.
- **Refusals**, all before the prompt:
  - `recipient` = the subject: 114, `data.reason` `SELF_CHANNEL`;
  - no open channel: -32006 `CHANNEL_NOT_FOUND`;
  - recipient not registered: 118;
  - a bad `recipient`, or a `pool_address` that is not the configured pool: 114.
  Refusing the prompt is 113 and returns nothing.
- **No outgoing-channel index** in the result.
- **Logging.** `channel_key` is replaced by `"<redacted>"` in the full-payload request log
  (`dispatch.rs`, `REDACTED_RESULT_FIELDS`). It is never logged otherwise. Node errors are
  scrubbed of both the viewing key and the channel key.
- **`marker` is what `channel_exists` checks.** In the pool,
  `open_channel` writes `channel_exists[compute_channel_marker(channel_key, sender_addr,
  recipient_addr, recipient_public_key)]`, which is
  `H('CHANNEL_MARKER_TAG:V1', ck, S, R, R_pk)` (`privacy.cairo` `open_channel`, `hashes.cairo`
  `compute_channel_marker`, at `4026da7`). That is `strk20::hashes::channel_marker`, which the
  conformance vector pins and `conformance/strk20.mjs` re-derives with starknet.js.
- **A known one-bit leak.** `CHANNEL_NOT_FOUND` comes back before the prompt, so an in-scope
  caller learns whether the subject has a channel to `recipient` without the user seeing a
  prompt. The existing methods already tell an in-scope caller similar things before their
  prompts (`INSUFFICIENT_PRIVATE_BALANCE`, `PRIVACY_LEAK`). Prompting for a channel that
  doesn't exist would only ask the user to approve nothing.

### 13.4 Verification

- **Against the deployed pool source (`4026da7`):**
  - `compute_channel_key`, `compute_channel_marker` and `compute_note_id` have the same tags
    and argument order as `strk20::hashes`;
  - `create_enc_note` stores `enc_note_packed_value(ck, token, index, salt, amount)` =
    `pack(salt, enc_amount)` at `notes[compute_note_id(ck, token, index)]`, which is
    `strk20::crypto::encrypt_note_value`.
- **Unit and dispatch tests** (`crates/strk20/src/planner.rs`,
  `crates/wallet-rpc/tests/strk20_disclosure.rs`, against an in-memory pool built with the
  pool's own hashes and encryptions):
  - locators recomputed from `ck`, token and index;
  - a locator's salt equals its revealed `create_enc_note` salt;
  - no viewing or channel key in any result or log;
  - change and deposits excluded, transfer-to-self included;
  - the prove job carries the plan's locators;
  - the channel key prompts under a grant, with the wording above;
  - a refusal gives 113 and nothing else;
  - self, no channel, unregistered recipient and scope are all refused without a prompt;
  - an owner subject works;
  - the key is absent from the request log;
  - the marker equals the pool derivation, and a note added after disclosure decrypts
    under the old key.
- **Live, Sepolia, read-only, test seed only** (`strk20_live channel-key`, test pool
  `0x03016a…6323`, no transaction):
  - account 1 → owner `0x01f03e…27f4`: the marker matches the derivation,
    `channel_exists(marker)` is true, and note 0 is found and decrypts to 0.01 STRK, the note
    of tx `0x523ada5b…e3`;
  - account 1 → account 0: the marker is set, and notes 0–4 are found and decrypt.
  The send-and-verify run of §10 (new notes, a starknet.js-only verifier) was not done,
  because it changes Sepolia state.

### 13.5 Still open

- Open questions 2, 4 and 6 stand. Whether to warn when the recipient is an app contract and
  suggest D waits for D.
- A Private-tab (desktop) entry point is not built. If added, it gets the same wording and an
  explicit confirmation (§8).
- No "recent sends" store (§7).
