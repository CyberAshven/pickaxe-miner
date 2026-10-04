import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildModule } from './test-build.mjs';
const { MiningError, searchBatch, unsupportedReason } = await buildModule('platform.js');

const supported = () => ({ isSecureContext: true, navigator: { gpu: {}, locks: { request() {} } }, WebAssembly: {} });

test('capabilities, not browser identity, determine whether mining can start', () => {
  for (const userAgent of ['Firefox', 'Safari', 'Chrome', 'An unknown browser']) {
    const environment = supported();
    environment.navigator.userAgent = userAgent;
    assert.equal(unsupportedReason(environment), null);
  }
  for (const [change, message] of [
    [e => { e.isSecureContext = false; }, /HTTPS or localhost/],
    [e => { delete e.navigator.gpu; }, /WebGPU is unavailable/],
    [e => { delete e.navigator.locks; }, /Web Locks is unavailable/],
    [e => { delete e.WebAssembly; }, /WebAssembly/],
  ]) {
    const environment = supported(); change(environment);
    assert.match(unsupportedReason(environment), message);
  }
});

test('failed GPU searches and malformed engine responses are fatal mining errors', async () => {
  for (const reason of [new Error('device lost'), 'signature verification failed']) {
    let calls = 0;
    await assert.rejects(searchBatch({ async search() { calls++; throw reason; } }), error => {
      assert.ok(error instanceof MiningError);
      assert.equal(error.cause, reason);
      return true;
    });
    assert.equal(calls, 1);
  }
  await assert.rejects(searchBatch({ async search() { return 'invalid JSON'; } }), MiningError);
  const result = { candidates: 1024, context: 'job', baton: 'outpoint' };
  assert.deepEqual(await searchBatch({ async search() { return JSON.stringify(result); } }), result);
});

test('typed mining boundary rejects malformed successful responses', async () => {
  for (const result of [null, {}, { candidates: '100', context: 'job', baton: 'point' },
    { candidates: -1, context: 'job', baton: 'point' }, { candidates: 1.5, context: 'job', baton: 'point' },
    { candidates: 100, context: 'job', baton: 'point', transaction: '00' }]) {
    await assert.rejects(searchBatch({ async search() { return JSON.stringify(result); } }), MiningError);
  }
});
