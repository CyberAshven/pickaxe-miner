// What: the browser mining session, without Pickaxe's page.
// Why: a site with its own page calls this build and pins the version.
// The donation stays in the Rust engine (src/browser.rs). Upgrading is
// replacing these files. Look here if: start, stop, or a reward submission
// behaves differently from the Pickaxe page.
import { BrowserMiner, BrowserControls, validate_payout_address, describe_browser_work } from './pkg/pickaxe_miner.js';
import { Electrum, rpcErrorCode } from './rpc.js';
import { resolveSubmission } from './submission.js';
import { SnapshotRefresh } from './refresh.js';
import { MiningError, searchBatch, isPendingReward, parsePendingReward, type PendingReward } from './platform.js';

export interface MinerSession {
  network: string;
  address: string;
  endpoints: string[];
  scriptHash: string;
  /** Read each batch, so a live control still applies. */
  intensity: () => number;
  stopped: () => boolean;
  status: (text: string) => void;
  log: (text: string) => void;
  stats: (text: string) => void;
  /** The address the engine accepted, before any GPU work. */
  onAddress: (address: string) => void;
  load: (key: string) => string | null;
  save: (key: string, value: string) => void;
  remove: (key: string) => void;
  /** The PHOTON generator table shipped beside this build. */
  table: () => Promise<Uint8Array>;
  /** Empty and 0 mean there is no host fee. At most 10% of the work. */
  hostAddress?: string;
  hostPercent?: number;
  /** 0 means the token's donation. A host cannot set it lower. */
  donationPercent?: number;
}

interface RawSnapshot { before: string; unspent: string; after: string; fee: string }

const wait = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));

/** The status line the Pickaxe page shows. `rate` and `average` are hashes per second. */
export function formatMinerStats(rate: number, average: number, wins: number, elapsedMs: number): string {
  return `Hashrate ${(rate / 1e6).toFixed(2)} MH/s · average ${average.toFixed(2)} MH/s\n${wins} accepted · ${Math.floor(elapsedMs / 1000)}s elapsed`;
}

export async function runMiner(call: MinerSession): Promise<void> {
  const network = call.network;
  let address = call.address.trim();
  const endpoints = call.endpoints;
  const journal = `pickaxe.pending.${network}`;
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
    for (let i = 0; i < endpoints.length && !call.stopped(); i++) {
      const endpoint = endpoints[endpointIndex++ % endpoints.length];
      if (!endpoint) throw new Error('No server available');
      try {
        session = await Electrum.connect(endpoint);
        feeChecked = 0;
        const source = session;
        // Read the live state when the server reports a change, as the
        // native miner does; a server without subscriptions is read every 2 s.
        let watched = false;
        try {
          await source.rpcRaw('blockchain.scripthash.subscribe', [call.scriptHash]);
          watched = true;
        } catch { watched = false; }
        const reader = new SnapshotRefresh(() => fetchSnapshot(source), () => performance.now(),
          watched ? 30000 : 2000, 1000);
        source.onNotice = () => reader.notice();
        refresh = reader;
        call.log(`Connected: ${new URL(endpoint).hostname}`);
        return;
      } catch (error) { lastError = error; await wait(1000); }
    }
    throw lastError || new Error('No server available');
  }
  async function fetchSnapshot(source: Electrum): Promise<RawSnapshot> {
    const before = await source.rpcRaw('blockchain.headers.subscribe');
    const unspent = await source.rpcRaw('blockchain.scripthash.listunspent', [call.scriptHash, 'include_tokens']);
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
  // A claim refused until its baton confirms waits on this connection for the
  // next block notice (at most 30 s between checks), waking each second for Stop.
  async function waitForBlock() {
    const source = connected();
    const until = performance.now() + 30000;
    while (!call.stopped() && source.open && performance.now() < until) {
      if (await source.nextBlock(Math.min(1000, until - performance.now()))) return;
    }
  }
  async function settle(pending: PendingReward) {
    const result = await resolveSubmission({
      session: connected(), snapshot, pending, stopped: () => call.stopped(), wait, waitForBlock, progress: call.status,
    });
    if (result !== 'pending') {
      call.remove(journal);
      if (result === 'accepted') { wins++; call.log(`Reward accepted: ${pending.txid}`); }
      else call.log('Previous work is stale; using the current baton.');
    }
  }
  // Show recent completed work per wall second. Active-only GPU
  // speed hides intensity pauses; a lifetime average reacts too slowly.
  const render = (stopped = false) => {
    const now = performance.now();
    const rate = stopped ? 0 : controls.current_rate(now);
    const average = now > started ? candidates / (now - started) / 1000 : 0;
    call.stats(formatMinerStats(rate, average, wins, now - started));
  };
  const renderTimer = setInterval(render, 1000);
  try {
    address = validate_payout_address(network, address);
    call.onAddress(address);
    const hostAddress = call.hostAddress ?? '';
    const hostPercent = call.hostPercent ?? 0;
    const donationPercent = call.donationPercent ?? 0;
    call.log(describe_browser_work(network, donationPercent, hostPercent));
    call.status('Loading GPU table and compiling browser WebGPU shaders…');
    miner = await BrowserMiner.create(
      network, address, await call.table(), hostAddress, hostPercent, donationPercent,
    );
    call.save(`pickaxe.address.${network}`, address);
    await connect();
    await snapshot();
    controls.reset(performance.now());
    let failures = 0;
    while (!call.stopped()) {
      try {
        const pending = call.load(journal);
        if (pending) { call.status('Resolving pending reward…'); await settle(parsePendingReward(pending)); controls.reset(performance.now()); continue; }
        const update = await refresh?.beforeBatch();
        if (update) applySnapshot(update);
        if (call.stopped()) break;
        call.status(candidates ? 'Mining' : 'Preparing the first GPU batch; shader compilation can take several minutes…');
        const batchStart = performance.now();
        const result = await searchBatch(miner);
        const elapsed = performance.now() - batchStart;
        const firstBatch = candidates === 0;
        candidates += result.candidates;
        const now = performance.now();
        const rest = controls.record(result.candidates, elapsed, now, call.intensity());
        // A driver may compile on first submission. Do not turn that startup wait into a long throttle delay.
        if (firstBatch) controls.reset(now);
        if (isPendingReward(result) && (await snapshot()).context === result.context) {
          call.save(journal, JSON.stringify(result));
          await settle(result);
          controls.reset(performance.now());
        }
        failures = 0;
        if (!firstBatch && rest > 0 && !call.stopped()) await wait(Math.min(rest, 1000));
      } catch (error) {
        if (error instanceof MiningError) throw error;
        // Bounded retries preserve the journal and surface persistent failures.
        if (++failures > 3) throw error;
        call.log(String(error)); call.status('Refreshing connection…');
        refresh?.close(); session?.close(); await refresh?.drain();
        await wait(2000); await connect(); await snapshot(); controls.reset(performance.now());
      }
    }
    call.status('Stopped');
  } finally {
    clearInterval(renderTimer);
    render(true);
    refresh?.close(); session?.close(); await refresh?.drain();
    miner?.free(); controls.free();
  }
}
