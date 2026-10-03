import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Electrum } from './rpc.js';

test('WebSocket framing preserves notifications, matches IDs, and rejects server errors', async () => {
  const sent = [];
  const socket = { readyState: 1, send: s => sent.push(JSON.parse(s)), close() {} };
  const rpc = new Electrum(socket);
  const first = rpc.rpc('example');
  socket.onmessage({ data: '{"method":"notification","params":[]}' });
  socket.onmessage({ data: '{"id":1,"res' });
  socket.onmessage({ data: 'ult":{"amount":"9007199254740993"}}\n' });
  assert.deepEqual(await first, { amount: '9007199254740993' });
  const second = rpc.rpc('unsupported');
  socket.onmessage({ data: '{"id":2,"error":{"code":-32601,"message":"not available"}}' });
  await assert.rejects(second, e => e.code === -32601);
  const third = rpc.rpc('disconnect');
  rpc.close(new Error('offline'));
  await assert.rejects(third, /offline/);
  assert.equal(rpc.pending.size, 0);
  assert.deepEqual(sent.map(s => s.id), [1, 2, 3]);
});
