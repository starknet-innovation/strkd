# strk20-owner-example

A reference STRK20 note owner that is a contract, for strkd issue
[#48](https://github.com/starknet-innovation/strkd/issues/48). An admin approves
strkd plans on-chain, and the privacy pool accepts a proof only for an approved plan.

> **Not audited. Testnet reference only.** It shows the checks an owner contract
> needs. It is not a wallet. Deploy your own reviewed contract for real funds.

Design: [`docs/project/design-48-strk20-external-owner.md`](../../docs/project/design-48-strk20-external-owner.md)
(§1 pool background, §4 signature model, §5.3 approval hash, §6 security).

## How it maps to the design

The pool runs every STRK20 operation as a virtual invoke from itself, with one call
`pool.compile_actions(owner, user_sk, client_actions)`. Then it calls
`assert_valid_signature(owner, calls, tx_info)` (starknet-privacy `4026da7`,
`packages/privacy/src/utils.cairo`). The first of three checks that passes wins:

1. **Custom.** The owner advertises SRC5 id
   `0x2eb9faa4ce06e09879e93803798b87d22877b73964334356bf9e0d8cc22ded` and
   `is_custom_signature_valid(calls, [], signature)` returns `VALIDATED`. A panic
   counts as "not accepted", because the pool uses a safe dispatcher.
2. `is_valid_signature(tx_hash, signature)`.
3. `is_valid_signature(SNIP-12 CallSet hash, signature)`.

| Design | Here |
|---|---|
| Owners must advertise SRC5 (D4) | SRC5 component. The constructor registers the custom-validation id. SRC5's own id is built in. SRC6 is not advertised: this is not an account, and the pool doesn't need one. |
| §5.3 approval hash | `is_custom_signature_valid` recomputes `plan_hash` from `calls` and returns `VALIDATED` iff `approved_plans[plan_hash]`. |
| §5.3 strict fallback | `is_valid_signature` always returns 0. |
| §4 caller signature | `signature` and `additional_data` are ignored. Authorisation is the on-chain approval (see below). |
| §6 "a leaked key costs privacy, not funds" | Holds because both entrypoints above are strict. |

### What `is_custom_signature_valid` checks

It returns 0 without panicking on every failure:

1. exactly one call;
2. `call.to == pool` (fixed at construction);
3. `call.selector == selector!("compile_actions")`;
4. `calldata.len() >= 3` and `calldata[0] == get_contract_address()`;
5. `actions_hash = poseidon_hash_span(calldata[2..])`: the serialized
   `Span<ClientAction>`, **including its length prefix**, exactly strkd's
   `client_actions_felts` (`serialize_actions`);
6. `plan_hash` = the SNIP-12 rev 1 message hash of
   `Strk20Plan { pool, owner: this, actions_hash }`, with domain
   `{ name: 'strkd.Strk20Plan', version: 1, chainId: tx_info.chain_id, revision: 1 }`
   and signer = this contract (OpenZeppelin `OffchainMessageHash`);
7. `approved_plans[plan_hash]`.

`calldata[1]` is the owner's STRK20 viewing key (`user_sk`). The contract never
reads, stores or emits it, and it is not part of the hash.

**Why `signature` is ignored.** The approval itself is the authorisation. The pool
reads it from storage at the proof's reference block. A signature on top would only
prove what the approval already proves, and it would need its own replay rules. The
pool always passes `additional_data` empty. A contract that wants a signature as well
(for example "admin approved and a co-signer signed `plan_hash`") can add that check.
strkd passes the caller's `signature` felts through verbatim (§4).

**Why `is_valid_signature` exists at all.** If it were missing, the pool's safe
dispatcher would see an "entrypoint not found" error and treat it as not accepted, so
leaving it out would also be strict. An explicit `0` makes the intent visible and
testable, and a later edit can't add a permissive version by accident.

## The approval hash (SNIP-12 `Strk20Plan`)

```
domain  = StarknetDomain { name: "strkd.Strk20Plan", version: "1", chainId, revision: "1" }
type    = "Strk20Plan"("pool":"ContractAddress","owner":"ContractAddress","actions_hash":"felt")
message = { pool, owner, actions_hash = poseidon_hash_many(client_actions_felts) }
plan_hash = SNIP-12 rev 1 message hash, account address = owner
```

**`version: "1"` is the felt `1`, not the shortstring `'1'` (`0x31`).** SNIP-12
tooling (starknet.js `getHex`, krusty-kms) encodes a decimal-digit string as the
number it spells. `"revision": "1"` follows the same rule, and that is why OpenZeppelin
hard-codes `revision: 1`. The contract therefore sets `version() = 1`. A Cairo `'1'`
there would produce a different hash, and no approval would ever match.

### Test vector

The vector lives in one place:
[`crates/wallet-core/tests/fixtures/strk20-plan-vector.json`](../../crates/wallet-core/tests/fixtures/strk20-plan-vector.json).
Three implementations check it:

| Implementation | Where |
|---|---|
| Cairo, OpenZeppelin SNIP-12 (free function and through the deployed contract, chain id cheated) | `tests/test_owner.cairo` (`vector_*`) |
| Rust, krusty-kms (`wallet_core::typed_data_message_hash`) | `crates/wallet-core/tests/strk20_plan_vector.rs`. It also fails if the Cairo constants drift from the JSON. |
| starknet.js 10.8.0 (`typedData.getMessageHash`) | `conformance/verify.mjs` |

| Input | Value |
|---|---|
| chainId | `SN_SEPOLIA` = `0x534e5f5345504f4c4941` |
| pool | `0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e` (strkd's Sepolia pool) |
| owner (= signer) | `0x0712d1ab5b6e5d3e3c0b2a1f9e8d7c6b5a4938271605f4e3d2c1b0a998877665` (made up) |
| client_actions_felts | `[0x2, 0x7, 0x2a, 0x3, 0x1]` (opaque stand-ins) |
| actions_hash | `0x03342f4106e584ae6fa004f9a518687fa0160dfd61720b8aae0edf25fe89f84a` |
| type hash | `0x035ab6743c4f282adfa4275ad5c9b3f7e15e33def33bdadc084b94eb369b8b66` |
| **plan_hash** | **`0x062a76d34a73ac201527a84ddcf018608bf9443c3daf8597c539304ff5ce3301`** |

## Approving a plan

1. The app calls strkd `companion_strk20Plan { owner_address, actions }` (§5.1) and
   gets `plan_hash`, `typed_data` and a summary. An approver who wants to check the
   hash themselves asks for `reveal_actions: true`, then recomputes `plan_hash` from
   `typed_data` (`companion_typedDataHash`, starknet.js, or this contract's
   `compute_plan_hash(actions_hash)` view).
2. The admin sends `approve_plan(plan_hash)` to this contract. That transaction lands
   in block `A`.
3. The app calls `companion_strk20Prove { plan_id }` with a reference block `B ≥ A`.
   strkd defaults to `head − 12`, so wait about 12 blocks after `A`, or pass
   `block_number`. The pool reads the approval from storage at `B`, and a proof built
   at `B < A` fails with `INVALID_SIGNATURE` (strkd: `OWNER_SIGNATURE_REJECTED`).
4. Submit `apply_actions`. The proof is single-use: nullifiers and write-once slots
   make a replay revert.

**Revocation.** `revoke_plan(plan_hash)` stops proofs whose reference block is after
the revocation. A proof already built against an earlier block, while the approval was
live, stays valid for the pool's `proof_validity_blocks` window (450 on strkd's
Sepolia pool). Revoke before that window opens, or treat the approval as final.

**No expiry.** Approvals stay valid until revoked. A view call can't consume an
approval, so this contract relies on the proof being single-use. Expiring by block
number was left out to keep the reference simple, and because what
`get_block_number()` returns inside the virtual block is unverified (design §10). Clear
stale approvals with `revoke_plan`.

## Security notes

- **Strict fallback.** `is_valid_signature` must stay `0`. A permissive version
  (for example a standard account's ECDSA check) would let a signature over the tx hash
  or the CallSet hash bypass approval.
- **Never read `user_sk`.** `calldata[1]` is the viewing key. It is visible during
  virtual execution, inside the prover only. Don't store it, emit it or branch on it.
- **Approval must come before the reference block.** See "Approving a plan" step 3.
- **Bound to chain, pool and owner.** `chainId` comes from `tx_info` (whether it is
  the real chain id inside the virtual OS is listed as unverified in design §10). The
  pool is fixed at construction. The owner is both a struct field and the SNIP-12
  signer.
- **Admin is a single address, with no transfer and no upgrade.** Use a multisig
  account as admin if you need more than one approver. To change the admin or the
  pool, deploy a new owner. The new owner's notes and viewing key are separate.
- **No funds handling.** Notes live in the pool. This contract holds no tokens.
- **What the admin approves.** Without `reveal_actions`, the admin trusts strkd's
  summary for what `actions_hash` stands for (design §6).

## Build and test

Toolchain in `.tool-versions` (scarb 2.18.0, starknet-foundry 0.60.0).
OpenZeppelin is pinned to the versions the deployed pool uses (introspection 3.0.0,
utils/interfaces 2.1.0).

```bash
cd contracts/strk20-owner-example
scarb fmt --check
scarb build
snforge test          # 26 tests, including the Cairo side of the vector
```

CI runs these in the `owner-contract` job (`.github/workflows/ci.yml`).

## Declare and deploy on Sepolia (sncast)

This was not run for the PR. It is shown for the manual live run in design §8. Use a
funded test account, never a real key.

```bash
# One-time: import or create a Sepolia test account for sncast
sncast account import --network sepolia --name owner-admin \
  --address 0x<admin_account> --private-key 0x<test_key> --type oz

scarb build
sncast --account owner-admin declare --network sepolia \
  --contract-name Strk20OwnerExample
# → class_hash

# constructor(admin, pool)
sncast --account owner-admin deploy --network sepolia \
  --class-hash 0x<class_hash> \
  --constructor-calldata 0x<admin_account> 0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e
# → owner contract address: enrol it with companion_strk20AddOwner

# Approve a plan_hash returned by companion_strk20Plan
sncast --account owner-admin invoke --network sepolia \
  --contract-address 0x<owner> --function approve_plan --calldata 0x<plan_hash>

# Check
sncast call --network sepolia --contract-address 0x<owner> \
  --function is_plan_approved --calldata 0x<plan_hash>
```

Flags vary between sncast versions; check `sncast <cmd> --help`. The pool address
above is strkd's Sepolia test pool. Check it against strkd's configured pool before
deploying.
