// Browser miner page. Mining runs in the Rust WebAssembly module
// (src/browser.rs) on browser WebGPU. This file is the page; the session
// is runMiner (miner.ts), which another page can call without this HTML.
// See docs/portable.md.
import init, { browser_config } from './pkg/pickaxe_miner.js';
import { unsupportedReason } from './platform.js';
import { runMiner } from './miner.js';

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
function parseConfig(raw: string): BrowserConfig {
  const value: unknown = JSON.parse(raw);
  if (typeof value !== 'object' || value === null || !('tokens' in value) || !Array.isArray(value.tokens)
      || !value.tokens.every((v: unknown) => typeof v === 'string') || !('endpoints' in value) || !Array.isArray(value.endpoints)
      || !value.endpoints.every((v: unknown) => typeof v === 'string') || !('scriptHash' in value) || typeof value.scriptHash !== 'string') {
    throw new Error('Invalid shared engine configuration');
  }
  return { tokens: value.tokens, endpoints: value.endpoints, scriptHash: value.scriptHash };
}
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
  const address = $('address').value.trim();
  const endpoints = [...new Set(($('endpoints').value.trim() ? $('endpoints').value.split(',').map(s => s.trim()) : config.endpoints))];
  const settings = $('setup').querySelectorAll<HTMLInputElement | HTMLSelectElement>('input:not([type=range]),select');
  settings.forEach(input => { input.disabled = true; });
  $('start').disabled = true;
  $('stop').disabled = false;
  stopping = false;
  try {
    await runMiner({
      network,
      address,
      endpoints,
      scriptHash: config.scriptHash,
      intensity: () => Number($('intensity').value),
      stopped: () => stopping,
      status,
      log,
      stats: text => { $('stats').textContent = text; },
      onAddress: value => { $('address').value = value; },
      load: key => localStorage.getItem(key),
      save: (key, value) => { localStorage.setItem(key, value); },
      remove: key => { localStorage.removeItem(key); },
      table: async () => {
        const response = await fetch('./photon-generator-table.bin');
        if (!response.ok) throw new Error('GPU table could not be downloaded');
        return new Uint8Array(await response.arrayBuffer());
      },
    });
  } finally {
    settings.forEach(input => { input.disabled = false; });
    $('start').disabled = false;
    $('stop').disabled = true;
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
