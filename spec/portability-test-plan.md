# Derivation Portability — Test Plan

**Status:** To be executed by the team + reviewed by security
**Relates to:** `wallet-companion-spec.md` §6.1, §12, and Risk #2 (portability unverified)
**Date:** 2026-06-09

> 🔐 **Read first — this is RED-data work.** Executing this plan involves a mnemonic (seed phrase), derived private keys, and signatures. Run it **only** on an isolated/test machine, with **test-only** seeds, **never** with real funds or a production seed, and have the **security team review** the procedure and results. This document deliberately contains **no mnemonics, private keys, or derived secrets** — those are produced by the team during execution, not embedded here. (Note for tooling: an AI assistant cannot generate or process this key material; a human runs these steps.)

---

## 1. Why this exists

The whole account model sits on one assumption: that a "user" account derived by the companion is **portable** — re-importing the same mnemonic into Argent/Braavos surfaces the *same* account — and that the "agent" branch is **non-colliding** (never appears in those wallets). The spec marks this as **unverified** because:

- The companion (via `krusty-kms`) uses standard BIP-39 → BIP-32 (secp256k1) → EIP-2645 SHA-256 grind → STARK key. Argent's published recover tool routes the seed through `ethers` (`Wallet.fromMnemonic`); whether the resulting seed bytes are identical must be confirmed.
- Braavos's exact base derivation path / seed step was not verifiable from a primary source.

**Until these tests pass, do not claim portability anywhere in the product or docs.**

## 2. Scope of what's being compared

- **Public outputs only** are compared across wallets: account **addresses** (and optionally public keys). These are not secret.
- Mnemonics and private keys stay on the test machine and are never recorded in this doc or anywhere shared.

## 3. Reference paths under test

| Branch | Account *n* | Companion call (`krusty-kms`) |
|---|---|---|
| User | `m/44'/9004'/n'/0/0` | `derive_keypair_with_coin_type(mnemonic, index=0, account_index=n, coin_type=STARKNET_COIN_TYPE, passphrase)` |
| Agent | `m/44'/9004'/0x41474E54'/0/n` | `derive_keypair_with_coin_type(mnemonic, index=n, account_index=0x41474E54, coin_type=STARKNET_COIN_TYPE, passphrase)` |

> Updated by [#16](https://github.com/starknet-innovation/strkd/issues/16). The
> primary comparison target is now **bramble**, which shares this crypto: both
> wallets derive through `krusty-kms` with coin type 9004, so the keys should be
> bit-identical and only the address step could differ. Argent and Braavos remain
> secondary targets.

Address from public key: `OpenZeppelinAccount::latest(chain_id).deployment_descriptor(&pubkey, SaltPolicy::Zero)` → `.address`. The **zero** salt is what bramble uses; `user_account_zero_matches_brambles_address_formula` already pins account 0 against a starknet.js-computed value, so this plan is now about the seed→key step and the live round-trip rather than the address formula.

> Note: Argent/Braavos use **their own** account class hashes. A cross-wallet address match requires deriving the **same public key** *and* using the **same account class** the other wallet uses. So run the comparison in two layers (below): first the **public key** (pure derivation), then the **address** (derivation + matching account class).

---

## 4. Test cases

### T1 — User-branch public-key portability (Argent)
**Goal:** prove the companion derives the *same key* Argent does for `m/44'/9004'/0'/0/i`.
1. On the test machine, generate a **test-only** mnemonic using official, offline tooling.
2. In the companion, derive user accounts `i = 0..4`; record the **public keys**.
3. Using Argent's open-source recover tooling (`argentlabs/argent-starknet-recover`) with the same mnemonic, derive the public keys for the same indices.
4. **Assert:** companion public keys == Argent public keys for every index.

| Index `i` | Companion pubkey | Argent pubkey | Match? |
|---|---|---|---|
| 0 | _(fill)_ | _(fill)_ | ☐ |
| 1 | _(fill)_ | _(fill)_ | ☐ |
| 2 | _(fill)_ | _(fill)_ | ☐ |

### T2 — User-branch address portability (Argent, live)
**Goal:** end-to-end address match in a real wallet.
1. Import the same test mnemonic into **Argent on Sepolia**.
2. Record the account addresses Argent shows for indices `0..4`.
3. In the companion, compute addresses for the same indices **using Argent's account class** (not OZ) for this comparison only.
4. **Assert:** addresses match index-for-index.

*(If T1 passes but T2 fails only because of a different account class, portability of the **key** still holds; document that the companion's default OZ class yields a different address than Argent's class for the same key — expected, and fine, as long as the key is portable.)*

### T3 — Braavos portability (caveated)
Repeat T1/T2 against **Braavos**. Braavos's seed step is unconfirmed, so treat a mismatch here as "Braavos portability not supported in v1," not a blocker — but record the finding explicitly.

### T4 — Agent-branch non-collision
**Goal:** prove agent accounts never appear in mainstream wallets and never collide with user accounts.
1. In the companion, derive agent accounts `j = 0..4` (`account_index = 0x41`); record public keys/addresses.
2. Confirm Argent and Braavos, scanning the same mnemonic, **do not** surface any of these accounts (they only scan `account' = 0'`).
3. **Assert:** agent public keys are disjoint from the full user-branch set; no agent address appears in either wallet's account list.

### T5 — krusty-kms internal consistency (known-answer)
**Goal:** lock the derivation against regressions without depending on a live wallet.
1. Build an **independent reference** of the documented algorithm (BIP-39 → BIP-32 secp256k1 master `HMAC-SHA512("Bitcoin seed")` → path `m/44'/9004'/account'/0/index` → EIP-2645 SHA-256 rejection-sample grind to the STARK order → STARK pubkey), or use a trusted existing implementation.
2. Compare reference vs `krusty-kms` output for a fixed set of test vectors.
3. Capture the passing vectors as a committed **known-answer test** so future `krusty-kms` bumps are validated automatically.

### T6 — Edge cases
- 12-word vs 24-word mnemonics.
- BIP-39 passphrase present vs absent (must be threaded consistently into `mnemonic_to_seed`).
- Non-contiguous indices (gaps) in the account registry.

---

## 5. Pass criteria & sign-off

- **Required to claim portability:** T1 passes (key portability to Argent) and T4 passes (agent isolation).
- **Nice to have:** T2 (live address), T3 (Braavos), T5 (KAT committed), T6 (edge cases).
- Record outcomes below; **security team signs off** before the portability claim ships or before any Mainnet use.

| Test | Result | Date | Reviewer |
|---|---|---|---|
| T1 | ☐ pass / ☐ fail | | |
| T2 | ☐ pass / ☐ fail | | |
| T3 | ☐ pass / ☐ fail / ☐ n/a | | |
| T4 | ☐ pass / ☐ fail | | |
| T5 | ☐ pass / ☐ fail | | |
| T6 | ☐ pass / ☐ fail | | |

## 6. Dependencies to confirm during execution (from spec §14)
- `StarkSignature` field names and `compute_typed_data_message_hash` input shape.
- OZ class-hash manifest currency per network (`OzAccountClassConfig::latest`) for Sepolia + Mainnet.
- Whether krusty's BIP-39→seed equals Argent's `ethers` route (the crux of T1).

---
*Test plan for internal use — execute on isolated test infrastructure with test-only seeds, under security-team review. Crypto/derivation claims must be independently verified before relying on portability or touching Mainnet.*
