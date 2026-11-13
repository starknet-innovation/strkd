# STRK20 (privacy pool)

strkd's support for the Starknet privacy pool, on wallet-API 0.10.4's
`wallet_strk20*` surface. The plan, decisions and phase-0 measurements are in
[`project/strk20-plan.md`](../project/strk20-plan.md); this page describes the code.

## Where it lives

| Piece | Path | What |
|---|---|---|
| Viewing key | `wallet-core::strk20_viewing_key` | Pool-scoped `user_sk`, bramble's derivation via krusty. Same seed → same notes in both wallets. |
| Protocol client | `crates/strk20` | Pure Rust port of the wallet side of StarkWare's SDK: hashes, decryption, client actions, the proof invocation, discovery, planner, `apply_actions`. No I/O. |
| RPC + desktop | `crates/wallet-rpc/src/strk20/` | The `wallet_strk20*` handlers and `companion_strk20Register`, the deposit provers (`remote.rs`), the AVNU private relay (`paymaster.rs`). |
| Desktop | `desktop/src/components/Privacy.tsx` | The Private tab; Tauri commands `strk20_*` run the same handlers as `Caller::Desktop`. |
| Settings | `prover::Strk20Config` | Per network: pool override, Starkscan key, deposit prover URL, AVNU key, paymaster URL, and (testnet only) a test pool's screener key. IPC-only, like the prover API key. |
| Pool settings + local screener | `crates/wallet-rpc/src/strk20/screener.rs`, `strk20::screening` | Settings validation, the Settings → STRK20 pool check, and testnet deposit attestations signed with a self-deployed pool's screener key. |

## Which pool

Each network uses the pool set in Settings → STRK20, else its canonical pool
(`MAINNET_POOL`, `SEPOLIA_POOL`). `ctx()` resolves it once per call, so every
method (register, balances, prepare, invoke) reads, proves against and submits
to the same pool, and the viewing key is derived for that pool. Callers can't
choose it: a `pool_address` param only asserts the expected pool, and a
mismatch is `114` before anything runs (a paired agent can't point strkd at an
arbitrary contract). Approval prompts name the pool and flag a non-canonical
one as `NON-CANONICAL pool 0x…, set in Settings → STRK20`. The desktop's
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

**Prover errors.** A local proving failure comes back as `163` (unknown error) with the
prover's own error, bounded to about 4,000 chars (head and tail kept, cut
between words) and scrubbed of the viewing key. When the virtual transaction
reverts, the message carries the full revert reason
(`prover::snip36::classify_prover_error`).

## Verification

- `crates/strk20/tests/fixtures/strk20-vectors.json` is checked by the crate
  and re-derived by `conformance/strk20.mjs` (starknet.js only, in CI). The
  official SDK reproduced it too, invocation included.
- `wallet-core/tests/strk20.rs` matches the viewing keys live pools registered.
- Live on Sepolia, against strkd's test pool (mainnet class, test screener):
  `crates/strk20/examples/sepolia_e2e.rs` (crate only) and
  `crates/wallet-rpc/examples/strk20_live.rs` (the RPC handlers; its
  `prepare-deposit` mode proves a locally screened deposit and only estimates
  the submission). Results are recorded in the plan.
- `crates/strk20/tests/screening.rs` checks attestations against
  starknet-privacy's screening vectors; `crates/wallet-rpc/tests/strk20_pool.rs`
  covers pool selection, prompts, the `pool_address` assertion and the local
  screener end to end against a mock node.

## Not yet

- Open notes (`"OPEN"`), `invoke`, `shadow_account_invoke` and
  `wallet_strk20ShadowAccountCommitment` (`-32601`).
- Live runs of Starkscan, the bramble gateway and the AVNU relay: mainnet-only
  or key-gated.
- Deposits batched with private actions (AVNU's `invoke_and_apply_action`
  route).
