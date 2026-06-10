# `wallet-core` — Reference

The crate that owns all key material: BIP-39 mnemonics, derivation, Stark
signing, OpenZeppelin address computation, the encrypted vault, and the account
registry. All cryptography is delegated to `krusty-kms`; vault encryption uses
`argon2` + `aes-gcm`.

- **Source:** `crates/wallet-core/src/`
- **Design rationale:** [`spec/wallet-companion-spec.md`](../../spec/wallet-companion-spec.md)
  §5 (security), §6 (key model)
- **Security boundary:** see [architecture.md](./architecture.md#security-boundary-the-most-important-diagram)

> ⚠️ This crate handles secret material. Read
> [workflow §security gates](../project/workflow.md#security-gates-non-negotiable)
> before changing it. Tests and examples must use **throwaway test seeds only**.

## Module map

| Module | Purpose |
|---|---|
| `lib.rs` | Public surface + re-exports; `generate_mnemonic`, `validate_mnemonic`, `address_hex`. |
| `domain.rs` | The two derivation branches and their path/index constants. |
| `keys.rs` | Derivation, public key, OZ address, signing, SNIP-12 typed-data. |
| `tx.rs` | Entry-point selectors, multicall calldata encoding, invoke-V3 hash + signing. |
| `vault.rs` | Passphrase-encrypted vault (Argon2id → AES-256-GCM). |
| `accounts.rs` | Account registry + per-caller scoping. |
| `error.rs` | `CoreError` / `Result`; deliberately opaque to avoid leaking key detail. |

## Public API

### Top level (`wallet_core::`)

```rust
fn generate_mnemonic(word_count: usize) -> Result<String>   // 12 or 24 words
fn validate_mnemonic(phrase: &str) -> Result<()>
fn address_hex(f: &Felt) -> String                          // "0x" + 64 hex digits
```

Re-exported foreign types so callers needn't depend on krusty directly:
`Felt`, `ChainId`, `StarkSignature`.

### `domain` — derivation branches

```rust
enum Domain { User, Agent }

const STARKNET_COIN_TYPE: u32 = 9004;
const USER_ACCOUNT_INDEX:  u32 = 0;      // portable branch (mirrors Argent base)
const AGENT_ACCOUNT_INDEX: u32 = 0x41;   // reserved, segregated branch

impl Domain {
    const fn account_index(self) -> u32;
    const fn coin_type(self) -> u32;     // always 9004
    fn path(self, index: u32) -> String; // e.g. "m/44'/9004'/0'/0/3"
}
```

- **User** → `m/44'/9004'/0'/0/i`. Matches Argent's base path, so user accounts
  are intended to be portable into Argent/Braavos (claim pending the
  [portability test plan](../../spec/portability-test-plan.md)).
- **Agent** → `m/44'/9004'/0x41'/0/j`. A reserved hardened account index;
  mainstream wallets only scan `account' = 0'`, so agent accounts stay isolated.
  **`0x41` must never be reused for user accounts.**

### `keys` — derivation & signing

```rust
fn public_key(mnemonic: &str, domain: Domain, index: u32,
              passphrase: Option<&str>) -> Result<Felt>

fn oz_address(mnemonic: &str, domain: Domain, index: u32,
              passphrase: Option<&str>, chain: ChainId) -> Result<Felt>

fn sign_hash(mnemonic: &str, domain: Domain, index: u32,
             passphrase: Option<&str>, hash: &Felt) -> Result<StarkSignature>
```

- The private key is derived on demand, used, and dropped immediately; it is
  never returned or logged.
- `oz_address` uses `SaltPolicy::PublicKey` and the OZ class hash from krusty's
  per-network manifest.
- `sign_hash` signs a caller-supplied hash (a tx hash or a SNIP-12 typed-data
  hash). Stark ECDSA is RFC-6979 deterministic, so signing the same hash with
  the same key is reproducible — tests rely on this.
- **Memory-hygiene caveat:** `Felt` is `Copy` and cannot be zeroized in place.
  Keys are kept short-lived; threading krusty's `SecretFelt` through the signing
  path is the planned hardening (spec §5.2).

### `vault` — encrypted storage

```rust
struct EncryptedVault { version, kdf: KdfParams, nonce, ciphertext }  // serde-serializable

impl EncryptedVault {
    fn seal(passphrase: &str, plaintext: &[u8]) -> Result<Self>;
    fn open(&self, passphrase: &str) -> Result<Zeroizing<Vec<u8>>>;
    fn to_json(&self) -> Result<String>;
    fn from_json(s: &str) -> Result<Self>;
}
```

- **Content-agnostic:** the vault seals/opens arbitrary bytes. Higher layers
  serialize `{ mnemonic, Registry }` into those bytes (see the
  `seed_and_registry_seal_cycle` test for the canonical shape).
- KDF = Argon2id (params persisted in `KdfParams`); AEAD = AES-256-GCM with a
  random per-seal salt and nonce. Wrong passphrase or any tampering fails the
  GCM tag → `CoreError::BadPassphraseOrCorrupt`.
- Plaintext is returned in a `Zeroizing` buffer (wiped on drop).

### `tx` — invoke transactions

```rust
struct Call { to: Felt, selector: Felt, calldata: Vec<Felt> }
struct InvokeV3Params { nonce, tip, l1_gas, l2_gas, l1_data_gas, paymaster_data, account_deployment_data }
struct SignedInvoke { transaction_hash: Felt, calldata: Vec<Felt>, r: Felt, s: Felt }

fn starknet_keccak(data: &[u8]) -> Felt           // top 6 bits cleared (250-bit)
fn get_selector_from_name(name: &str) -> Felt     // = starknet_keccak(name)
fn resolve_selector(s: &str) -> Result<Felt>      // accepts "0x…" hex OR a name
fn encode_calls(calls: &[Call]) -> Vec<Felt>      // Cairo 1 __execute__ layout
fn invoke_v3_hash(sender, calls, chain, params) -> Felt
fn sign_invoke_v3(mnemonic, domain, index, passphrase, sender, calls, chain, params) -> Result<SignedInvoke>
```

- **Why this lives here:** krusty provides the V3 hash over a *pre-flattened*
  calldata array (`compute_invoke_v3_hash`) but no selector helper and no
  multicall encoder, so those two primitives are implemented locally.
- **Calldata layout (Cairo 1 / SNIP-6):**
  `[ n_calls, (to, selector, calldata_len, calldata…)* ]` — the format modern
  OZ/Argent/Braavos accounts expect.
- **Selector correctness** is pinned by a golden known-answer test:
  `get_selector_from_name("transfer")` ==
  `0x0083afd3…b482d12e` (the canonical ERC20 transfer selector).
- **Sign-only:** the hash is independent of the signature, so the returned
  `transaction_hash` is the hash the tx *will* have once broadcast with the
  same params. Broadcasting is a later (Phase 2) capability.

> ⚠️ **End-to-end hash acceptance is not yet proven.** The selector is
> golden-tested and krusty's V3 hash is parity-tested upstream, but that the
> composed hash is *accepted by a Starknet node* requires either a golden
> tx-hash vector or a Sepolia submission — that check belongs in the
> security-reviewed test plan, not the structural tests here.

### `accounts` — registry & scoping

```rust
struct AccountRef { domain, index, address, label, owner_client_id: Option<String> }
struct Registry { accounts: Vec<AccountRef> }

impl Registry {
    fn next_index(&self, domain: Domain) -> u32;
    fn add(&mut self, account: AccountRef);
    fn remove_by_address(&mut self, address: &str) -> bool;   // rollback support
    fn user_accounts(&self) -> impl Iterator<Item = &AccountRef>;
    fn scoped_for(&self, client_id: Option<&str>) -> Box<dyn Iterator<Item = &AccountRef> + '_>;
}
```

- `scoped_for` implements the spec §6.3 rule: an **agent client** (`Some(id)`)
  sees only the agent accounts it owns; a **user/app caller** (`None`) sees user
  accounts. This is what `wallet_requestAccounts` will return per caller.

### `error`

`CoreError` variants are intentionally coarse and never embed krusty error
detail, so key material can't leak into logs. `From<KmsError>` collapses to
`CoreError::Crypto`.

## Tests

| File | Covers |
|---|---|
| `tests/derivation.rs` (8) | mnemonic gen/validate, determinism, address well-formedness & distinctness, **user/agent domain isolation**, path layout, passphrase effect, deterministic signing bound to account. |
| `tests/vault.rs` (7) | seal/open round-trip, wrong passphrase, ciphertext ≠ plaintext, tamper detection, salt/nonce uniqueness, JSON persistence, full seed+registry seal cycle + scoping. |
| `tests/tx.rs` (6) | **golden `transfer` selector**, name/hex selector equivalence, Cairo 1 calldata layout (single + multi + empty), V3 hash determinism & chain-binding, signed-invoke binds signature to hash + account. |

All tests use the public BIP-39 test vector `abandon … about`. Assertions are
**structural**, not golden cross-wallet vectors — those require the
[portability test plan](../../spec/portability-test-plan.md).

Run them:
```bash
cargo test -p wallet-core
```

## Extending this crate

- New crypto capability → add a thin wrapper in the closest module, keep the key
  lifetime minimal, return only public data, and add tests with a test seed.
- Update this reference in the same change (see the
  [doc rules](../../README.md#contributing-to-the-documentation)).
- If a change could let key material escape `wallet-core`, stop and flag it as
  security-gated.
