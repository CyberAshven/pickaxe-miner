import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildModule } from './test-build.mjs';
const { SnapshotRefresh } = await buildModule('refresh.js');
const flush = () => new Promise(resolve => setImmediate(resolve));

function fixture() {
  let now = 0, active = 0, maxActive = 0;
  const reads = [];
  const reader = new SnapshotRefresh(() => {
    maxActive = Math.max(maxActive, ++active);
    return new Promise((resolve, reject) => reads.push({
      resolve: value => { --active; resolve(value); },
      reject: error => { --active; reject(error); },
    }));
  }, () => now);
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
