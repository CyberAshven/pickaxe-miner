// #### PR #65: execute the existing ASIC header-token payout forwarders in
// an independent VM. No token is registered or designed by this fixture.
// AuxProof Case A/B and donation bindings are tested by the Rust merge suite;
// this checks the executable payout bytecode, including composed inputs.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createVirtualMachineBch2026, hexToBin } from '@bitauth/libauth';

const root = fileURLToPath(new URL('../..', import.meta.url));
const temp = mkdtempSync(join(tmpdir(), 'pickaxe-merge-vm-'));
try {
  const path = process.argv[2] ?? join(temp, 'forwarders.json');
  if (!process.argv[2]) {
    const run = spawnSync('cargo', ['test', '--locked', '--no-default-features',
      '--features', 'stratum-v2', '--lib',
      'stratum_v2::merge::header::tests::vm_forwarder_vectors', '--', '--exact'], {
      cwd: root, stdio: 'inherit', env: { ...process.env, PICKAXE_MERGE_VM_PROOF: path },
    });
    assert.equal(run.status, 0, run.error?.message ?? 'Rust forwarder export failed');
  }
  const samples = JSON.parse(readFileSync(path, 'utf8'));
  assert.equal(samples.length, 4);
  const input = (index) => ({
    outpointTransactionHash: new Uint8Array(32).fill(index + 1),
    outpointIndex: 0, sequenceNumber: 0xffffffff, unlockingBytecode: new Uint8Array(),
  });
  const source = (sample) => ({
    lockingBytecode: hexToBin(sample.script), valueSatoshis: 10000n,
    token: { category: hexToBin(sample.category), amount: BigInt(sample.amount) },
  });
  const output = (sample) => ({
    ...source(sample), lockingBytecode: hexToBin(sample.destination), valueSatoshis: 9000n,
  });
  let accepted = 0;
  let rejected = 0;
  for (const standard of [false, true]) {
    const vm = createVirtualMachineBch2026(standard);
    const check = (sources, transaction) => vm.verify({ sourceOutputs: sources, transaction });
    for (const sample of samples) {
      const sources = [source(sample)];
      const tx = { version: 2, locktime: 0, inputs: [input(0)], outputs: [output(sample)] };
      assert.equal(check(sources, tx), true, `${sample.role}/${sample.channel} intended payout`);
      accepted += 1;
      const wrongDestination = structuredClone(tx);
      wrongDestination.outputs[0].lockingBytecode[3] ^= 1;
      assert.notEqual(check(sources, wrongDestination), true, 'recipient redirection');
      const partialPayment = structuredClone(tx);
      partialPayment.outputs[0].token.amount -= 1n;
      assert.notEqual(check(sources, partialPayment), true, 'partial token payment');
      const wrongCategory = structuredClone(tx);
      wrongCategory.outputs[0].token.category[0] ^= 1;
      assert.notEqual(check(sources, wrongCategory), true, 'category substitution');
      rejected += 3;
    }
    // Personal and donation wins can be swept together; each input still binds
    // its own output. Unequal amounts expose output-sharing/double-counting.
    for (const channel of ['standard', 'extended']) {
      const pair = samples.filter((sample) => sample.channel === channel);
      const sources = pair.map(source);
      const tx = { version: 2, locktime: 0, inputs: pair.map((_, i) => input(i)), outputs: pair.map(output) };
      assert.equal(check(sources, tx), true, 'independent personal and donation outputs');
      accepted += 1;
      const swapped = structuredClone(tx);
      swapped.outputs.reverse();
      assert.notEqual(check(sources, swapped), true, 'swapped recipient outputs');
      const shared = structuredClone(tx);
      shared.outputs[0].token.amount += shared.outputs[1].token.amount;
      shared.outputs.pop();
      assert.notEqual(check(sources, shared), true, 'one output cannot satisfy both inputs');
      rejected += 2;
    }
  }
  console.log(`Merged-token forwarder VM: ${accepted} valid and ${rejected} refused; standard + consensus BCH 2026; no registration or broadcast.`);
} finally {
  rmSync(temp, { recursive: true, force: true });
}
