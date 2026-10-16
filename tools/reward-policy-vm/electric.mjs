// #### PR #51: run the ELECTRIC claims and pot refunds that Rust builds
// through the BCH 2026 VM, against the published contract body and the pot.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import {
  bigIntToVmNumber, binToHex, createVirtualMachineBch2026, decodeTransactionBch,
  encodeDataPush, flattenBinArray, hash256, hexToBin, swapEndianness,
} from '@bitauth/libauth';

// The covenant holding the mainnet baton, read from Fulcrum on 2026-10-15
// (scripthash 77f49785…c4b6), independently of the Rust constant.
const LIVE_MAINNET_LOCK =
  'aa20f3b3cc8e800544dacb055235a1d45d7b7d0804a894935d301ef4796a2b08e70587';

const root = fileURLToPath(new URL('../..', import.meta.url));
const temp = mkdtempSync(join(tmpdir(), 'pickaxe-electric-proof-'));
try {
  const path = process.argv[2] ?? join(temp, 'electric.json');
  if (!process.argv[2]) {
    const result = spawnSync('cargo', ['test', '--locked', '--no-default-features', '--lib',
      'electric::tests::vm_proof_vectors', '--', '--exact'], {
      cwd: root, stdio: 'inherit', env: { ...process.env, PICKAXE_ELECTRIC_PROOF: path },
    });
    assert.equal(result.status, 0, result.error?.message ?? 'Rust ELECTRIC vectors failed');
  }
  const proof = JSON.parse(readFileSync(path, 'utf8'));

  // Rebuild the covenant from the contract body and constructor values as
  // cashc does (arguments reversed, minimal pushes), here rather than in Rust.
  const source = readFileSync(join(root, 'src/electric.rs'), 'utf8');
  const bodyBlock = source.match(/POW_MINE_STATEFUL_V1_BODY_HEX: &str = concat!\(([\s\S]*?)\);/)[1];
  const body = hexToBin([...bodyBlock.matchAll(/"([0-9a-f]+)"/g)].map(m => m[1]).join(''));
  const redeem = (category, reward, min, max) => flattenBinArray([
    encodeDataPush(bigIntToVmNumber(BigInt(max))),
    encodeDataPush(bigIntToVmNumber(BigInt(min))),
    encodeDataPush(bigIntToVmNumber(BigInt(reward))),
    encodeDataPush(hexToBin(swapEndianness(category))),
    body,
  ]);
  const p2sh32 = script => flattenBinArray([Uint8Array.of(0xaa, 0x20), hash256(script), Uint8Array.of(0x87)]);
  const mainnet = p2sh32(redeem(
    '821f86ed21c38043500d7364fa73108cce8990baf95c4a0925fc44abfc27f8c7', 60000000, 3, 4));
  assert.equal(binToHex(mainnet), LIVE_MAINNET_LOCK, 'contract body drifted from the live covenant');
  assert.equal(proof.mainnet_lock, LIVE_MAINNET_LOCK);
  const testRedeem = redeem(proof.category, proof.reward, proof.min_difficulty, proof.max_difficulty);
  assert.equal(binToHex(testRedeem), proof.electric_redeem);
  assert.equal(binToHex(p2sh32(testRedeem)), proof.electric_lock);

  let valid = 0;
  let refused = 0;
  for (const sample of proof.samples) {
    const tx = decodeTransactionBch(hexToBin(sample.raw));
    assert.notEqual(typeof tx, 'string', sample.name);
    const pot = { lockingBytecode: hexToBin(sample.pot_lock), valueSatoshis: BigInt(sample.pot_value) };
    const refund = tx.inputs.length === 1;
    const sourceOutputs = refund ? [pot] : [{
      lockingBytecode: hexToBin(proof.electric_lock),
      valueSatoshis: BigInt(sample.baton.value),
      token: {
        category: hexToBin(proof.category),
        amount: BigInt(sample.baton.amount),
        nft: { capability: 'mutable', commitment: hexToBin(sample.baton.commitment) },
      },
    }, pot];
    const inputs = sourceOutputs.reduce((sum, output) => sum + output.valueSatoshis, 0n);
    const outputs = tx.outputs.reduce((sum, output) => sum + output.valueSatoshis, 0n);
    if (sample.expect === 'valid') {
      assert.equal(inputs - outputs, BigInt(sample.fee), `${sample.name}: fee`);
      assert.ok(BigInt(sample.fee) >= BigInt(sample.raw.length / 2), `${sample.name}: relay fee`);
    }
    for (const standard of [false, true]) {
      const vm = createVirtualMachineBch2026(standard);
      const result = vm.verify({ sourceOutputs, transaction: tx });
      if (sample.expect === 'valid') {
        assert.equal(result, true, `${sample.name} (standard=${standard}): ${result}`);
        valid += 1;
        // The reward and the baton's state are bound by the contract.
        const altered = structuredClone(tx);
        if (refund) {
          altered.outputs[0].valueSatoshis = pot.valueSatoshis - BigInt(proof.pot_max_fee) - 1n;
        } else {
          altered.outputs[0].token.amount += 1n;
        }
        assert.notEqual(vm.verify({ sourceOutputs, transaction: altered }), true, sample.name);
        if (refund) {
          const duplicate = structuredClone(tx);
          duplicate.inputs.push({ ...structuredClone(tx.inputs[0]), outpointIndex: 999 });
          assert.notEqual(vm.verify({ sourceOutputs: [pot, pot], transaction: duplicate }), true,
            `${sample.name}: cannot share one refund output between two pot inputs`);
          if (sample.owner) {
            for (const change of [
              t => { t.inputs[0].outpointIndex += 1; },
              t => { t.outputs[0].valueSatoshis -= 1n; },
              t => { t.outputs[0].lockingBytecode[3] ^= 1; },
              t => { t.inputs[0].unlockingBytecode[1] ^= 1; },
              t => { t.inputs[0].unlockingBytecode[65] = 0x42; },
            ]) {
              const tampered = structuredClone(tx);
              change(tampered);
              assert.notEqual(vm.verify({ sourceOutputs, transaction: tampered }), true, sample.name);
            }
            assert.notEqual(vm.verify({ sourceOutputs: [{ ...pot, valueSatoshis: pot.valueSatoshis + 1n }], transaction: tx }), true,
              `${sample.name}: signature commits to the spent value`);
          }
        }
      } else {
        // The pot (input 1 of a claim, input 0 of a refund) is what refuses,
        // so for a claim the ELECTRIC covenant at input 0 accepted it.
        assert.equal(typeof result, 'string', `${sample.name} must fail`);
        assert.match(result, new RegExp(`input index ${refund ? 0 : 1}\\b`), `${sample.name}: ${result}`);
        refused += 1;
      }
    }
  }
  assert.equal(valid, 2 * 10);
  assert.equal(refused, 2 * 10);
  console.log(`PASS ELECTRIC: the contract body matches the live mainnet covenant; ${proof.samples.length} claims and refunds in standard and consensus VMs: the pot pays claims to the payout and both donation addresses and the cheap same-height claim, refunds only to the payout, and refuses replay-shaped claims, other recipients, fees above its cap, and unsigned or wrong-key recovery of owner pots.`);
} finally {
  rmSync(temp, { recursive: true });
}
