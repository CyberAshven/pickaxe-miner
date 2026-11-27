import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { buildModule } from './test-build.mjs';

const { formatMinerStats } = await buildModule('miner.js');

test('the session status line stays the page line', () => {
  assert.equal(
    formatMinerStats(1_450_000, 1.25, 3, 12_345),
    'Hashrate 1.45 MH/s · average 1.25 MH/s\n3 accepted · 12s elapsed',
  );
  assert.equal(
    formatMinerStats(0, 0, 0, 999),
    'Hashrate 0.00 MH/s · average 0.00 MH/s\n0 accepted · 0s elapsed',
  );
});

test('the callable session does not look up the Pickaxe page', () => {
  const source = readFileSync(new URL('./miner.ts', import.meta.url), 'utf8');
  assert.equal(source.includes('getElementById'), false);
  assert.equal(source.includes('index.html'), false);
  assert.match(source, /export async function runMiner/);
});
