// Can a PHOTON claim carry an ELECTRIC commitment, AuxPoW style, under the
// live PHOTON v3.2 covenant? The claim pays its reward to a P2SH32 address
// whose redeem script starts with the commitment, so PHOTON's proof (the
// claim's HASH256) covers it. See docs/photon-electric-auxpow.md.
//
// Offline and deterministic: test keys, an easy starting target in a
// synthetic baton, no network and no broadcast. The covenant bytes are the
// mainnet ones; only the baton's starting target is easy, so no grinding.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  binToHex, createVirtualMachineBch2026, encodeDataPush, encodeTransactionBch, flattenBinArray,
  generateSigningSerializationBch, hash160, hash256, hexToBin, secp256k1, sha256, utf8ToBin,
} from '@bitauth/libauth';

const cat = (...parts) => flattenBinArray(parts.map((p) => Uint8Array.from(p)));
const u32le = (v) => Uint8Array.of(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff);
const numToLe32 = (n) => Uint8Array.from({ length: 32 }, (_, i) => Number((n >> BigInt(8 * i)) & 0xffn));
// BIN2NUM of the 32-byte digest: little-endian, bit 255 is the sign.
const digestNumber = (b) => {
  const n = b.reduceRight((acc, x) => (acc << 8n) | BigInt(x), 0n);
  return b[31] & 0x80 ? -(n & ((1n << 255n) - 1n)) : n;
};
const p2sh32 = (redeem) => cat([0xaa, 0x20], hash256(redeem), [0x87]);

const read = (path) => readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
const redeem = hexToBin(read('reference/photon_v32_redeem.hex').trim());
const protocol = read('src/protocol.rs');
const constant = (name) => protocol.match(new RegExp(`pub const ${name}: &str =\\s*"([0-9a-f]+)"`))[1];
const covenantLock = p2sh32(redeem);
assert.equal(binToHex(covenantLock), constant('COVENANT_LOCKING_BYTECODE_HEX'), 'not the live mainnet PHOTON covenant');
const category = hexToBin(constant('MAINNET_CATEGORY_HEX'));

const secret = sha256.hash(utf8ToBin('pickaxe photon auxpow test key'));
const pubkey = secp256k1.derivePublicKeyCompressed(secret);
const pkh = hash160(pubkey);
const p2pkh = cat([0x76, 0xa9, 0x14], pkh, [0x88, 0xac]);

// An illustrative ELECTRIC commitment: the aux contract's current state (here
// a stand-in baton outpoint) and the recipient, so a proof cannot be replayed
// on a later baton (the weakness in docs/electric-token.md).
const electricCommitment = hash256(cat(utf8ToBin('ELECTRIC-AUX'), hexToBin('cc'.repeat(32)), u32le(1), pkh));
// The payout redeem script: `<commitment> OP_DROP <pubkey> OP_CHECKSIG`.
const payoutRedeem = (commitment) => cat(encodeDataPush(commitment), [0x75], encodeDataPush(pubkey), [0xac]);
const auxPayout = p2sh32(payoutRedeem(electricCommitment));

const consensus = createVirtualMachineBch2026(false);
const standard = createVirtualMachineBch2026(true);
const verify = (vm, sourceOutputs, transaction) => vm.verify({ sourceOutputs, transaction });

// A synthetic baton at an easy target; age 1 keeps the target unchanged.
const age = 1;
const prevTarget = 1n << 254n;
const nextTarget = (prevTarget * BigInt(age + 143)) / 144n;
const value = 15_971_500n;
const amount = 2_099_905_002_035_715n;
const reward = amount / 420000n;
const baton = {
  lockingBytecode: covenantLock,
  valueSatoshis: value,
  token: { category, amount, nft: { capability: 'mutable', commitment: cat(u32le(0), numToLe32(prevTarget), new Uint8Array(64)) } },
};

// The claim the miner grinds: nonce and next target signed by the miner key,
// two outputs, the baton keeping at least value - 1500 sats.
const claim = (payoutLock, payoutSats) => {
  for (let nonce = 0; nonce < 10_000; nonce += 1) {
    const message = cat(u32le(nonce), numToLe32(nextTarget));
    const signature = secp256k1.signMessageHashSchnorr(secret, sha256.hash(message));
    const tx = {
      version: 2,
      locktime: 0,
      inputs: [{
        outpointTransactionHash: hexToBin('aa'.repeat(32)),
        outpointIndex: 0,
        sequenceNumber: age,
        unlockingBytecode: cat(encodeDataPush(pubkey), [0x50 + age], encodeDataPush(redeem)),
      }],
      outputs: [
        {
          lockingBytecode: covenantLock,
          valueSatoshis: value - 1500n,
          token: { category, amount: amount - reward, nft: { capability: 'mutable', commitment: cat(message, signature) } },
        },
        { lockingBytecode: payoutLock, valueSatoshis: payoutSats, token: { category, amount: reward } },
      ],
    };
    const proof = digestNumber(hash256(encodeTransactionBch(tx)));
    if (proof > 0n && proof < nextTarget) return tx;
  }
  throw new Error('no nonce met the easy target');
};

// 1. Control: today's P2PKH payout of 700 sats.
const control = claim(p2pkh, 700n);
assert.equal(verify(consensus, [baton], control), true);
assert.equal(verify(standard, [baton], control), true);

// 2. A P2SH32 payout carrying the commitment is valid PHOTON. At 700 sats it
// is consensus-valid but dust (a token P2SH32 output needs 705), so the
// payout must be raised; 800 sats fits the covenant's 1,500-sat allowance.
const dusty = claim(auxPayout, 700n);
assert.equal(verify(consensus, [baton], dusty), true);
assert.match(String(verify(standard, [baton], dusty)), /dust/);
const aux = claim(auxPayout, 800n);
assert.equal(verify(consensus, [baton], aux), true);
assert.equal(verify(standard, [baton], aux), true);
const auxBytes = encodeTransactionBch(aux);
const fee = value - aux.outputs.reduce((sum, o) => sum + o.valueSatoshis, 0n);
assert.ok(fee >= BigInt(auxBytes.length), `fee ${fee} below 1 sat/byte for ${auxBytes.length} bytes`);

// 3. The commitment is inside PHOTON's proof: another commitment, another hash.
const other = structuredClone(aux);
other.outputs[1].lockingBytecode = p2sh32(payoutRedeem(hash256(utf8ToBin('another commitment'))));
assert.notEqual(binToHex(hash256(encodeTransactionBch(other))), binToHex(hash256(auxBytes)));

// 4. The miner still spends the PHOTON reward; another key cannot.
const sources = [aux.outputs[1], { lockingBytecode: p2pkh, valueSatoshis: 10_000n }];
const spend = {
  version: 2,
  locktime: 0,
  inputs: [
    { outpointTransactionHash: hash256(auxBytes).reverse(), outpointIndex: 1, sequenceNumber: 0, unlockingBytecode: Uint8Array.of() },
    { outpointTransactionHash: hexToBin('bb'.repeat(32)), outpointIndex: 0, sequenceNumber: 0, unlockingBytecode: Uint8Array.of() },
  ],
  outputs: [{ lockingBytecode: p2pkh, valueSatoshis: 10_000n, token: { category, amount: reward } }],
};
const sign = (index, coveredBytecode, key) => cat(secp256k1.signMessageHashSchnorr(key, hash256(
  generateSigningSerializationBch(
    { inputIndex: index, sourceOutputs: sources, transaction: spend },
    { coveredBytecode, signingSerializationType: Uint8Array.of(0x41) },
  ),
)), [0x41]);
const spendWith = (key) => {
  const tx = structuredClone(spend);
  tx.inputs[0].unlockingBytecode = cat(encodeDataPush(sign(0, payoutRedeem(electricCommitment), key)),
    encodeDataPush(payoutRedeem(electricCommitment)));
  tx.inputs[1].unlockingBytecode = cat(encodeDataPush(sign(1, p2pkh, secret)), encodeDataPush(pubkey));
  return tx;
};
assert.equal(verify(standard, sources, spendWith(secret)), true);
assert.notEqual(verify(consensus, sources, spendWith(sha256.hash(utf8ToBin('someone else')))), true);

console.log(`PASS PHOTON AuxPoW payout: the live v3.2 covenant accepts a ${auxBytes.length}-byte claim paying ` +
  'an 800-sat P2SH32 payout that carries an ELECTRIC commitment inside the proof hash (700 sats is dust); ' +
  'only the miner key spends the reward');
