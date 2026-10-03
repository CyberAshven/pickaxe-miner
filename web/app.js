import init, { BrowserMiner, browser_config, validate_payout_address } from './pkg/pickaxe_miner.js';
import { Electrum } from './rpc.js';
import { resolveSubmission } from './submission.js';

const $ = id => document.getElementById(id);
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
const status = text => { $('status').textContent = text; };
const log = text => { $('events').textContent = `${text}\n${$('events').textContent}`.split('\n').slice(0, 16).join('\n'); };
let stopping = false;
let ready = false;
let config;
function networkChanged() {
  config = JSON.parse(browser_config($('network').value));
  $('token').replaceChildren(...config.tokens.map(token => new Option(token, token)));
  $('address').value = localStorage.getItem(`pickaxe.address.${$('network').value}`) || '';
}
$('network').onchange = networkChanged;
$('intensity').oninput = () => { $('intensityLabel').textContent = `${$('intensity').value}%`; };
$('stop').onclick = () => { stopping = true; status('Stopping after the current batch/submission…'); };

async function run() {
  const network = $('network').value;
  let address = $('address').value.trim();
  const endpoints = [...new Set(($('endpoints').value.trim() ? $('endpoints').value.split(',').map(s => s.trim()) : config.endpoints))];
  const journal = `pickaxe.pending.${network}`;
  const settings = $('setup').querySelectorAll('input:not([type=range]),select');
  settings.forEach(input => { input.disabled = true; });
  $('start').disabled = true;
  $('stop').disabled = false;
  stopping = false;
  let miner, session;
  let candidates = 0, wins = 0;
  const started = performance.now();
  let lastSnapshot = 0, feeChecked = 0, fee, endpointIndex = 0;
  async function connect() {
    let lastError;
    for (let i = 0; i < endpoints.length && !stopping; i++) {
      const endpoint = endpoints[endpointIndex++ % endpoints.length];
      try {
        session = await Electrum.connect(endpoint);
        feeChecked = 0;
        log(`Connected: ${new URL(endpoint).hostname}`);
        return;
      } catch (error) { lastError = error; await wait(1000); }
    }
    throw lastError || new Error('No server available');
  }
  async function snapshot() {
    const before = await session.rpcRaw('blockchain.headers.subscribe');
    const unspent = await session.rpcRaw('blockchain.scripthash.listunspent', [config.scriptHash, 'include_tokens']);
    const after = await session.rpcRaw('blockchain.headers.subscribe');
    if (performance.now() - feeChecked >= 30000 || !feeChecked) {
      try {
        fee = await session.rpcRaw('mempool.get_info');
      } catch (error) {
        if (error.code !== -32601) throw error;
        fee = await session.rpcRaw('blockchain.relayfee');
      }
      feeChecked = performance.now();
    }
    const context = miner.set_snapshot(before, unspent, after, fee);
    lastSnapshot = performance.now();
    return { context, baton: miner.baton() };
  }
  async function settle(pending) {
    const result = await resolveSubmission({ session, snapshot, pending, stopped: () => stopping, wait, progress: status });
    if (result !== 'pending') {
      localStorage.removeItem(journal);
      if (result === 'accepted') { wins++; log(`Reward accepted: ${pending.txid}`); }
      else log('Previous work is stale; using the current baton.');
    }
  }
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
    let failures = 0;
    while (!stopping) {
      try {
        const pending = localStorage.getItem(journal);
        if (pending) { status('Resolving pending reward…'); await settle(JSON.parse(pending)); continue; }
        if (performance.now() - lastSnapshot > 1000) await snapshot();
        status(candidates ? 'Mining' : 'Preparing the first GPU batch; shader compilation can take several minutes…');
        const batchStart = performance.now();
        const result = JSON.parse(await miner.search());
        const elapsed = performance.now() - batchStart;
        candidates += result.candidates;
        $('stats').textContent = `Active GPU ${(result.candidates / Math.max(elapsed, 1) / 1000).toFixed(2)} MH/s · wall average ${(candidates / (performance.now() - started) / 1000).toFixed(2)} MH/s\n${wins} accepted · ${Math.floor((performance.now() - started) / 1000)}s elapsed`;
        if (result.transaction && (await snapshot()).context === result.context) {
          localStorage.setItem(journal, JSON.stringify(result));
          await settle(result);
        }
        failures = 0;
        await wait(Math.max(0, elapsed * (100 / Number($('intensity').value) - 1)));
      } catch (error) {
        // Bounded retries preserve the journal and surface persistent failures.
        if (++failures > 3) throw error;
        log(String(error)); status('Refreshing connection…');
        session?.close(); await wait(2000); await connect(); await snapshot();
      }
    }
    status('Stopped');
  } finally {
    session?.close(); miner?.free();
    settings.forEach(input => { input.disabled = false; });
    $('start').disabled = false; $('stop').disabled = true;
  }
}

$('setup').onsubmit = async event => {
  event.preventDefault();
  if (!ready) return;
  try {
    if (!navigator.gpu || !navigator.locks) throw new Error('This browser needs WebGPU and Web Locks on HTTPS or localhost');
    await navigator.locks.request('pickaxe-gpu', { ifAvailable: true }, async lock => {
      if (!lock) throw new Error('Another Pickaxe tab on this site is already mining');
      await run();
    });
  } catch (error) { status(String(error)); }
};
try { await init(); networkChanged(); ready = true; status('Ready'); }
catch (error) { status(`Could not load shared Rust engine: ${error}`); }
