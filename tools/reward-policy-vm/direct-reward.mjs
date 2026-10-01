// Execute the Rust payout/journal lifecycle, then independently verify the
// resulting transactions against both deployed covenants and relay policy.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { cashAddressToLockingBytecode, createVirtualMachineBch2026,
  decodeTransactionBch, hexToBin } from '@bitauth/libauth';

const root = fileURLToPath(new URL('../..', import.meta.url));
const temp = mkdtempSync(join(tmpdir(), 'pickaxe-direct-proof-'));
try {
  const path = process.argv[2] ?? join(temp, 'transactions.json');
  if (!process.argv[2]) {
  const result = spawnSync('cargo', ['test', '--locked', '--no-default-features', '--features', 'tail-grind',
    'runtime::tests::direct_reward_lifecycle_and_recovery', '--', '--exact'], {
    cwd: root, stdio: 'inherit', env: { ...process.env, PICKAXE_DIRECT_REWARD_PROOF: path },
  });
  assert.equal(result.status, 0, result.error?.message ?? 'Rust lifecycle failed');
  }
  const protocol = readFileSync(join(root, 'src/protocol.rs'), 'utf8');
  const constant = name => hexToBin(protocol.match(new RegExp(`pub const ${name}: &str =\\s*"([a-f0-9]+)"`))[1]);
  const samples = JSON.parse(readFileSync(path, 'utf8'));
  assert.equal(samples.length, process.argv[2] ? 5 : 25);
  for (const sample of samples) {
    const chipnet = sample.network === 'chipnet';
    const tx = decodeTransactionBch(hexToBin(sample.raw));
    assert.notEqual(typeof tx, 'string');
    const source = {
      lockingBytecode: constant(chipnet ? 'CHIPNET_COVENANT_LOCKING_BYTECODE_HEX' : 'COVENANT_LOCKING_BYTECODE_HEX'),
      valueSatoshis: BigInt(sample.value),
      token: { category: constant(chipnet ? 'CHIPNET_CATEGORY_HEX' : 'MAINNET_CATEGORY_HEX'),
        amount: BigInt(sample.amount), nft: { capability: 'mutable',
          commitment: Uint8Array.from([...new Uint8Array(4), ...hexToBin(sample.old_target_le), ...new Uint8Array(64)]) } },
    };
    const payout = cashAddressToLockingBytecode(sample.payout);
    assert.notEqual(typeof payout, 'string');
    assert.equal(tx.inputs.length, 1, 'must not spend a user funding input');
    assert.equal(tx.outputs.length, 2, 'no split or intermediate reward outputs');
    assert.deepEqual(tx.outputs[1].lockingBytecode, payout.bytecode);
    assert.equal(tx.outputs[1].token.amount, BigInt(sample.reward));
    assert.equal(tx.outputs[1].valueSatoshis, 700n);
    assert.equal(source.valueSatoshis - tx.outputs.reduce((sum, o) => sum + o.valueSatoshis, 0n), 800n);
    for (const standard of [false, true]) {
      const vm = createVirtualMachineBch2026(standard);
      assert.equal(vm.verify({ sourceOutputs: [source], transaction: tx }), true,
        `${sample.network} ${sample.recipient} age=${sample.age}`);
      const altered = structuredClone(tx);
      altered.outputs[1].token.amount += 1n;
      assert.notEqual(vm.verify({ sourceOutputs: [source], transaction: altered }), true);
    }
  }
  console.log(`PASS direct payout lifecycle: ${samples.length} mainnet/Chipnet claims, all active recipients, standard and consensus VM; altered rewards rejected.`);
} finally {
  rmSync(temp, { recursive: true });
}
