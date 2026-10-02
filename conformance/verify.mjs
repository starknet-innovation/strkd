// Independent re-derivation of crates/wallet-core/tests/fixtures/conformance-vectors.json.
//
// strkd generates that fixture with krusty-kms. A fixture only ever checked
// against the code that produced it proves nothing, so this script recomputes
// every value with different, pinned libraries (see package.json):
//   seed → public key   @scure/bip39 + @scure/bip32 (BIP-32 path), starknet.js grindKey (EIP-2645)
//   public key → address starknet.js hash.calculateContractAddressFromHash — the call bramble makes
//   SNIP-12 digests      starknet.js typedData.getMessageHash
//   invoke V3 hashes     starknet.js hash.calculateInvokeTransactionHash
//
// Run: npm ci && npm run verify   (CI runs it on every pull request).
// Public data only: the mnemonic is the published BIP-39 all-"abandon" test vector.

import { readFileSync } from "node:fs";
import { mnemonicToSeedSync } from "@scure/bip39";
import { HDKey } from "@scure/bip32";
import { ec, hash, typedData, constants } from "starknet";

const FIXTURE = new URL("../crates/wallet-core/tests/fixtures/conformance-vectors.json", import.meta.url);
const doc = JSON.parse(readFileSync(FIXTURE, "utf8"));

let failures = 0;
let checks = 0;
function check(what, got, want) {
  checks++;
  if (BigInt(got) !== BigInt(want)) {
    failures++;
    console.error(`MISMATCH ${what}\n  fixture: ${want}\n  derived: ${got}`);
  }
}

// --- seed → public key → address ------------------------------------------------
const root = HDKey.fromMasterSeed(mnemonicToSeedSync(doc.mnemonic, ""));
for (const v of doc.vectors) {
  const where = `${v.domain} ${v.n} (${v.path})`;
  const starkKey = ec.starkCurve.grindKey(root.derive(v.path).privateKey);
  const pk = ec.starkCurve.getStarkKey(starkKey);
  check(`public key, ${where}`, pk, v.public_key);
  const addr = hash.calculateContractAddressFromHash(doc.salt, doc.class_hash, [pk], 0);
  check(`address, ${where}`, addr, v.address);
}

// --- SNIP-12 rev 1 message hashes ------------------------------------------------
for (const [i, v] of (doc.typed_data_vectors ?? []).entries()) {
  const h = typedData.getMessageHash(v.typed_data, v.account_address);
  check(`SNIP-12 digest #${i} (${v.typed_data.primaryType})`, h, v.message_hash);
}

// --- invoke V3 transaction hashes ------------------------------------------------
const CHAIN = { SN_SEPOLIA: constants.StarknetChainId.SN_SEPOLIA, SN_MAIN: constants.StarknetChainId.SN_MAIN };
const DA = { L1: 0, L2: 1 };
for (const v of doc.invoke_v3_vectors ?? []) {
  const rb = (r) => ({ max_amount: BigInt(r.max_amount), max_price_per_unit: BigInt(r.max_price_per_unit) });
  const h = hash.calculateInvokeTransactionHash({
    senderAddress: v.sender_address,
    version: "0x3",
    compiledCalldata: v.calldata,
    chainId: CHAIN[v.chain_id],
    nonce: v.nonce,
    accountDeploymentData: [],
    nonceDataAvailabilityMode: DA[v.nonce_data_availability_mode],
    feeDataAvailabilityMode: DA[v.fee_data_availability_mode],
    resourceBounds: {
      l1_gas: rb(v.resource_bounds.l1_gas),
      l2_gas: rb(v.resource_bounds.l2_gas),
      l1_data_gas: rb(v.resource_bounds.l1_data_gas),
    },
    tip: v.tip,
    paymasterData: [],
  });
  check(`invoke V3 hash (${v.chain_id})`, h, v.transaction_hash);
}

if (checks === 0) {
  console.error("no vectors checked — the fixture is empty or its shape changed");
  process.exit(1);
}
console.log(`${checks - failures}/${checks} conformance values reproduced independently`);
process.exit(failures ? 1 : 0);
