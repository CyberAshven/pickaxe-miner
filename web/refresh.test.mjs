import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildModule } from './test-build.mjs';
const { SnapshotRefresh } = await buildModule('refresh.js');
const flush = () => new Promise(resolve => setImmediate(resolve));

function fixture(...timing) {
  let now = 0, active = 0, maxActive = 0;
  const reads = [];
  const reader = new SnapshotRefresh(() => {
    maxActive = Math.max(maxActive, ++active);
    return new Promise((resolve, reject) => reads.push({
      resolve: value => { --active; resolve(value); },
      reject: error => { --active; reject(error); },
    }));
  }, () => now, ...timing);
  return { reader, reads, at: t => { now = t; }, maxActive: () => maxActive };
}

async function initialize(f) {
  const initial = f.reader.fresh();
  await flush();
  f.at(300); f.reads[0].resolve('initial');
  assert.equal(await initial, 'initial');
}

test('server reads overlap work without changing the one-second freshness boundary', async () => {
  const f = fixture();
  await initialize(f);
  f.at(999); assert.equal(await f.reader.beforeBatch(), undefined);
  assert.equal(f.reads.length, 1);
  f.at(1000); assert.equal(await f.reader.beforeBatch(), undefined);
  await flush(); assert.equal(f.reads.length, 2);
  f.at(1299); assert.equal(await f.reader.beforeBatch(), undefined);
  f.at(1300);
  let ready = false;
  const expired = f.reader.beforeBatch().then(v => { ready = true; return v; });
  await flush(); assert.equal(ready, false);
  f.at(1350); f.reads[1].resolve('new baton');
  assert.equal(await expired, 'new baton');
  assert.equal(f.maxActive(), 1);
});

test('completed reads are delivered at the next batch boundary and only once', async () => {
  const f = fixture();
  await initialize(f);
  f.at(1000); await f.reader.beforeBatch(); await flush();
  f.at(1200); f.reads[1].resolve('updated'); await flush();
  assert.equal(await f.reader.beforeBatch(), 'updated');
  assert.equal(await f.reader.beforeBatch(), undefined);
  assert.equal(f.reads.length, 2);
});

test('winner validation drains a pre-winner read and fetches again without overlap', async () => {
  const f = fixture();
  await initialize(f);
  f.at(1000); await f.reader.beforeBatch(); await flush();
  const validation = f.reader.fresh();
  await flush(); assert.equal(f.reads.length, 2);
  f.reads[1].resolve('pre-winner'); await flush();
  assert.equal(f.reads.length, 3);
  f.reads[2].resolve('post-winner');
  assert.equal(await validation, 'post-winner');
  assert.equal(f.maxActive(), 1);
});

test('a reply completed before suspension is discarded rather than made fresh', async () => {
  const f = fixture();
  await initialize(f);
  f.at(1000); await f.reader.beforeBatch(); await flush();
  f.at(1200); f.reads[1].resolve('old'); await flush();
  f.at(120000);
  let ready = false;
  const resumed = f.reader.beforeBatch().then(v => { ready = true; return v; });
  await flush(); assert.equal(ready, false);
  assert.equal(f.reads.length, 3);
  f.reads[2].resolve('after resume');
  assert.equal(await resumed, 'after resume');
});

test('background failure is handled then thrown before another GPU batch', async () => {
  const f = fixture();
  await initialize(f);
  f.at(1000); await f.reader.beforeBatch(); await flush();
  f.reads[1].reject(new Error('connection lost')); await flush();
  await assert.rejects(f.reader.beforeBatch(), /connection lost/);
  assert.equal(f.reads.length, 2);
});

test('stop/reconnect drains the old reader and never delivers late data', async () => {
  const f = fixture();
  await initialize(f);
  f.at(1000); await f.reader.beforeBatch(); await flush();
  f.reader.close();
  f.reads[1].reject(new Error('socket closed'));
  await f.reader.drain();
  await assert.rejects(f.reader.beforeBatch(), /closed/);
  await assert.rejects(f.reader.fresh(), /closed/);
  assert.equal(f.reads.length, 2);
});

test('a subscribed reader reads on a server notice at most once a second, else every 30 s', async () => {
  const f = fixture(30000, 1000);
  await initialize(f);
  f.at(5000); assert.equal(await f.reader.beforeBatch(), undefined);
  assert.equal(f.reads.length, 1);
  f.reader.notice();
  f.at(5001); await f.reader.beforeBatch(); await flush();
  assert.equal(f.reads.length, 2);
  f.at(5100); f.reads[1].resolve('claim seen'); await flush();
  assert.equal(await f.reader.beforeBatch(), 'claim seen');
  // A burst of notices within a second waits for the gap.
  f.reader.notice();
  f.at(5500); await f.reader.beforeBatch(); await flush();
  assert.equal(f.reads.length, 2);
  f.at(6001); await f.reader.beforeBatch(); await flush();
  assert.equal(f.reads.length, 3);
  f.reads[2].resolve('block'); await flush();
  assert.equal(await f.reader.beforeBatch(), 'block');
  // With no notices, the next read is the safety read 30 s later, and the
  // GPU waits for it, since the last delivered state is then 30 s old.
  f.at(36000); await f.reader.beforeBatch(); await flush();
  assert.equal(f.reads.length, 3);
  f.at(36001);
  const safety = f.reader.beforeBatch(); await flush();
  assert.equal(f.reads.length, 4);
  f.reads[3].resolve('safety');
  assert.equal(await safety, 'safety');
});

test('ten quiet minutes cost twenty reads subscribed, and one read every 2 s without', async () => {
  for (const [interval, expected] of [[30000, 20], [2000, 300]]) {
    const f = fixture(interval, 1000);
    await initialize(f);
    for (let t = 1000; t <= 600000; t += 100) {
      f.at(t);
      const started = f.reads.length;
      const batch = f.reader.beforeBatch(); await flush();
      // A read due now is answered at once; the batch may wait for it.
      if (f.reads.length > started) f.reads.at(-1).resolve(t);
      await batch; await flush();
    }
    assert.equal(f.reads.length - 1, expected, `interval ${interval}`);
    assert.equal(f.maxActive(), 1);
  }
});
