// Exercise the actual generated WASM, without consuming a GPU or broadcasting.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import init, { browser_config, BrowserMiner } from '../dist/web/pkg/pickaxe_miner.js';

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
console.log('Generated WASM configuration and invalid-input checks passed.');
