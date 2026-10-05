// Browser GPU miner page. Mining runs in the Rust WebAssembly module
// (src/browser.rs) on the browser's WebGPU; this file wires the page,
// network I/O and display. See docs/portable.md.
import init, { BrowserMiner, BrowserControls, browser_config, validate_payout_address } from './pkg/pickaxe_miner.js';
import { Electrum, rpcErrorCode } from './rpc.js';
import { resolveSubmission } from './submission.js';
import { SnapshotRefresh } from './refresh.js';
import { MiningError, searchBatch, unsupportedReason, isPendingReward, parsePendingReward, type PendingReward } from './platform.js';

function element<T extends HTMLElement>(id: string, type: { new(): T }): T {
  const value = document.getElementById(id);
  if (!(value instanceof type)) throw new Error(`Missing interface element: ${id}`);
  return value;
}
const elements = {
  network: element('network', HTMLSelectElement), token: element('token', HTMLSelectElement),
  address: element('address', HTMLInputElement), endpoints: element('endpoints', HTMLInputElement),
  intensity: element('intensity', HTMLInputElement), intensityLabel: element('intensityLabel', HTMLOutputElement),
  setup: element('setup', HTMLFormElement), start: element('start', HTMLButtonElement), stop: element('stop', HTMLButtonElement),
  status: element('status', HTMLParagraphElement), events: element('events', HTMLPreElement), stats: element('stats', HTMLPreElement),
};
const $ = <K extends keyof typeof elements>(id: K): typeof elements[K] => elements[id];
interface BrowserConfig { tokens: string[]; endpoints: string[]; scriptHash: string }
interface RawSnapshot { before: string; unspent: string; after: string; fee: string }
function parseConfig(raw: string): BrowserConfig {
  const value: unknown = JSON.parse(raw);
  if (typeof value !== 'object' || value === null || !('tokens' in value) || !Array.isArray(value.tokens)
      || !value.tokens.every((v: unknown) => typeof v === 'string') || !('endpoints' in value) || !Array.isArray(value.endpoints)
      || !value.endpoints.every((v: unknown) => typeof v === 'string') || !('scriptHash' in value) || typeof value.scriptHash !== 'string') {
    throw new Error('Invalid shared engine configuration');
  }
  return { tokens: value.tokens, endpoints: value.endpoints, scriptHash: value.scriptHash };
}
const wait = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));
const status = (text: string) => {
  if ($('status').textContent !== text) $('status').textContent = text;
};
const log = (text: string) => { $('events').textContent = `${text}\n${$('events').textContent}`.split('\n').slice(0, 16).join('\n'); };
let stopping = false;
let ready = false;
let config: BrowserConfig;
function networkChanged() {
  config = parseConfig(browser_config($('network').value));
  $('token').replaceChildren(...config.tokens.map(token => new Option(token, token)));
  $('address').value = localStorage.getItem(`pickaxe.address.${$('network').value}`) || '';
}
$('network').onchange = networkChanged;
const showIntensity = () => { $('intensityLabel').textContent = `${$('intensity').value}%`; };
$('intensity').oninput = showIntensity;
showIntensity();
$('stop').onclick = () => { stopping = true; status('Stopping after the current batch/submission…'); };

async function run() {
  const network = $('network').value;
  let address = $('address').value.trim();
  const endpoints = [...new Set(($('endpoints').value.trim() ? $('endpoints').value.split(',').map(s => s.trim()) : config.endpoints))];
  const journal = `pickaxe.pending.${network}`;
  const settings = $('setup').querySelectorAll<HTMLInputElement | HTMLSelectElement>('input:not([type=range]),select');
  settings.forEach(input => { input.disabled = true; });
  $('start').disabled = true;
  $('stop').disabled = false;
  stopping = false;
  let miner: BrowserMiner | undefined;
  let session: Electrum | undefined;
  let refresh: SnapshotRefresh<RawSnapshot> | undefined;
  let candidates = 0, wins = 0;
  const started = performance.now();
  const controls = new BrowserControls(started);
  let feeChecked = 0, endpointIndex = 0;
  let fee = '';
  function connected(): Electrum { if (!session) throw new Error('Not connected'); return session; }
  function engine(): BrowserMiner { if (!miner) throw new Error('Mining engine is not ready'); return miner; }
  async function connect() {
    let lastError;
    for (let i = 0; i < endpoints.length && !stopping; i++) {
      const endpoint = endpoints[endpointIndex++ % endpoints.length];
      if (!endpoint) throw new Error('No server available');
      try {
        session = await Electrum.connect(endpoint);
        feeChecked = 0;
        const source = session;
        refresh = new SnapshotRefresh(() => fetchSnapshot(source), () => performance.now());
        log(`Connected: ${new URL(endpoint).hostname}`);
        return;
      } catch (error) { lastError = error; await wait(1000); }
    }
    throw lastError || new Error('No server available');
  }
  async function fetchSnapshot(source: Electrum): Promise<RawSnapshot> {
    const before = await source.rpcRaw('blockchain.headers.subscribe');
    const unspent = await source.rpcRaw('blockchain.scripthash.listunspent', [config.scriptHash, 'include_tokens']);
    const after = await source.rpcRaw('blockchain.headers.subscribe');
    if (performance.now() - feeChecked >= 30000 || !feeChecked) {
      try {
        fee = await source.rpcRaw('mempool.get_info');
      } catch (error) {
        if (rpcErrorCode(error) !== -32601) throw error;
        fee = await source.rpcRaw('blockchain.relayfee');
      }
      feeChecked = performance.now();
    }
    return { before, unspent, after, fee };
  }
  function applySnapshot({ before, unspent, after, fee }: RawSnapshot) {
    const context = engine().set_snapshot(before, unspent, after, fee);
    return { context, baton: engine().baton() };
  }
  async function snapshot() {
    if (!refresh) throw new Error('Snapshot reader is not ready');
    return applySnapshot(await refresh.fresh());
  }
  async function settle(pending: PendingReward) {
    const result = await resolveSubmission({ session: connected(), snapshot, pending, stopped: () => stopping, wait, progress: status });
    if (result !== 'pending') {
      localStorage.removeItem(journal);
      if (result === 'accepted') { wins++; log(`Reward accepted: ${pending.txid}`); }
      else log('Previous work is stale; using the current baton.');
    }
  }
  // #### PR #22: show recent completed work per wall second. Active-only GPU
  // speed hides intensity pauses; a lifetime average reacts too slowly.
  const render = (stopped = false) => {
    const now = performance.now();
    const rate = stopped ? 0 : controls.current_rate(now);
    const average = now > started ? candidates / (now - started) / 1000 : 0;
    $('stats').textContent = `Hashrate ${(rate / 1e6).toFixed(2)} MH/s · average ${average.toFixed(2)} MH/s\n${wins} accepted · ${Math.floor((now - started) / 1000)}s elapsed`;
  };
  const renderTimer = setInterval(render, 1000);
  try {
    address = validate_payout_address(network, address);
    $('address').value = address;
    status('Loading GPU table and compiling WebGPU…');
    const response = await fetch('./photon-generator-table.bin');
    if (!response.ok) throw new Error('GPU table could not be downloaded');
    miner = await BrowserMiner.create(network, address, new Uint8Array(await response.arrayBuffer()));
    localStorage.setItem(`pickaxe.address.${network}`, address);
    await connect();
    await snapshot();
    controls.reset(performance.now());
    let failures = 0;
    while (!stopping) {
      try {
        const pending = localStorage.getItem(journal);
        if (pending) { status('Resolving pending reward…'); await settle(parsePendingReward(pending)); controls.reset(performance.now()); continue; }
        const update = await refresh?.beforeBatch();
        if (update) applySnapshot(update);
        if (stopping) break;
        status(candidates ? 'Mining' : 'Preparing the first GPU batch; shader compilation can take several minutes…');
        const batchStart = performance.now();
        const result = await searchBatch(miner);
        const elapsed = performance.now() - batchStart;
        const firstBatch = candidates === 0;
        candidates += result.candidates;
        const now = performance.now();
        const rest = controls.record(result.candidates, elapsed, now, Number($('intensity').value));
        // A driver may compile on first submission. Do not turn that startup wait into a long throttle delay.
        if (firstBatch) controls.reset(now);
        if (isPendingReward(result) && (await snapshot()).context === result.context) {
          localStorage.setItem(journal, JSON.stringify(result));
          await settle(result);
          controls.reset(performance.now());
        }
        failures = 0;
        if (!firstBatch && rest > 0 && !stopping) await wait(Math.min(rest, 1000));
      } catch (error) {
        if (error instanceof MiningError) throw error;
        // Bounded retries preserve the journal and surface persistent failures.
        if (++failures > 3) throw error;
        log(String(error)); status('Refreshing connection…');
        refresh?.close(); session?.close(); await refresh?.drain();
        await wait(2000); await connect(); await snapshot(); controls.reset(performance.now());
      }
    }
    status('Stopped');
  } finally {
    clearInterval(renderTimer);
    render(true);
    refresh?.close(); session?.close(); await refresh?.drain();
    miner?.free(); controls.free();
    settings.forEach(input => { input.disabled = false; });
    $('start').disabled = false; $('stop').disabled = true;
  }
}

$('setup').onsubmit = async event => {
  event.preventDefault();
  if (!ready) return;
  try {
    const unsupported = unsupportedReason();
    if (unsupported) throw new Error(unsupported);
    await navigator.locks.request('pickaxe-gpu', { ifAvailable: true }, async lock => {
      if (!lock) throw new Error('Another Pickaxe tab on this site is already mining');
      await run();
    });
  } catch (error) { status(String(error)); }
};
const unsupported = unsupportedReason();
if (unsupported) { $('start').disabled = true; status(unsupported); }
else {
  try { await init(); networkChanged(); ready = true; status('Ready'); }
  catch (error) { $('start').disabled = true; status(`Could not load shared Rust engine: ${error}`); }
}
