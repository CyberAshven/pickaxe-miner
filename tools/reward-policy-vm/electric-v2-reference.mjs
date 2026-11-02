// #### PR #71: future-token reference only; the live ELECTRIC miner stays v1.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import {
  assembleBytecodeBCH, bigIntToVmNumber, binToHex, createVirtualMachineBch2026,
  encodeDataPush, flattenBinArray, hash256, hexToBin, swapEndianness,
} from '@bitauth/libauth';

const artifact = JSON.parse(readFileSync(new URL('../../reference/electric-v2/PoWMineStatefulV2.json', import.meta.url)));
assert.equal(artifact.revision, '2a8a9cfba0fa2d6a3e571c236e912fa6e2ef5766');
assert.equal(artifact.compiler.version, '0.13.3');
assert.equal(artifact.fingerprint, 'dc05e9eb8a7956dcb6b6ff9d259528590f4dc8f2255e185aa6b4d03be08a590c');
const assembly = assembleBytecodeBCH(artifact.bytecode.split(' ')
  .map(token => token.startsWith('OP_') ? token : `<0x${token}>`).join(' '));
assert.equal(assembly.success, true);
assert.equal(binToHex(assembly.bytecode), artifact.bytecodeHex);
const category = '24'.repeat(32); // synthetic; never the live ELECTRIC category
const reward = 100000000n;
const redeem = flattenBinArray([
  encodeDataPush(bigIntToVmNumber(3n)), encodeDataPush(bigIntToVmNumber(1n)),
  encodeDataPush(bigIntToVmNumber(reward)), encodeDataPush(hexToBin(swapEndianness(category))),
  assembly.bytecode,
]);
const lockingBytecode = flattenBinArray([Uint8Array.of(0xaa, 0x20), hash256(redeem), Uint8Array.of(0x87)]);
const pkh = new Uint8Array(20).fill(0x11);
const p2pkh = bytes => flattenBinArray([hexToBin('76a914'), bytes, hexToBin('88ac')]);
const commitment = (count = 0, height = 100, difficulty = 1, reserved = new Uint8Array(7)) => {
  const bytes = new Uint8Array(16);
  bytes[0] = difficulty;
  const view = new DataView(bytes.buffer);
  view.setUint32(1, height, true);
  view.setUint32(5, count, true);
  bytes.set(reserved, 9);
  return bytes;
};
const nonceBytes = nonce => {
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, BigInt(nonce), true);
  return bytes;
};
const doubleHash = bytes => createHash('sha256').update(createHash('sha256').update(bytes).digest()).digest();
const proof = (state, recipient, nonce, legacy = false) =>
  doubleHash(Buffer.concat([legacy ? state.slice(1, 5) : state, recipient, nonceBytes(nonce)]));
const find = (state, recipient, legacy = false, reject = () => false) => {
  for (let nonce = 0; nonce < 1000000; nonce++) {
    if (proof(state, recipient, nonce, legacy).subarray(0, state[0]).every(b => b === 0) && !reject(nonce)) return nonce;
  }
  throw new Error('bounded reference nonce search exhausted');
};
function fixture({state = commitment(), recipient = pkh, locktime = 101, nonce} = {}) {
  nonce ??= find(state, recipient);
  const view = new DataView(state.buffer, state.byteOffset, state.byteLength);
  const next = commitment(view.getUint32(5, true) + 1, locktime,
    locktime > view.getUint32(1, true) ? 1 : Math.min(state[0] + 1, 3));
  const sourceOutputs = [{
    lockingBytecode, valueSatoshis: 1800n,
    token: { category: hexToBin(category), amount: reward * 100n,
      nft: { capability: 'mutable', commitment: state } },
  }, {lockingBytecode: Uint8Array.of(0x51), valueSatoshis: 10000n}];
  const transaction = {
    version: 2, locktime,
    inputs: [{outpointTransactionHash: new Uint8Array(32).fill(0x42), outpointIndex: 0,
      sequenceNumber: 0xfffffffe,
      unlockingBytecode: flattenBinArray([encodeDataPush(nonceBytes(nonce)), encodeDataPush(recipient), encodeDataPush(redeem)])},
    {outpointTransactionHash: new Uint8Array(32).fill(0x43), outpointIndex: 0,
      sequenceNumber: 0xfffffffe, unlockingBytecode: new Uint8Array()}],
    outputs: [{lockingBytecode: p2pkh(recipient), valueSatoshis: 1000n,
      token: {category: hexToBin(category), amount: reward}},
    {lockingBytecode, valueSatoshis: 1800n, token: {category: hexToBin(category), amount: reward * 99n,
      nft: {capability: 'mutable', commitment: next}}},
    {lockingBytecode: p2pkh(new Uint8Array(20).fill(0x44)), valueSatoshis: 8000n}],
  };
  return {transaction, sourceOutputs};
}
let valid = 0, refused = 0;
for (const standard of [false, true]) {
  const vm = createVirtualMachineBch2026(standard);
  const accept = (name, sample) => { assert.equal(vm.verify(sample), true, name); valid++; };
  const reject = (name, sample) => { const result = vm.verify(sample); assert.notEqual(result, true, name); refused++; };
  accept('new height', fixture());
  accept('all prior reserved bytes hashed', fixture({state: commitment(7, 100, 1, Uint8Array.of(1,2,3,4,5,6,7))}));
  accept('last encodable positive counter advance', fixture({state: commitment(0x7ffffffe)}));
  for (const recipient of [new Uint8Array(20).fill(0x22), new Uint8Array(20).fill(0x33)])
    accept('independent work-recipient reward', fixture({recipient}));
  const three = fixture();
  three.transaction.inputs.push({...structuredClone(three.transaction.inputs[1]), outpointIndex: 1});
  three.sourceOutputs.push(structuredClone(three.sourceOutputs[1]));
  three.transaction.outputs[2].valueSatoshis += 10000n;
  accept('optional third plain funding input', three);
  const state = commitment();
  const oldNonce = find(state, pkh, true, n => proof(state, pkh, n)[0] === 0);
  reject('v1 height-only proof is incompatible', fixture({state, nonce: oldNonce}));
  const successor = commitment(1, 100, 2);
  const replayNonce = find(state, pkh, false, n => proof(successor, pkh, n).subarray(0, 2).every(b => b === 0));
  const first = fixture({state, locktime: 100, nonce: replayNonce});
  accept('same height', first);
  assert.equal(binToHex(first.transaction.outputs[1].token.nft.commitment), binToHex(successor));
  reject('prior proof does not unlock its actual same-height successor', fixture({state: successor, locktime: 100, nonce: replayNonce}));
  for (const [name, mutate] of [
    ['different recipient', s => {s.transaction.outputs[0].lockingBytecode[3] ^= 1;}],
    ['different reward', s => {s.transaction.outputs[0].token.amount += 1n;}],
    ['immutable continuation', s => {s.transaction.outputs[1].token.nft.capability = 'none';}],
    ['continuation category', s => {s.transaction.outputs[1].token.category[0] ^= 1;}],
    ['reserved commitment byte', s => {s.sourceOutputs[0].token.nft.commitment[15] ^= 1;}],
    ['counter commitment byte', s => {s.sourceOutputs[0].token.nft.commitment[5] ^= 1;}],
    ['final sequence', s => {s.transaction.inputs[0].sequenceNumber = 0xffffffff;}],
    ['seven-byte nonce', s => {const u = s.transaction.inputs[0].unlockingBytecode;
      s.transaction.inputs[0].unlockingBytecode = flattenBinArray([Uint8Array.of(7), u.slice(1,8), u.slice(9)]);}],
    ['missing funding input', s => {s.transaction.inputs.pop();s.sourceOutputs.pop();}],
    ['four inputs', s => {for(let i=1;i<=2;i++){s.transaction.inputs.push({...structuredClone(s.transaction.inputs[1]),outpointIndex:i});s.sourceOutputs.push(structuredClone(s.sourceOutputs[1]));s.transaction.outputs[2].valueSatoshis+=10000n;}}],
    ['token-bearing change', s => {s.transaction.outputs[2].token = {category:hexToBin(category),amount:1n};s.transaction.outputs[1].token.amount-=1n;}],
  ]) { const sample = fixture(); mutate(sample); reject(name, sample); }
  reject('next counter cannot fit a positive four-byte Script number', fixture({state: commitment(0x7fffffff)}));
  reject('negative decoded counter', fixture({state: commitment(0xffffffff)}));
  reject('timestamp locktime is not a height', fixture({locktime: 500000000}));
}
assert.equal(valid, 14);
assert.equal(refused, 32);
console.log(`PASS future ELECTRIC v2 reference: ${valid} intended spends, ${refused} rejected mutations; live v1 miner unchanged.`);
