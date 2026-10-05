import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildModule } from './test-build.mjs';
const { Electrum } = await buildModule('rpc.js');

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

test('raw RPC responses retain integers beyond JavaScript precision across frames', async () => {
  const socket = { readyState: 1, send() {}, close() {} };
  const rpc = new Electrum(socket);
  const response = '{"id":1,"result":{"amount":9223372036854775807}}';
  const request = rpc.rpcRaw('blockchain.scripthash.listunspent');
  socket.onmessage({ data: response.slice(0, 25) });
  socket.onmessage({ data: response.slice(25) + '\n' });
  assert.equal(await request, response);
  const unframed = rpc.rpcRaw('blockchain.scripthash.listunspent');
  const second = response.replace('"id":1', '"id":2');
  socket.onmessage({ data: second });
  assert.equal(await unframed, second);
  rpc.close();
});
