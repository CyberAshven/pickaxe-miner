import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildModule } from './test-build.mjs';
const { resolveSubmission } = await buildModule('submission.js');

const pending = { txid: 'ab'.repeat(32), transaction: '01020304', baton: `${'cd'.repeat(32)}:0`, context: 'original' };
const defaults = { pending, stopped: () => false, wait: async () => {}, progress() {} };

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
