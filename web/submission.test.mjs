import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildModule } from './test-build.mjs';
const { resolveSubmission, refusedAsNotFinal } = await buildModule('submission.js');

const pending = { txid: 'ab'.repeat(32), transaction: '01020304', baton: `${'cd'.repeat(32)}:0`, context: 'original' };
const defaults = { pending, stopped: () => false, wait: async () => {}, waitForBlock: async () => { throw new Error('no block wait expected'); }, progress() {} };
const notFound = () => Object.assign(new Error('not found'), { code: 2 });
// Fulcrum's answer while the claim's baton is not confirmed yet.
const notFinal = () => Object.assign(new Error('the transaction was rejected by network rules.\n\nnon-BIP68-final (code 64)\n'), { code: 1 });

test('an already accepted transaction waits for baton advance despite context changes', async () => {
  let snapshots = 0;
  const calls = [];
  const result = await resolveSubmission({ ...defaults,
    session: { rpc: async method => { calls.push(method); return pending.transaction; } },
    snapshot: async () => ({ context: `changed fee or height ${snapshots}`, baton: ++snapshots < 4 ? pending.baton : 'successor' }),
  });
  assert.equal(result, 'accepted');
  assert.equal(snapshots, 4);
  assert.deepEqual(calls, ['blockchain.transaction.get']);
});

test('broadcast success with delayed indexing leaves the journal pending on stop', async () => {
  let stopped = false;
  const calls = [];
  const result = await resolveSubmission({ ...defaults,
    stopped: () => stopped,
    wait: async () => { stopped = true; },
    session: { rpc: async method => {
      calls.push(method);
      if (method.endsWith('.get')) throw Object.assign(new Error('not found'), { code: 1 });
      return pending.txid;
    } },
    snapshot: async () => ({ baton: pending.baton }),
  });
  assert.equal(result, 'pending');
  assert.deepEqual(calls, ['blockchain.transaction.get', 'blockchain.transaction.broadcast']);
});

test('a lost broadcast response does not discard the pending transaction', async () => {
  await assert.rejects(resolveSubmission({ ...defaults,
    session: { rpc: async method => {
      if (method.endsWith('.get')) throw Object.assign(new Error('not found'), { code: 1 });
      throw new Error('connection lost after send');
    } },
    snapshot: async () => ({ baton: pending.baton }),
  }), /connection lost after send/);
});

test('unindexed accepted rewards time out without treating a fee change as a new baton', async () => {
  await assert.rejects(resolveSubmission({ ...defaults,
    session: { rpc: async () => pending.transaction },
    snapshot: async () => ({ baton: pending.baton, context: 'new fee' }),
  }), /has not indexed/);
});

test('a competing baton advance drops stale work without broadcasting or counting a win', async () => {
  const calls = [];
  const result = await resolveSubmission({ ...defaults,
    session: { rpc: async method => { calls.push(method); throw Object.assign(new Error('not found'), { code: 1 }); } },
    snapshot: async () => ({ baton: 'successor' }),
  });
  assert.equal(result, 'stale');
  assert.deepEqual(calls, ['blockchain.transaction.get']);
});

test('only a server refusing the claim as not final yet counts as a refusal', () => {
  assert.ok(refusedAsNotFinal(notFinal()));
  assert.ok(refusedAsNotFinal(Object.assign(new Error('bad-txns-nonfinal, non-final transaction (code 64)'), { code: -26 })));
  assert.ok(!refusedAsNotFinal(new Error('non-BIP68-final')), 'a message without a server error code');
  assert.ok(!refusedAsNotFinal(new Error('Server timed out')));
  assert.ok(!refusedAsNotFinal(Object.assign(new Error('txn-mempool-conflict (code 18)'), { code: 1 })));
});

test('a claim refused as not final waits for the next block and sends the same bytes again, uncounted', async () => {
  const calls = [];
  let blocks = 0, accepted = false;
  const result = await resolveSubmission({ ...defaults,
    waitForBlock: async () => { blocks++; },
    session: { rpc: async (method, params) => {
      calls.push([method, ...(params ?? [])]);
      if (method.endsWith('.get')) throw notFound();
      if (blocks === 0) throw notFinal();
      accepted = true;
      return pending.txid;
    } },
    snapshot: async () => ({ baton: accepted ? 'successor' : pending.baton }),
  });
  assert.equal(result, 'accepted');
  assert.equal(blocks, 1);
  assert.deepEqual(calls, [
    ['blockchain.transaction.get', pending.txid],
    ['blockchain.transaction.broadcast', pending.transaction],
    // Nothing is sent while the claim waits for the block.
    ['blockchain.transaction.get', pending.txid],
    ['blockchain.transaction.broadcast', pending.transaction],
  ]);
});

test('a claim waiting for a block is dropped as stale once another claim takes the baton', async () => {
  const calls = [];
  let blocks = 0;
  const result = await resolveSubmission({ ...defaults,
    waitForBlock: async () => { blocks++; },
    session: { rpc: async method => { calls.push(method); if (method.endsWith('.get')) throw notFound(); throw notFinal(); } },
    snapshot: async () => ({ baton: blocks ? 'theirs' : pending.baton }),
  });
  assert.equal(result, 'stale');
  assert.deepEqual(calls, ['blockchain.transaction.get', 'blockchain.transaction.broadcast', 'blockchain.transaction.get']);
});

test('a claim waiting for a block stays pending on stop and is sent once per block', async () => {
  let stopped = false, blocks = 0, sends = 0;
  const result = await resolveSubmission({ ...defaults,
    stopped: () => stopped,
    waitForBlock: async () => { if (++blocks === 5) stopped = true; },
    session: { rpc: async method => { if (method.endsWith('.get')) throw notFound(); sends++; throw notFinal(); } },
    snapshot: async () => ({ baton: pending.baton }),
  });
  assert.equal(result, 'pending');
  assert.equal(blocks, 5);
  assert.equal(sends, 5);
});

test('another refusal or a lost reply still fails the attempt for the bounded retries', async () => {
  for (const failure of [Object.assign(new Error('txn-mempool-conflict (code 18)'), { code: 1 }), new Error('Server timed out')]) {
    await assert.rejects(resolveSubmission({ ...defaults,
      session: { rpc: async method => { if (method.endsWith('.get')) throw notFound(); throw failure; } },
      snapshot: async () => ({ baton: pending.baton }),
    }), error => error === failure);
  }
});
