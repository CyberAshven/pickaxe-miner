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
  verifyTransactionTokens,
  walletTemplateP2pkhNonHd,
  walletTemplateToCompilerBch,
} from '@bitauth/libauth';

const DONATION_ADDRESS =
  'bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3';
const DONATION_BPS = 200n;
const MULTI_INPUT_MAX_BATON_DECREASE_SATS = 8000n;
const RELAY_FEE_SATS_PER_KB = 1000n;
const PRIVATE_KEY = hexToBin('00'.repeat(31) + '01');

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

const bytesEqual = (left, right) =>
  left.length === right.length && left.every((byte, index) => byte === right[index]);

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

const decodeP2pkh = (address) => {
  const decoded = cashAddressToLockingBytecode(address);
  if (typeof decoded === 'string') throw new Error(decoded);
  if (decoded.bytecode.length !== 25) throw new Error('expected P2PKH locking bytecode');
  return decoded.bytecode;
};

const requiredRelayFee = (bytes) =>
  (BigInt(bytes) * RELAY_FEE_SATS_PER_KB + 999n) / 1000n;

const imported = importWalletTemplate(walletTemplateP2pkhNonHd);
if (typeof imported === 'string') throw new Error(imported);
const p2pkhCompiler = walletTemplateToCompilerBch(imported);

const referenceHex = readFileSync(
  new URL('../../reference/photon_vector_tx.hex', import.meta.url),
  'utf8',
).trim();
const redeemScript = hexToBin(
  readFileSync(new URL('../../reference/photon_redeem.hex', import.meta.url), 'utf8').trim(),
);
const parent = decodeTransactionBch(hexToBin(referenceHex));
if (parent.outputs.length !== 2) {
  throw new Error(`authoritative PHOTON vector has ${parent.outputs.length} outputs`);
}
const baton = parent.outputs[0];
const reward = parent.outputs[1];
if (baton.token === undefined || reward.token === undefined) {
  throw new Error('authoritative PHOTON vector outputs must carry CashTokens');
}

const rewardLock = hexToBin('76a914751e76e8199196d454941c45d1b3a323f1433bd688ac');
parent.outputs[1].lockingBytecode = rewardLock;
const parentBytes = encodeTransactionBch(parent);
const parentHash = hash256(parentBytes);
const parentOutpointHash = reverse(parentHash);
const donationLock = decodeP2pkh(DONATION_ADDRESS);
const rewardRaw = reward.token.amount;
const donationRaw = (rewardRaw * DONATION_BPS) / 10000n;
const minerRaw = rewardRaw - donationRaw;
if (minerRaw + donationRaw !== rewardRaw) throw new Error('98/2 token conservation failed');

const buildSettlement = (batonDecreaseSats, patch = {}) => {
  const batonValue = baton.valueSatoshis - batonDecreaseSats;
  if (batonValue < 0n) throw new Error('negative baton value');
  const alteredBatonLock = Uint8Array.from(baton.lockingBytecode);
  if (patch.breakBatonLock) alteredBatonLock[alteredBatonLock.length - 1] ^= 1;
  const outputs = [
    {
      lockingBytecode: alteredBatonLock,
      valueSatoshis: batonValue,
      token: patch.breakBatonToken
        ? { ...baton.token, amount: baton.token.amount - 1n }
        : baton.token,
    },
    {
      lockingBytecode: rewardLock,
      valueSatoshis: reward.valueSatoshis,
      token: { ...reward.token, amount: minerRaw },
    },
    {
      lockingBytecode: patch.redirectDonation ? rewardLock : donationLock,
      valueSatoshis: reward.valueSatoshis,
      token: { ...reward.token, amount: donationRaw + (patch.mintExtra ?? 0n) },
    },
  ];
  if (patch.extraOutput) {
    outputs.push({ lockingBytecode: Uint8Array.of(0x51), valueSatoshis: 0n });
  }
  const generated = generateTransaction({
    version: 2,
    locktime: 0,
    inputs: [
      {
        outpointIndex: 0,
        outpointTransactionHash: parentOutpointHash,
        sequenceNumber: 0,
        unlockingBytecode: pushData(redeemScript),
      },
      {
        outpointIndex: 1,
        outpointTransactionHash: parentOutpointHash,
        sequenceNumber: 0,
        unlockingBytecode: {
          compiler: p2pkhCompiler,
          data: { keys: { privateKeys: { key: PRIVATE_KEY } } },
          valueSatoshis: reward.valueSatoshis,
          script: 'unlock',
          token: reward.token,
        },
      },
    ],
    outputs,
  });
  if (!generated.success) throw new Error(JSON.stringify(generated.errors));
  return generated.transaction;
};

const provisional = buildSettlement(MULTI_INPUT_MAX_BATON_DECREASE_SATS);
const settlementBytes = encodeTransactionBch(provisional).length;
const relayFee = requiredRelayFee(settlementBytes);
const batonDecrease = reward.valueSatoshis + relayFee;
if (batonDecrease > MULTI_INPUT_MAX_BATON_DECREASE_SATS) {
  throw new Error(
    `self-funded settlement requires ${batonDecrease} baton sats, covenant allows 8000`,
  );
}
const settlement = buildSettlement(batonDecrease);
const finalBytes = encodeTransactionBch(settlement);
if (finalBytes.length !== settlementBytes) throw new Error('fee selection changed serialized size');

const sourceOutputs = [baton, reward];
const tokenResult = verifyTransactionTokens(settlement, sourceOutputs, {
  maximumTokenCommitmentLength: 128,
});
if (tokenResult !== true) {
  throw new Error(`CashToken conservation failed: ${String(tokenResult)}`);
}

for (const standard of [false, true]) {
  const vm = createVirtualMachineBch2026(standard);
  const result = vm.verify({ sourceOutputs, transaction: settlement });
  if (result !== true) {
    throw new Error(
      `self-funded settlement rejected (standard=${standard}): ${String(result)}`,
    );
  }
}

const inputValue = sourceOutputs.reduce((sum, output) => sum + output.valueSatoshis, 0n);
const outputValue = settlement.outputs.reduce((sum, output) => sum + output.valueSatoshis, 0n);
const actualFee = inputValue - outputValue;
if (actualFee !== relayFee) {
  throw new Error(`fee ${actualFee} != required relay fee ${relayFee}`);
}
if (baton.valueSatoshis - settlement.outputs[0].valueSatoshis !== batonDecrease) {
  throw new Error('baton BCH decrease mismatch');
}
if (!bytesEqual(settlement.outputs[0].lockingBytecode, baton.lockingBytecode)) {
  throw new Error('baton locking bytecode changed');
}
if (settlement.outputs[0].token?.amount !== baton.token.amount) {
  throw new Error('baton token amount changed');
}
if (!bytesEqual(settlement.outputs[0].token?.category ?? new Uint8Array(), baton.token.category)) {
  throw new Error('baton token category changed');
}
if (
  !bytesEqual(
    settlement.outputs[0].token?.nft?.commitment ?? new Uint8Array(),
    baton.token.nft?.commitment ?? new Uint8Array(),
  )
) {
  throw new Error('baton NFT commitment changed');
}
if (settlement.outputs[1].token?.amount !== minerRaw) {
  throw new Error('miner token amount is not exact 98% remainder');
}
if (settlement.outputs[2].token?.amount !== donationRaw) {
  throw new Error('donation token amount is not exact 2% floor');
}
if (minerRaw + donationRaw !== rewardRaw) {
  throw new Error('reward token split does not conserve the full reward');
}

const attacks = [
  ['change baton locking bytecode', () => buildSettlement(batonDecrease, { breakBatonLock: true })],
  ['change baton token amount', () => buildSettlement(batonDecrease, { breakBatonToken: true })],
  [
    'decrease baton by more than 8000 satoshis',
    () => buildSettlement(MULTI_INPUT_MAX_BATON_DECREASE_SATS + 1n),
  ],
];
for (const [name, buildAttack] of attacks) {
  const transaction = buildAttack();
  const vm = createVirtualMachineBch2026(false);
  const result = vm.verify({ sourceOutputs, transaction });
  if (result === true) throw new Error(`authoritative covenant mutation accepted: ${name}`);
}

const tokenMint = buildSettlement(batonDecrease, { mintExtra: 1n });
if (
  verifyTransactionTokens(tokenMint, sourceOutputs, {
    maximumTokenCommitmentLength: 128,
  }) === true
) {
  throw new Error('CashToken mint mutation unexpectedly conserved');
}

console.log('PASS BCH 2026 VM self-funded PHOTON settlement proof');
console.log(`parent_bytes=${parentBytes.length}`);
console.log(`settlement_bytes=${finalBytes.length}`);
console.log(`required_relay_fee_sats=${relayFee}`);
console.log(`actual_fee_sats=${actualFee}`);
console.log(`baton_input_sats=${baton.valueSatoshis}`);
console.log(`baton_output_sats=${settlement.outputs[0].valueSatoshis}`);
console.log(`baton_decrease_sats=${batonDecrease}`);
console.log(`reward_input_sats=${reward.valueSatoshis}`);
console.log(`miner_output_sats=${settlement.outputs[1].valueSatoshis}`);
console.log(`donation_output_sats=${settlement.outputs[2].valueSatoshis}`);
console.log(`reward_raw=${rewardRaw}`);
console.log(`miner_raw=${minerRaw}`);
console.log(`donation_raw=${donationRaw}`);
console.log(`settlement_txid_internal=${binToHex(hash256(finalBytes))}`);
console.log(`adversarial_cases=${attacks.length + 1}`);
