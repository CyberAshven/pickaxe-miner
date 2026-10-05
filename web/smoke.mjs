// Exercise the actual generated WASM, without consuming a GPU or broadcasting.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { buildFile, buildModule } from './test-build.mjs';
const { default: init, browser_config, BrowserMiner, BrowserControls, validate_payout_address } = await buildModule('pkg/pickaxe_miner.js');

await init({ module_or_path: await readFile(buildFile('pkg/pickaxe_miner_bg.wasm')) });
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

// Exercise the actual shared Rust pacing through WASM with coarse browser timers.
for (const intensity of [10, 25, 50, 75, 100]) {
  const controls = new BrowserControls(0);
  let now = 0, work = 0;
  while (now < 30000) {
    now += 1; work++;
    const rest = controls.record(1_000_000, 1, now, intensity);
    if (rest > 0) now += Math.ceil(rest / 16) * 16;
  }
  assert.ok(Math.abs(work / now - intensity / 100) < .02);
  assert.ok(Math.abs(controls.active_rate() - 1e9) < .001);
  assert.ok(Math.abs(controls.current_rate(now) / 1e9 - intensity / 100) < .03);
  assert.equal(controls.current_rate(now + 6000), 0);
  controls.reset(now + 10000);
  assert.equal(controls.current_rate(now + 10000), 0);
  assert.ok(controls.record(1_000_000, 60, now + 10060, 50) >= 50);
  controls.free();
}
console.log('Shared Rust pacing and time-weighted rate passed through generated WASM.');
