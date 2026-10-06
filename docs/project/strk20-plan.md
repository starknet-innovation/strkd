# STRK20 — plan

_Drafted 2026-10-05. Supersedes the "Privacy — parked" decision ([#20](https://github.com/starknet-innovation/strkd/issues/20),
[`bramble-convergence.md`](./bramble-convergence.md) §5.6, decision I)._

strkd adds support for the canonical Starknet privacy pool (STRK20), on the standard
`wallet_strk20*` surface, interoperable with bramble. Privacy was parked because the only
implementation was Tongo, which cannot express the note-based surface. This plan targets the
official protocol instead, so that objection no longer applies. PR #11 (Tongo) is not revived.

## 1. How the protocol works (what strkd must implement)

Sources: `starknet-innovation/starknet-privacy` (contracts under `packages/privacy`, SDK under
`sdk/`), bramble's `docs/STRK20_PRIVACY.md`, Starkscan's
[STRK20 prover docs](https://starkscan.co/docs/api/strk20-prover).

- **Notes, not balances.** The pool stores encrypted notes, nullifiers and per-recipient
  channels in write-once slots; Poseidon throughout, `'<NAME>_TAG:V1'` domain tags, no Merkle tree.
- **One secret per account, `user_sk`** (the SDK's "viewing key"). It both decrypts and spends:
  nullifiers and channel keys depend on it. Its public key is registered once (`SetViewingKey`,
  immutable), with an escrowed copy encrypted to the pool's auditor key.
- **Derivation (wallet convention, shared with bramble):** sign
  `starknet_keccak("<chainId>:<poolAddress>")` with RFC-6979 Stark ECDSA under the account key,
  `Poseidon(r, s) mod n`, fold into `[1, n/2)`. Reference: krusty
  `crates/kms/src/strk20.rs::derive_scoped_strk20_viewing_key`. Same seed → same notes in both wallets.
- **Every operation is a SNIP-36 virtual invoke.**
  1. The wallet builds an INVOKE v3 with `sender_address = pool`, nonce 0, zero resource prices,
     calldata `pool.compile_actions(user_addr, user_sk, ClientActions)`, signed by the user's account.
  2. A prover runs it in the virtual OS; `__execute__` emits `ServerActions` as one L2→L1 message.
  3. Any account submits `pool.apply_actions(ServerActions, Option<ScreeningAttestation>)` as a
     proof-carrying invoke. The pool checks the virtual-OS variant, the block window and the
     message hash; the network verifier checks the proof.
- **Operations:** register (`SetViewingKey`), deposit (`TransferFrom`, needs an ERC-20 approve to
  the pool), private transfer (`UseNote` + `CreateEncNote` + change), withdraw (`TransferTo`),
  and later `OPEN` notes, `invoke`, `subaccount_invoke`. The proving base block is ≥ 10 blocks
  behind the head; any state the proof reads must be at least that old.
- **Deposit screening.** `apply_actions` requires a `DepositorValidation` attestation (SNIP-12,
  pool `screener_public_key`, valid 300 s from `issued_at`) iff the batch contains a deposit, and
  rejects one otherwise. Only the operator's prover can issue it (it screens the depositor while
  proving), so **deposits cannot be proved locally.**
- **Fees:** the pool charges a STRK fee per action (~4 STRK live, read `get_fee_amount()`).

## 2. Decisions (2026-10-05)

| # | Decision |
|---|---|
| P1 | **Prove locally everything except deposits** (register, transfer, withdraw) with strkd's prover. `user_sk` never leaves the machine for these. This is the main difference from bramble, which proves everything remotely. |
| P2 | **Deposits use a remote prover.** Starkscan's relay with the user's own API key from Settings; if none is set, bramble's gateway (`wallet.nodes.starknet.org/api/strk20/prover`). |
| P3 | **Submission of non-deposit batches:** AVNU private paymaster when the user sets an AVNU API key in Settings. Without one, strkd returns the signed, proven payload unbroadcast, for submission from another account. |
| P4 | **Rust port** of the SDK's action compiler and invocation builder; no embedded TS SDK. Checked against the TS SDK by an independent JS verifier, as `conformance/verify.mjs` does for derivation. |
| P5 | **Agents may deposit, transfer and withdraw**, under the existing pairing and approval policy. |
| P6 | ~~Large proofs (PROOF2 / 0.14.4) first.~~ **Dropped 2026-10-05:** StarkWare is reworking PROOF2 for soundness issues before it reaches testnet. STRK20 builds on `PROOF1` (v1.2.2) and revisits the proof format when a fixed release lands. See [`backlog.md`](./backlog.md). |
| P7 | **Test on Sepolia first.** Pool `0x03ce2d315cb201ac87f4ff1736d366b39e18fdcac669ea007a51a74407803a3e` (deployed block 15865757, UDC tx `0x7c6949e9…c614`) runs the **same class as mainnet** (`0x6d163f2b…cf83`) and has accepted `PROOF1` proofs. Its config: fee 0, `proof_validity_blocks` 450, auditor key `0x6287ba0e…2bcb`, screener key `0x2159dc65…bf5` (not the `0xCAFEBABE` test key, so Sepolia deposits need the operator's Sepolia prover). The older `0xd894af9e…c233` pool (mezcal runbook) predates screening; ignore it. Mainnet (canonical pool, Starkscan) is the final check, with a maintainer-funded account. |

## 2b. Phase 0 results (2026-10-06, Sepolia)

Every privacy operation was proved **locally** by strkd's bundled prover (snip-36-prover-backend
v1.2.2, `PROOF1`) and accepted on-chain. The official TS SDK (`0.14.3-rc.4`) built and signed each
transaction and treated the local runner as its proving service. The viewing key uses bramble's
scoped derivation; the JS port matches krusty's known-answer vector.

Pool: our own deployment of the mainnet class (`0x6d163f2b…cf83`) at
`0x03016a46eec8b164e8a89337c048b5cd1463ea4c4de120b9cb76b3df88646323`. Its screener is the
public test key `0xCAFEBABE` from StarkWare's screening vectors, so we sign test attestations
ourselves. Test-seed accounts #0 `0x0497e844…7b4f` and #1 `0x024680b5…f8270`.

| Operation | Tx | Prove | Proof | L2 gas | Fee (Sepolia) |
|---|---|---|---|---|---|
| Register #0 | `0x37023762…d9c1` | 60 s | 238 KB | 78.5M | 1.31 STRK |
| Deposit 1 STRK | `0x451409dd…eb69` | 34 s | 236 KB | 84.6M | 1.42 STRK |
| Withdraw 0.3 STRK | `0x4bb423d1…12e7` | 49 s | 229 KB | 78.5M | 1.32 STRK |
| Register #1 | `0xdd60bc76…2def` | 55 s | 236 KB | 78.5M | 1.32 STRK |
| Transfer 0.2 STRK #0→#1 | `0x429a2f83…64a6` | 68 s | 230 KB | 83.7M | 1.41 STRK |

- **Size cap: not an issue.** Every proof stays far under the 2^20-row cap and the gateway's
  480 KB limit. Proving takes 20–70 s on an M4 Pro.
- **Screening enforced as designed.** Deposit without attestation → `SCREENING_REQUIRED`;
  400 s old → `SCREENING_EXPIRED`; bad signature → `SCREENING_INVALID_SIGNATURE`. The attestation
  is not proof-bound: one proof served all four simulations.
- **Discovery over plain RPC works.** `ContractDiscoveryProvider`, with no indexer, found #0's
  0.5 STRK change note and #1's received 0.2 STRK note, so `user_sk` never leaves the machine.
- **Cost is the fixed proof verification**, about 78M L2 gas per proof-carrying tx. That is the
  dominant fee, so the relayer-fee withdraw (P3) must cover it, and fee headroom of about 3.2 STRK
  is needed at Sepolia prices to submit at all.
- **Timing:** proofs read state 12 blocks back, so a freshly deployed or registered account must
  wait about 12 blocks before its next proof.
- Spike scripts (TS SDK + local runner) are kept outside the repo; the Rust port re-derives each
  of these transactions as conformance vectors.

## 3. Phases

**Status (2026-10-06):** phases 0–5 are built in PR #39 (`docs/code/strk20.md`). Open: the
mainnet end-to-end (Starkscan or bramble-gateway deposits, AVNU relay), a live run of the
desktop Private tab, and the "Later" items.

### 0. De-risk (spikes, no user-facing change)
- Locally prove a real `compile_actions` virtual tx (register, then a transfer) against mainnet
  state, without submitting it. Measure rows per component against the cap, time and size; check
  it with `starknet_proof_verifier::verify_proof`.
- Confirm which proof version the networks accept for privacy txs (PROOF1 today).
- Mainnet pool class confirmed `0x6d163f2b…cf83` (2026-10-05), identical to the Sepolia pool's.

### 1. Keys and reads
- Re-pin krusty to a version with the scoped STRK20 derivation (overlaps #21).
- Note discovery and decryption locally over RPC, via StarkWare's `discovery-core` crate, so no
  indexer sees `user_sk`.
- Registration status; `wallet_strk20Balances`; a per-account "private balances" disclosure toggle.

### 2. Action compiler (Rust port)
- Port `client-actions`, `compiler`, `builders`, `proof-invocation-factory`, `serialization` from
  the SDK. Conformance vectors in Rust, re-checked by an independent JS verifier against the TS SDK.

### 3. Register, transfer, withdraw (local proof)
- Prove locally, then submit through the AVNU private paymaster (relayer-fee withdraw action
  appended), or return the payload unbroadcast (P3).

### 4. Deposit (remote proof)
- Starkscan adapter: `Idempotency-Key`, async job polling, `pollAfterSeconds`.
- **Persist the result before anything else.** The relay delivers a proof once and cannot
  re-serve it.
- Submit approve + `apply_actions` before the 300 s attestation window closes; refuse to submit an
  expired one.
- Distinct, non-retryable UX for a screening rejection (prover error `10000`); retry only on
  `prover_unavailable` with the same key; never resubmit on `unknown_delivery`.
- Bramble-gateway fallback with the same result handling.

### 5. Surface
- Dapp RPC aligned with bramble and upstream wallet-api: `wallet_strk20Balances`,
  `wallet_strk20PrepareInvoke`, `wallet_strk20InvokeTransaction`, a register extension; error
  codes 113/114/118/119/120 and a screening-rejected code.
- Desktop: settings (Starkscan key, AVNU key), activate private balances, shield / unshield /
  private send, phased job status, approvals that show exact actions and fees.
- Agent docs at `GET /`.

### Later
`OPEN` notes, `invoke`, `subaccount_invoke`, `wallet_strk20SubaccountCommitment`.

## 4. Risks and caveats

- **Deposits disclose `user_sk`** to the remote prover and relay: it is in the calldata. Inherent
  to the protocol; the deposit approval must say so.
- **Row cap.** `compile_actions` is EC-heavy; it may not fit PROOF1's 2^20 rows per component
  (#29). Phase 0 answers this.
- **Starkscan relay:** mainnet only, operator-issued keys, one-shot results; its docs say it is
  currently dormant (404).
- **Sepolia deposits** need an attestation from the Sepolia pool's screener key; Starkscan and the bramble gateway are mainnet-only, so a Sepolia prover endpoint is still to be found. Register, transfer and withdraw are proved locally and need none.
- Screening and sanctions behaviour is taken from the vendored service READMEs and Starkscan's
  public docs; it needs verification and legal review before anyone relies on it.
