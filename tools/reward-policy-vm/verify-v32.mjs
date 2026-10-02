#!/usr/bin/env node
// Verifies the INACTIVE PHOTON v3.2 contract fixture.
//
// Fixture provenance (all three files are inert reference data; nothing here
// touches the miner, a mainnet category, or the network):
//   reference/photon_v32_template.json
//     byte-identical copy of the official template pinned at 2qx/vox commit
//     cb11abc790a7, packages/photon/src/template.v3.2.json
//   reference/photon_v32_redeem.hex
//     the 273-byte compiled `lock` script (lockingType p2sh32) of that
//     template, lowercase hex + trailing newline
//
// This verifier is deliberately dependency-free: it imports only Node built-ins
// (node:assert, node:crypto, node:fs, node:path, node:url) so it runs in the
// isolated Task MCP sandbox, which has no node_modules and where an `npm ci`
// preflight is blocked (NF-2026-00010). It therefore does NOT compile the
// template -- compilation against @bitauth/libauth is performed independently
// outside isolated validation. Instead it re-derives every published artifact
// from the fixture bytes and asserts each one, which pins the pair without
// needing a compiler.

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Published, independently compiled expectations for PHOTON v3.2. */
const EXPECTED = {
  /** sha256 of reference/photon_v32_template.json (upstream bytes). */
  templateSha256:
    'c9d58afe205c5c51fb925a069d0c1eb836e2d0e0e3d0273a82bbf6894f8211cf',
  /** sha256 of reference/photon_v32_redeem.hex (the text file itself). */
  redeemFileSha256:
    'f2c4870fd115c13dd40e4977a1f5f1040819a50b0f4e6706a4774f475ee2bd78',
  /** Compiled `lock` script length in bytes. */
  redeemLength: 273,
  /** P2SH32 locking bytecode: OP_HASH256 <32> sha256d(redeem) OP_EQUAL. */
  lockingBytecode:
    'aa200ab476a6dab00ba11118a11c078f871c5060a66e18140368425202772d0e80c287',
  /** Electrum `blockchain.scripthash.*` key: sha256(lock), byte-reversed. */
  electrumScriptHash:
    '0cea1bfb91d50a9a3fb88bdf4f9d466c15738d7feba0873a5e85313d42035261',
  /** Published Chipnet address (token-aware P2SH32). */
  chipnetAddress:
    'bchtest:rv9tga4xm2cqhgg3rzs3cpu0suw9qc9xdcvpgqmggffqyaedp6qvysm43tgje',
};

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const TEMPLATE_PATH = join(REPO_ROOT, 'reference', 'photon_v32_template.json');
const REDEEM_PATH = join(REPO_ROOT, 'reference', 'photon_v32_redeem.hex');

const sha256 = (bytes) => createHash('sha256').update(bytes).digest();
const sha256d = (bytes) => sha256(sha256(bytes));

const checks = [];

/** Assert one named expectation and record it for the summary. */
function check(name, actual, expected) {
  assert.equal(actual, expected, `${name}\n  expected ${expected}\n  actual   ${actual}`);
  checks.push(name);
}

// --- CashAddr (token-aware P2SH32) -----------------------------------------
// Version byte packs a 4-bit type and a 3-bit hash-size code. CashTokens adds
// token-aware types, so a token-aware P2SH (type 3) with a 32-byte hash
// (size code 3) is (3 << 3) | 3 = 0x1b -- this is why the published address
// begins with `r` rather than the `p` of a plain P2SH.
const CASHADDR_TOKEN_AWARE_P2SH32_VERSION = 0x1b;
const CASHADDR_CHARSET = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l';
const CASHADDR_GENERATOR = [
  0x98f2bc8e61n,
  0x79b76d99e2n,
  0xf33e5fb3c4n,
  0xae2eabe2a8n,
  0x1e4f43e470n,
];

/** BCH 40-bit CashAddr checksum over 5-bit groups. */
function cashAddrPolymod(groups) {
  let checksum = 1n;
  for (const group of groups) {
    const carry = checksum >> 35n;
    checksum = ((checksum & 0x07ffffffffn) << 5n) ^ BigInt(group);
    for (let bit = 0; bit < CASHADDR_GENERATOR.length; bit += 1) {
      if ((carry >> BigInt(bit)) & 1n) checksum ^= CASHADDR_GENERATOR[bit];
    }
  }
  return checksum ^ 1n;
}

/** Regroup bytes into 5-bit values, zero-padding the final group. */
function toGroups5(bytes) {
  const groups = [];
  let accumulator = 0;
  let bits = 0;
  for (const byte of bytes) {
    accumulator = (accumulator << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      groups.push((accumulator >> bits) & 31);
    }
  }
  if (bits > 0) groups.push((accumulator << (5 - bits)) & 31);
  return groups;
}

function encodeCashAddr(prefix, version, hash) {
  const payload = toGroups5(Buffer.concat([Buffer.from([version]), hash]));
  const prefixGroups = [...prefix].map((character) => character.charCodeAt(0) & 31);
  const checksum = cashAddrPolymod([...prefixGroups, 0, ...payload, 0, 0, 0, 0, 0, 0, 0, 0]);
  const checksumGroups = [];
  for (let index = 0; index < 8; index += 1) {
    checksumGroups.push(Number((checksum >> BigInt(5 * (7 - index))) & 31n));
  }
  const body = [...payload, ...checksumGroups].map((group) => CASHADDR_CHARSET[group]).join('');
  return `${prefix}:${body}`;
}

// --- Checks ----------------------------------------------------------------

// 1. The template is the official upstream file, byte for byte.
const templateBytes = readFileSync(TEMPLATE_PATH);
check('template byte sha256', sha256(templateBytes).toString('hex'), EXPECTED.templateSha256);

// The template must still describe a p2sh32 `lock`; that is what the redeem
// fixture is the compilation of.
const template = JSON.parse(templateBytes.toString('utf8'));
check('template lock lockingType', template.scripts?.lock?.lockingType, 'p2sh32');

// 2. The redeem fixture is the pinned text file, and decodes to 273 bytes.
const redeemFileBytes = readFileSync(REDEEM_PATH);
check('redeem file sha256', sha256(redeemFileBytes).toString('hex'), EXPECTED.redeemFileSha256);

const redeemHex = redeemFileBytes.toString('utf8').trim();
assert.match(redeemHex, /^[0-9a-f]+$/, 'redeem fixture must be lowercase hex');
assert.equal(redeemHex.length % 2, 0, 'redeem fixture must hold whole bytes');
const redeem = Buffer.from(redeemHex, 'hex');
check('redeem length (bytes)', redeem.length, EXPECTED.redeemLength);

// 3. P2SH32 locking bytecode derived from sha256d(redeem).
const redeemHash = sha256d(redeem);
const lockingBytecode = Buffer.concat([
  Buffer.from([0xaa, 0x20]), // OP_HASH256 OP_PUSHBYTES_32
  redeemHash,
  Buffer.from([0x87]), // OP_EQUAL
]);
check('p2sh32 locking bytecode', lockingBytecode.toString('hex'), EXPECTED.lockingBytecode);

// 4. Electrum script hash: sha256 of the locking bytecode, byte-reversed.
const electrumScriptHash = Buffer.from(sha256(lockingBytecode)).reverse();
check('electrum script hash', electrumScriptHash.toString('hex'), EXPECTED.electrumScriptHash);

// 5. Published Chipnet address.
const address = encodeCashAddr('bchtest', CASHADDR_TOKEN_AWARE_P2SH32_VERSION, redeemHash);
check('chipnet address', address, EXPECTED.chipnetAddress);

for (const name of checks) console.log(`ok  ${name}`);
console.log(`\nPHOTON v3.2 fixture verified (${checks.length} checks, inactive reference data only).`);
