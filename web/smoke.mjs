// Exercise the actual generated WASM, without consuming a GPU or broadcasting.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import init, { browser_config, BrowserMiner, validate_payout_address } from '../dist/web/pkg/pickaxe_miner.js';

await init({ module_or_path: await readFile(new URL('../dist/web/pkg/pickaxe_miner_bg.wasm', import.meta.url)) });
const main = JSON.parse(browser_config('mainnet'));
const chip = JSON.parse(browser_config('chipnet'));
const manifest = await readFile(new URL('../Cargo.toml', import.meta.url), 'utf8');
assert.equal(main.version, manifest.match(/^version = "([^"]+)"/m)[1]);
assert.deepEqual(main.tokens, ['PHOTON']);
assert.match(main.scriptHash, /^[0-9a-f]{64}$/);
assert.notDeepEqual(main.endpoints, chip.endpoints);
assert.ok(main.endpoints.length && chip.endpoints.length);
assert.throws(() => browser_config('invalid'));
await assert.rejects(BrowserMiner.create('mainnet', 'invalid', new Uint8Array()));

// Independently encoded with libauth: P2PKH and token-aware P2PKH, hash [0x42; 20].
const addresses = {
  mainnet: ['bitcoincash:qppyysjzgfpyysjzgfpyysjzgfpyysjzggkqscq4cy', 'bitcoincash:zppyysjzgfpyysjzgfpyysjzgfpyysjzgg32rxwn8h'],
  chipnet: ['bchtest:qppyysjzgfpyysjzgfpyysjzgfpyysjzggjj5lzzlc', 'bchtest:zppyysjzgfpyysjzgfpyysjzgfpyysjzgg4c8pvyqt'],
};
let invalidCases = 0;
for (const [network, valid] of Object.entries(addresses)) {
  for (const address of valid) {
    for (const input of [address, address.toUpperCase(), address.split(':')[1], `  ${address}  `]) {
      assert.equal(validate_payout_address(network, input), address);
    }
  }
  const other = addresses[network === 'mainnet' ? 'chipnet' : 'mainnet'];
  const invalid = [
    ...other.map(address => [address, 'payout address must use']),
    ...other.map(address => [address.split(':')[1], 'checksum invalid']),
    [valid[0].slice(0, -1) + 'q', 'checksum invalid'],
    [valid[0].replace(':q', ':Q'), 'must not mix upper and lower case'],
    ['', 'payout address required'],
  ];
  for (const [address, message] of invalid) {
    const matches = error => String(error).includes(message);
    assert.throws(() => validate_payout_address(network, address), matches);
    // Exercise actual startup too; rejection must happen before GPU initialization.
    await assert.rejects(BrowserMiner.create(network, address, new Uint8Array()), matches);
    invalidCases++;
  }
}
console.log(`Generated WASM configuration, 16 valid address forms and ${invalidCases} invalid startup cases passed.`);
