import { readFileSync } from 'node:fs';
import {
  binToHex,
  cashAddressToLockingBytecode,
  createVirtualMachineBch2026,
  decodeTransactionBch,
  encodeTransactionBch,
  generateTransaction,
  hash256,
  hexToBin,
  importWalletTemplate,
  stringifyDebugTraceSummary,
  summarizeDebugTrace,
  verifyTransactionTokens,
  walletTemplateP2pkhNonHd,
  walletTemplateToCompilerBch,
} from '@bitauth/libauth';

const DONATION_ADDRESS =
  'bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3';
const DONATION_BPS = 200n;
const TOKEN_OUTPUT_SATS = 700n;
const SPONSOR_SUBSIDY_SATS = 2000n;
const SPONSOR_RESERVE_SATS = 100000n;
const PRIVATE_KEY = hexToBin('00'.repeat(31) + '01');
const REWARD_LOCK = hexToBin(
  '76a914751e76e8199196d454941c45d1b3a323f1433bd688ac',
);

const op = Object.freeze({
  OP_0: 0x00,
  OP_IF: 0x63,
  OP_ELSE: 0x67,
  OP_ENDIF: 0x68,
  OP_TOALTSTACK: 0x6b,
  OP_FROMALTSTACK: 0x6c,
  OP_DROP: 0x75,
  OP_DUP: 0x76,
  OP_NIP: 0x77,
  OP_SWAP: 0x7c,
  OP_CAT: 0x7e,
  OP_SPLIT: 0x7f,
  OP_EQUALVERIFY: 0x88,
  OP_SUB: 0x94,
  OP_MUL: 0x95,
  OP_DIV: 0x96,
  OP_NUMEQUALVERIFY: 0x9d,
  OP_HASH160: 0xa9,
  OP_HASH256: 0xaa,
  OP_CHECKSIG: 0xac,
  OP_INPUTINDEX: 0xc0,
  OP_ACTIVEBYTECODE: 0xc1,
  OP_TXINPUTCOUNT: 0xc3,
  OP_TXOUTPUTCOUNT: 0xc4,
  OP_UTXOVALUE: 0xc6,
  OP_OUTPOINTTXHASH: 0xc8,
  OP_OUTPOINTINDEX: 0xc9,
  OP_OUTPUTVALUE: 0xcc,
  OP_OUTPUTBYTECODE: 0xcd,
  OP_UTXOTOKENAMOUNT: 0xd0,
  OP_OUTPUTTOKENAMOUNT: 0xd3,
});

const concat = (...parts) => {
  const length = parts.reduce((sum, part) => sum + part.length, 0);
  const out = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
};

const reverse = (bytes) => Uint8Array.from(bytes).reverse();

const pushData = (data) => {
  if (data.length <= 75) return concat(Uint8Array.of(data.length), data);
  if (data.length <= 0xff) return concat(Uint8Array.of(0x4c, data.length), data);
  if (data.length <= 0xffff) {
    return concat(
      Uint8Array.of(0x4d, data.length & 0xff, (data.length >>> 8) & 0xff),
      data,
    );
  }
  throw new Error('push exceeds proof harness limit');
};

const scriptNumberBytes = (value) => {
  let n = BigInt(value);
  if (n === 0n) return new Uint8Array();
  const negative = n < 0n;
  if (negative) n = -n;
  const bytes = [];
  while (n > 0n) {
    bytes.push(Number(n & 0xffn));
    n >>= 8n;
  }
  if ((bytes.at(-1) & 0x80) !== 0) bytes.push(negative ? 0x80 : 0x00);
  else if (negative) bytes[bytes.length - 1] |= 0x80;
  return Uint8Array.from(bytes);
};

const pushNumber = (value) => {
  const n = BigInt(value);
  if (n === 0n) return Uint8Array.of(op.OP_0);
  if (n >= 1n && n <= 16n) return Uint8Array.of(0x50 + Number(n));
  return pushData(scriptNumberBytes(n));
};

const ops = (...names) => Uint8Array.from(names.map((name) => op[name]));

const decodeP2pkh = (address) => {
  const decoded = cashAddressToLockingBytecode(address);
  if (typeof decoded === 'string') throw new Error(decoded);
  if (decoded.bytecode.length !== 25) throw new Error('expected P2PKH locking bytecode');
  return decoded.bytecode;
};

const buildSponsorScript = ({ expectedBatonHash, ownerLock, siteBps, rewardSats }) => {
  const ownerPkh = ownerLock.slice(3, 23);
  const statePrefix = pushData(expectedBatonHash);
  if (statePrefix.length !== 33) throw new Error('invalid state prefix');
  return concat(
    statePrefix,
    ops('OP_TOALTSTACK', 'OP_IF'),
    ops('OP_FROMALTSTACK', 'OP_DROP', 'OP_DUP', 'OP_HASH160'),
    pushData(ownerPkh),
    ops('OP_EQUALVERIFY', 'OP_CHECKSIG', 'OP_ELSE'),
    ops('OP_INPUTINDEX'), pushNumber(1), ops('OP_NUMEQUALVERIFY', 'OP_TXINPUTCOUNT'),
    pushNumber(2), ops('OP_NUMEQUALVERIFY', 'OP_TXOUTPUTCOUNT'), pushNumber(3), ops('OP_NUMEQUALVERIFY'),
    pushNumber(0), ops('OP_OUTPOINTINDEX'), pushNumber(1), ops('OP_NUMEQUALVERIFY'),
    ops('OP_DUP', 'OP_HASH256'), pushNumber(0), ops('OP_OUTPOINTTXHASH', 'OP_EQUALVERIFY'),
    pushNumber(5), ops('OP_SPLIT', 'OP_SWAP'), pushData(hexToBin('0200000001')), ops('OP_EQUALVERIFY'),
    pushNumber(32), ops('OP_SPLIT', 'OP_SWAP', 'OP_FROMALTSTACK', 'OP_EQUALVERIFY'),
    pushNumber(4), ops('OP_SPLIT', 'OP_DROP'), pushData(Uint8Array.of(0, 0, 0, 0)), ops('OP_EQUALVERIFY'),
    pushNumber(0), ops('OP_UTXOTOKENAMOUNT', 'OP_DUP'), pushNumber(siteBps), ops('OP_MUL'),
    pushNumber(10000), ops('OP_DIV', 'OP_DUP'), pushNumber(1), ops('OP_OUTPUTTOKENAMOUNT', 'OP_NUMEQUALVERIFY', 'OP_SUB'),
    pushNumber(0), ops('OP_OUTPUTTOKENAMOUNT', 'OP_NUMEQUALVERIFY'),
    pushNumber(1), ops('OP_OUTPUTBYTECODE'), pushData(ownerLock), ops('OP_EQUALVERIFY'),
    pushNumber(0), ops('OP_OUTPUTVALUE'), pushNumber(TOKEN_OUTPUT_SATS), ops('OP_NUMEQUALVERIFY'),
    pushNumber(1), ops('OP_OUTPUTVALUE'), pushNumber(TOKEN_OUTPUT_SATS), ops('OP_NUMEQUALVERIFY'),
    pushNumber(0), ops('OP_UTXOVALUE'), pushNumber(rewardSats), ops('OP_NUMEQUALVERIFY'),
    pushData(Uint8Array.of(0x20)), pushNumber(0), ops('OP_OUTPOINTTXHASH', 'OP_CAT', 'OP_ACTIVEBYTECODE'),
    pushNumber(33), ops('OP_SPLIT', 'OP_NIP', 'OP_CAT'), pushNumber(2), ops('OP_OUTPUTBYTECODE', 'OP_EQUALVERIFY'),
    pushNumber(1), ops('OP_UTXOVALUE'), pushNumber(SPONSOR_SUBSIDY_SATS), ops('OP_SUB'),
    pushNumber(2), ops('OP_OUTPUTVALUE', 'OP_NUMEQUALVERIFY'),
    pushNumber(1), ops('OP_ENDIF'),
  );
};

const imported = importWalletTemplate(walletTemplateP2pkhNonHd);
if (typeof imported === 'string') throw new Error(imported);
const p2pkhCompiler = walletTemplateToCompilerBch(imported);

const referenceHex = readFileSync(
  new URL('../../reference/photon_vector_tx.hex', import.meta.url),
  'utf8',
).trim();
const runtimeConfigSource = readFileSync(
  new URL('../../src/config.rs', import.meta.url),
  'utf8',
);
const configuredBps = runtimeConfigSource.match(/pub const DONATION_BPS: u16 = (\d+);/u)?.[1];
const configuredAddress = runtimeConfigSource.match(
  /pub const DONATION_ADDRESS: &str = "([^"]+)";/u,
)?.[1];
if (configuredBps !== DONATION_BPS.toString()) {
  throw new Error(`Rust donation BPS drifted: ${configuredBps ?? 'missing'}`);
}
if (configuredAddress !== DONATION_ADDRESS) {
  throw new Error(`Rust donation address drifted: ${configuredAddress ?? 'missing'}`);
}
const referenceParent = decodeTransactionBch(hexToBin(referenceHex));
if (referenceParent.outputs.length !== 2) {
  throw new Error(`authoritative PHOTON vector has ${referenceParent.outputs.length} outputs`);
}
const donationLock = decodeP2pkh(DONATION_ADDRESS);

const buildCase = ({
  parentBatonHash = referenceParent.inputs[0].outpointTransactionHash,
  siteAmountDelta = 0n,
  redirectDonation = false,
  drainSponsor = 0n,
  breakContinuation = false,
  extraOutput = false,
  rewardOutpointIndex = 1,
} = {}) => {
  const parent = structuredClone(referenceParent);
  parent.inputs[0].outpointTransactionHash = parentBatonHash;
  parent.outputs[1].lockingBytecode = REWARD_LOCK;
  const parentBytes = encodeTransactionBch(parent);
  const parentHash = hash256(parentBytes);
  const reward = parent.outputs[1];
  if (reward.token === undefined) throw new Error('reference reward output has no token');
  const rewardRaw = reward.token.amount;
  const donationRaw = (rewardRaw * DONATION_BPS) / 10000n;
  const minerRaw = rewardRaw - donationRaw;
  const sponsorScript = buildSponsorScript({
    // The sponsor state commits the baton outpoint exactly as it appears in the
    // serialized parent transaction, while Libauth stores transaction hashes in
    // its transaction object in display order.
    expectedBatonHash: reverse(referenceParent.inputs[0].outpointTransactionHash),
    ownerLock: donationLock,
    siteBps: DONATION_BPS,
    rewardSats: reward.valueSatoshis,
  });
  const continuation = concat(Uint8Array.of(0x20), parentHash, sponsorScript.slice(33));
  if (breakContinuation) continuation[continuation.length - 1] ^= 1;
  const sponsorSource = {
    lockingBytecode: sponsorScript,
    valueSatoshis: SPONSOR_RESERVE_SATS,
  };
  const outputs = [
    {
      lockingBytecode: REWARD_LOCK,
      valueSatoshis: TOKEN_OUTPUT_SATS,
      token: { ...reward.token, amount: minerRaw - siteAmountDelta },
    },
    {
      lockingBytecode: redirectDonation ? REWARD_LOCK : donationLock,
      valueSatoshis: TOKEN_OUTPUT_SATS,
      token: { ...reward.token, amount: donationRaw + siteAmountDelta },
    },
    {
      lockingBytecode: continuation,
      valueSatoshis: SPONSOR_RESERVE_SATS - SPONSOR_SUBSIDY_SATS - drainSponsor,
    },
  ];
  if (extraOutput) outputs.push({ lockingBytecode: Uint8Array.of(0x51), valueSatoshis: 0n });
  const config = {
    version: 2,
    locktime: 0,
    inputs: [
      {
        outpointIndex: rewardOutpointIndex,
        // Libauth transaction fields use display-order transaction hashes while
        // BCH introspection exposes the serialized/HASH256 byte order. Store the
        // reversed digest here so OP_OUTPOINTTXHASH matches OP_HASH256(raw parent).
        outpointTransactionHash: reverse(parentHash),
        sequenceNumber: 0,
        unlockingBytecode: {
          compiler: p2pkhCompiler,
          data: { keys: { privateKeys: { key: PRIVATE_KEY } } },
          valueSatoshis: reward.valueSatoshis,
          script: 'unlock',
          token: reward.token,
        },
      },
      {
        outpointIndex: 0,
        outpointTransactionHash: hexToBin('55'.repeat(32)),
        sequenceNumber: 0,
        unlockingBytecode: concat(pushData(parentBytes), Uint8Array.of(op.OP_0)),
      },
    ],
    outputs,
  };
  const generated = generateTransaction(config);
  if (!generated.success) throw new Error(JSON.stringify(generated.errors));
  return {
    parentBytes,
    reward,
    sponsorScript,
    sourceOutputs: [reward, sponsorSource],
    transaction: generated.transaction,
  };
};

const verify = (fixture, standard) => {
  const tokenResult = verifyTransactionTokens(fixture.transaction, fixture.sourceOutputs, {
    maximumTokenCommitmentLength: 128,
  });
  const vm = createVirtualMachineBch2026(standard);
  const vmResult = vm.verify({
    sourceOutputs: fixture.sourceOutputs,
    transaction: fixture.transaction,
  });
  let trace;
  if (vmResult !== true) {
    trace = stringifyDebugTraceSummary(
      summarizeDebugTrace(
        vm.debug({
          inputIndex: 1,
          sourceOutputs: fixture.sourceOutputs,
          transaction: fixture.transaction,
        }),
      ),
    );
  }
  return { tokenResult, vmResult, trace };
};

const valid = buildCase();
if (valid.parentBytes.length !== 615) throw new Error(`parent length ${valid.parentBytes.length}`);
if (valid.sponsorScript.length > 201) throw new Error(`sponsor script too large: ${valid.sponsorScript.length}`);
for (const standard of [false, true]) {
  const result = verify(valid, standard);
  if (result.tokenResult !== true || result.vmResult !== true) {
    if (result.trace !== undefined) console.error(result.trace);
    throw new Error(`valid fixture rejected (standard=${standard}): ${String(result.tokenResult)} / ${String(result.vmResult)}`);
  }
}

const attacks = [
  ['underpay donation by one raw unit', { siteAmountDelta: -1n }],
  ['redirect donation output', { redirectDonation: true }],
  ['drain one extra sponsor satoshi', { drainSponsor: 1n }],
  ['break sponsor continuation', { breakContinuation: true }],
  ['add a fourth output', { extraOutput: true }],
  ['spend parent output zero', { rewardOutpointIndex: 0 }],
  ['stale sponsor baton state', { parentBatonHash: hexToBin('22'.repeat(32)) }],
];

for (const [name, patch] of attacks) {
  const fixture = buildCase(patch);
  const result = verify(fixture, false);
  if (result.vmResult === true) throw new Error(`attack accepted by BCH 2026 VM: ${name}`);
}

const fee = valid.sourceOutputs.reduce((sum, output) => sum + output.valueSatoshis, 0n) -
  valid.transaction.outputs.reduce((sum, output) => sum + output.valueSatoshis, 0n);
console.log(`PASS BCH 2026 VM reward-policy proof`);
console.log(`parent_bytes=${valid.parentBytes.length}`);
console.log(`sponsor_script_bytes=${valid.sponsorScript.length}`);
console.log(`child_bytes=${encodeTransactionBch(valid.transaction).length}`);
console.log(`child_fee_sats=${fee}`);
console.log(`reward_raw=${valid.reward.token.amount}`);
console.log(`donation_raw=${(valid.reward.token.amount * DONATION_BPS) / 10000n}`);
console.log(`miner_raw=${valid.reward.token.amount - (valid.reward.token.amount * DONATION_BPS) / 10000n}`);
console.log(`child_txid_internal=${binToHex(hash256(encodeTransactionBch(valid.transaction)))}`);
console.log(`adversarial_cases=${attacks.length}`);
