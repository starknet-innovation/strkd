// Independent re-derivation of crates/strk20/tests/fixtures/strk20-vectors.json.
//
// strkd generates that fixture with its Rust port of StarkWare's privacy SDK.
// This script recomputes every value from the protocol definitions with pinned,
// unrelated libraries (starknet.js poseidon/curve/signing, @scure for the seed),
// following the formulas in the pool's hashes.cairo / utils.cairo and the SDK's
// proof-invocation-factory.ts:
//   hashes       poseidonHashMany([shortstring(TAG), ...inputs]), index followed by 0
//   encryption   ECDH x-coordinate on the Stark curve; field addition; amounts mod 2^128
//   invocation   pool.__execute__([compile_actions(user, user_sk, actions)]), V3 hash
//                with sender = pool, signed by the user's account key
//
// Run: npm ci && npm run verify:strk20   (CI runs it on every pull request).
// Public data only: the mnemonic is the published BIP-39 all-"abandon" test vector.

import { readFileSync } from "node:fs";
import { mnemonicToSeedSync } from "@scure/bip39";
import { HDKey } from "@scure/bip32";
import { ec, hash, num, shortString, constants, CallData } from "starknet";

const FIXTURE = new URL("../crates/strk20/tests/fixtures/strk20-vectors.json", import.meta.url);
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

const P = 2n ** 251n + 17n * 2n ** 192n + 1n; // felt252 field prime
const TWO_128 = 2n ** 128n;
const H = (tag, ...xs) =>
  BigInt(hash.computePoseidonHashOnElements([shortString.encodeShortString(tag), ...xs.map((x) => num.toHex(x))]));
const pub = (sk) => BigInt(ec.starkCurve.getStarkKey(num.toHex(sk)));
function sharedX(sk, publicX) {
  // The x-coordinate alone fixes the shared x: recover either point, multiply.
  const point = ec.starkCurve.ProjectivePoint.fromHex("02" + num.toHex(publicX).slice(2).padStart(64, "0"));
  return point.multiply(BigInt(sk)).toAffine().x;
}

// --- hashes (pool hashes.cairo) -------------------------------------------------
const i = Object.fromEntries(Object.entries(doc.inputs).map(([k, v]) => [k, BigInt(v)]));
const expected = {
  identity_key: H("IDENTITY_KEY_TAG:V1", i.a, i.b, i.c),
  enc_private_key_hash: H("ENC_PRIVATE_KEY_TAG:V1", i.a),
  enc_user_addr_hash: H("ENC_USER_ADDR_TAG:V1", i.a),
  enc_token_hash: H("ENC_TOKEN_TAG:V1", i.a, i.index, 0n, i.salt),
  enc_channel_key_hash: H("ENC_CHANNEL_KEY_TAG:V1", i.a),
  enc_sender_addr_hash: H("ENC_SENDER_ADDR_TAG:V1", i.a),
  enc_recipient_addr_hash: H("ENC_RECIPIENT_ADDR_TAG:V1", i.a, i.b, i.index, 0n, i.salt),
  channel_key: H("CHANNEL_KEY_TAG:V1", i.a, i.b, i.c, i.d),
  outgoing_channel_id: H("OUTGOING_CHANNEL_ID_TAG:V1", i.a, i.b, i.index, 0n),
  channel_marker: H("CHANNEL_MARKER_TAG:V1", i.a, i.b, i.c, i.d),
  subchannel_id: H("SUBCHANNEL_ID_TAG:V1", i.a, i.index, 0n),
  subchannel_marker: H("SUBCHANNEL_MARKER_TAG:V1", i.a, i.b, i.c, i.d),
  note_id: H("NOTE_ID_TAG:V1", i.a, i.c, i.index, 0n),
  enc_amount_hash: H("ENC_AMOUNT_TAG:V1", i.a, i.c, i.index, 0n, i.salt),
  nullifier: H("NULLIFIER_TAG:V1", i.a, i.c, i.index, 0n, i.b),
};
for (const [name, value] of Object.entries(expected)) check(`hash ${name}`, value, doc.hashes[name]);

// --- encryption (pool utils.cairo) ------------------------------------------------
const e = doc.encryption;
{
  const c = e.channel;
  const shared = sharedX(BigInt(c.ephemeral_sk), pub(BigInt(c.recipient_sk)));
  check("channel ephemeral pubkey", pub(BigInt(c.ephemeral_sk)), c.ephemeral_pubkey);
  check("channel enc_channel_key", (H("ENC_CHANNEL_KEY_TAG:V1", shared) + BigInt(c.channel_key)) % P, c.enc_channel_key);
  check("channel enc_sender_addr", (H("ENC_SENDER_ADDR_TAG:V1", shared) + BigInt(c.sender)) % P, c.enc_sender_addr);
  // and the recipient's side of the ECDH agrees
  check("channel ECDH symmetry", sharedX(BigInt(c.recipient_sk), BigInt(c.ephemeral_pubkey)), shared);

  const s = e.subchannel;
  check("subchannel enc_token", (H("ENC_TOKEN_TAG:V1", BigInt(s.channel_key), s.index, 0n, BigInt(s.salt)) + BigInt(s.token)) % P, s.enc_token);

  const n = e.note;
  const pad = H("ENC_AMOUNT_TAG:V1", BigInt(n.channel_key), BigInt(n.token), n.index, 0n, BigInt(n.salt));
  check("note packed_value", BigInt(n.salt) * TWO_128 + ((pad + BigInt(n.amount)) % TWO_128), n.packed_value);

  const o = e.outgoing;
  check("outgoing enc_recipient_addr", (H("ENC_RECIPIENT_ADDR_TAG:V1", BigInt(o.sender), BigInt(o.sender_sk), o.index, 0n, BigInt(o.salt)) + BigInt(o.recipient)) % P, o.enc_recipient_addr);
}

// --- proof invocation (SDK proof-invocation-factory.ts) ---------------------------
{
  const v = doc.invocation;
  const root = HDKey.fromMasterSeed(mnemonicToSeedSync("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about", ""));
  const sk = "0x" + ec.starkCurve.grindKey(root.derive("m/44'/9004'/0'/0/0").privateKey);
  const OZ = "0x01d1777db36cdd06dd62cfde77b1b6ae06412af95d57a13dc40ac77b8a702381";
  check("invocation user address", hash.calculateContractAddressFromHash(0, OZ, [ec.starkCurve.getStarkKey(sk)], 0), v.user);

  // Rebuild compile_actions calldata from the actions encoded in the fixture
  // (every ClientAction field is one felt), and the __execute__ wrapper around it.
  const inner = v.calldata.slice(4);
  check("invocation inner user_addr", inner[0], v.user);
  check("invocation inner user_sk", inner[1], v.user_sk);
  check("invocation wrapper", CallData.compile([[{ to: v.pool, selector: hash.getSelectorFromName("compile_actions"), calldata: inner }]]).length, v.calldata.length);
  check("invocation selector", v.calldata[2], hash.getSelectorFromName("compile_actions"));

  const txHash = hash.calculateInvokeTransactionHash({
    senderAddress: v.pool,
    version: "0x3",
    compiledCalldata: v.calldata,
    chainId: constants.StarknetChainId.SN_SEPOLIA,
    nonce: v.nonce,
    accountDeploymentData: [],
    nonceDataAvailabilityMode: 0,
    feeDataAvailabilityMode: 0,
    resourceBounds: {
      l1_gas: { max_amount: 1n, max_price_per_unit: 0n },
      l2_gas: { max_amount: 100_000_000n, max_price_per_unit: 0n },
      l1_data_gas: { max_amount: 1n, max_price_per_unit: 0n },
    },
    tip: 0n,
    paymasterData: [],
  });
  check("invocation transaction hash", txHash, v.transaction_hash);
  const sig = ec.starkCurve.sign(num.toHex(txHash), sk);
  check("invocation signature r", sig.r, v.signature[0]);
  check("invocation signature s", sig.s, v.signature[1]);
}

if (checks === 0) {
  console.error("no vectors checked — the fixture is empty or its shape changed");
  process.exit(1);
}
console.log(`${checks - failures}/${checks} strk20 values reproduced independently`);
process.exit(failures ? 1 : 0);
