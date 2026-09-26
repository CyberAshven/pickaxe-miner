// BCH 2026 VM proof for the PHOTON mining transaction layout at every age
// push width, and for the covenant's proof-of-work comparison.
//
// The covenant pushes the baton age as a minimal script number, so the
// mining transaction is 615 bytes for age 0..=16 and grows by one byte per
// extra number byte (616 for 17..=127, 617 for 128..=32767, 618 for
// 32768..=65534; the covenant rejects age >= 65535). The proof-of-work check
// is ABS(BIN2NUM(HASH256(tx))) < target, so bit 255 of the digest is ignored.
//
// Offline and deterministic: no network, no production key, no broadcast.
import { readFileSync } from 'node:fs';
import {
  createVirtualMachineBch2026,
  decodeTransactionBch,
  hash256,
  hexToBin,
  secp256k1,
  sha256,
} from '@bitauth/libauth';

const concat = (...parts) => {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
};
const reverse = (bytes) => Uint8Array.from(bytes).reverse();
const u32le = (v) => Uint8Array.of(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
const u64le = (v) => {
  const out = new Uint8Array(8);
  let x = BigInt(v);
  for (let i = 0; i < 8; i += 1) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  return out;
};
const compactUint = (value) => {
  const v = BigInt(value);
  if (v <= 0xfcn) return Uint8Array.of(Number(v));
  if (v <= 0xffffn) return concat(Uint8Array.of(0xfd), u64le(v).slice(0, 2));
  if (v <= 0xffffffffn) return concat(Uint8Array.of(0xfe), u64le(v).slice(0, 4));
  return concat(Uint8Array.of(0xff), u64le(v));
};
const numToLe32 = (n) => {
  const out = new Uint8Array(32);
  let x = n;
  for (let i = 0; i < 32; i += 1) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  if (x !== 0n || (out[31] & 0x80) !== 0) throw new Error('target does not fit a positive 32-byte number');
  return out;
};
const le32ToNum = (bytes) => bytes.reduceRight((acc, byte) => (acc << 8n) | BigInt(byte), 0n);

// Mirrors src/tx.rs encode_positive_script_number_push.
const agePush = (age) => {
  if (age === 0) return Uint8Array.of(0x00);
  if (age <= 16) return Uint8Array.of(0x50 + age);
  const bytes = [];
  let v = age;
  while (v > 0) {
    bytes.push(v & 0xff);
    v >>>= 8;
  }
  if (bytes[bytes.length - 1] & 0x80) bytes.push(0);
  return Uint8Array.of(bytes.length, ...bytes);
};

const read = (name) => readFileSync(new URL(`../../reference/${name}`, import.meta.url), 'utf8').trim();
const redeem = hexToBin(read('photon_redeem.hex'));
const vector = decodeTransactionBch(hexToBin(read('photon_vector_tx.hex')));
if (typeof vector === 'string') throw new Error(vector);
const covenantLock = vector.outputs[0].lockingBytecode;
const category = vector.outputs[0].token.category;
const payoutLock = hexToBin('76a9146e0810ceea13412b73feb41566a3d2d0ce54e10188ac');

// Mirrors src/tx.rs build_photon_template_bytes.
const buildMiningTx = ({ prevTxid, age, publicKey, target, signature, nonce, value, amount, reward }) => {
  const inputScript = concat(Uint8Array.of(0x21), publicKey, agePush(age), Uint8Array.of(0x4d, 0x03, 0x01), redeem);
  const commitment = concat(u32le(nonce), target, signature);
  const catRev = reverse(category);
  const output0 = concat(
    Uint8Array.of(0xef), catRev, Uint8Array.of(0x71), compactUint(commitment.length), commitment,
    compactUint(amount - reward), covenantLock,
  );
  const output1 = concat(Uint8Array.of(0xef), catRev, Uint8Array.of(0x10), compactUint(reward), payoutLock);
  return concat(
    u32le(2), Uint8Array.of(1), reverse(hexToBin(prevTxid)), u32le(0),
    compactUint(inputScript.length), inputScript, u32le(age), Uint8Array.of(2),
    u64le(value - 1500n), compactUint(output0.length), output0,
    u64le(700n), compactUint(output1.length), output1, u32le(0),
  );
};

// The mirror must reproduce the authoritative vector byte for byte.
{
  const commitment = vector.outputs[0].token.nft.commitment;
  const rebuilt = buildMiningTx({
    prevTxid: '000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042',
    age: 10,
    publicKey: hexToBin('0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798'),
    target: commitment.slice(4, 36),
    signature: commitment.slice(36, 100),
    nonce: 0x12345678,
    value: 15_971_500n,
    amount: 2_099_905_002_035_715n,
    reward: 4_999_773_813n,
  });
  if (Buffer.compare(Buffer.from(rebuilt), Buffer.from(hexToBin(read('photon_vector_tx.hex')))) !== 0) {
    throw new Error('JS mirror of the Rust PHOTON serializer drifted from the reference vector');
  }
}

const secret = hexToBin('11'.repeat(32));
const publicKey = secp256k1.derivePublicKeyCompressed(secret);
if (typeof publicKey === 'string') throw new Error(publicKey);
const vms = [createVirtualMachineBch2026(false), createVirtualMachineBch2026(true)];
const amount = 2_099_905_002_035_715n;
const reward = amount / 420000n;
const value = 15_971_500n;

// Optional independently reconstructed incremental-k GPU test vectors.
if (process.argv[2]) {
  const vectors = JSON.parse(readFileSync(process.argv[2], 'utf8'));
  let accepts = 0;
  for (const sample of vectors) {
    const transaction = decodeTransactionBch(hexToBin(sample.raw));
    if (typeof transaction === 'string') throw new Error(transaction);
    const source = {
      lockingBytecode: covenantLock, valueSatoshis: value,
      token: { category, amount, nft: {
        capability: 'mutable',
        commitment: concat(u32le(0), numToLe32(BigInt(`0x${sample.old_target}`)), new Uint8Array(64)),
      } },
    };
    for (const vm of vms) {
      const verdict = vm.verify({ sourceOutputs: [source], transaction });
      if ((verdict === true) !== sample.meets) {
        throw new Error(`incremental age=${sample.age}: expected=${sample.meets}, verdict=${verdict}`);
      }
    }
    if (sample.meets) accepts += 1;
  }
  if (!accepts || accepts === vectors.length) throw new Error('incremental vectors need wins and misses');
  console.log(`PASS incremental-k BCH 2026 VM: ${accepts} accepted, ${vectors.length - accepts} rejected, standard and consensus`);
}

const expectedBytes = (age) => 615 + agePush(age).length - 1;
const results = [];
for (const age of [10, 16, 17, 127, 128, 32767, 32768, 65534]) {
  // Easy target (~2^250) so the harness finds winners in a few dozen tries.
  const oldTarget = (1n << 250n) * 144n / BigInt(age + 143);
  const newTarget = (oldTarget * BigInt(age + 143)) / 144n;
  const targetBytes = numToLe32(newTarget);
  const source = {
    lockingBytecode: covenantLock,
    valueSatoshis: value,
    token: {
      category,
      amount,
      nft: { capability: 'mutable', commitment: concat(u32le(0), numToLe32(oldTarget), new Uint8Array(64)) },
    },
  };
  const found = { strict: null, topBit: null, miss: null };
  for (let nonce = 0; nonce < 5000 && !(found.strict && found.topBit && found.miss); nonce += 1) {
    const message = sha256.hash(concat(u32le(nonce), targetBytes));
    const signature = secp256k1.signMessageHashSchnorr(secret, message);
    if (typeof signature === 'string') throw new Error(signature);
    const bytes = buildMiningTx({
      prevTxid: 'aa'.repeat(32), age, publicKey, target: targetBytes, signature, nonce, value, amount, reward,
    });
    if (bytes.length !== expectedBytes(age)) {
      throw new Error(`age ${age}: built ${bytes.length} bytes, expected ${expectedBytes(age)}`);
    }
    const digest = hash256(bytes);
    const topBit = (digest[31] & 0x80) !== 0;
    const masked = Uint8Array.from(digest);
    masked[31] &= 0x7f;
    const meets = le32ToNum(masked) < newTarget;
    const slot = !meets ? 'miss' : topBit ? 'topBit' : 'strict';
    if (!found[slot]) found[slot] = bytes;
  }
  for (const [slot, bytes] of Object.entries(found)) {
    if (!bytes) throw new Error(`age ${age}: no ${slot} sample found`);
    const transaction = decodeTransactionBch(bytes);
    if (typeof transaction === 'string') throw new Error(transaction);
    for (const vm of vms) {
      const verdict = vm.verify({ sourceOutputs: [source], transaction });
      const accepted = verdict === true;
      if (accepted !== (slot !== 'miss')) {
        throw new Error(`age ${age} ${slot}: covenant ${accepted ? 'accepted' : `rejected (${verdict})`}`);
      }
    }
  }
  results.push(`age=${age} bytes=${expectedBytes(age)} strict=accept top_bit=accept miss=reject`);
}

// The covenant caps age below 65535.
{
  const age = 65535;
  const oldTarget = (1n << 250n) * 144n / BigInt(age + 143);
  const targetBytes = numToLe32((oldTarget * BigInt(age + 143)) / 144n);
  const message = sha256.hash(concat(u32le(0), targetBytes));
  const signature = secp256k1.signMessageHashSchnorr(secret, message);
  const transaction = decodeTransactionBch(buildMiningTx({
    prevTxid: 'aa'.repeat(32), age, publicKey, target: targetBytes, signature, nonce: 0, value, amount, reward,
  }));
  const source = {
    lockingBytecode: covenantLock,
    valueSatoshis: value,
    token: { category, amount, nft: { capability: 'mutable', commitment: concat(u32le(0), numToLe32(oldTarget), new Uint8Array(64)) } },
  };
  if (vms[0].verify({ sourceOutputs: [source], transaction }) === true) {
    throw new Error('covenant accepted age 65535');
  }
  results.push('age=65535 rejected');
}

console.log('PASS BCH 2026 VM PHOTON mining layout proof');
for (const line of results) console.log(line);
