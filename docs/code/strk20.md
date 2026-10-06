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
| Settings | `prover::Strk20Config` | Per network: pool override, Starkscan key, deposit prover URL, AVNU key, paymaster URL. IPC-only, like the prover API key. |

## How an operation runs

1. **Discover** at the proving block (head − 12): registration, the user's
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

## Verification

- `crates/strk20/tests/fixtures/strk20-vectors.json` is checked by the crate
  and re-derived by `conformance/strk20.mjs` (starknet.js only, in CI). The
  official SDK reproduced it too, invocation included.
- `wallet-core/tests/strk20.rs` matches the viewing keys live pools registered.
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
