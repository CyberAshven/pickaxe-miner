export interface PendingReward { txid: string; transaction: string; baton: string; context: string }
export interface BatchResult { candidates: number; context: string; baton: string; txid?: string; transaction?: string; tail_j?: number }
export function isPendingReward(value: unknown): value is PendingReward {
  return typeof value === 'object' && value !== null &&
    ['txid', 'transaction', 'baton', 'context'].every(key => key in value && typeof Reflect.get(value, key) === 'string');
}
export function parsePendingReward(raw: string): PendingReward {
  const value: unknown = JSON.parse(raw);
  if (!isPendingReward(value)) throw new Error('Invalid pending reward; preserve the saved transaction for recovery.');
  return value;
}
interface Capabilities {
  isSecureContext?: boolean;
  navigator?: { gpu?: unknown; locks?: { request?: unknown } };
  WebAssembly?: unknown;
}
// Check standard capabilities, never browser names or user-agent strings.
export function unsupportedReason(environment: Capabilities = globalThis) {
  if (!environment.isSecureContext) return 'Open Pickaxe over HTTPS or localhost to use your GPU.';
  if (!environment.navigator?.gpu) return 'WebGPU is unavailable in this browser. Check that your browser, operating system and graphics driver support WebGPU and GPU acceleration is enabled.';
  if (!environment.navigator?.locks?.request) return 'This browser cannot safely coordinate mining tabs because Web Locks is unavailable.';
  if (!environment.WebAssembly) return 'This browser does not provide WebAssembly for the shared mining engine.';
  return null;
}

export class MiningError extends Error {}

export async function searchBatch(miner: { search(): Promise<string> }): Promise<BatchResult> {
  try {
    const value: unknown = JSON.parse(await miner.search());
    if (typeof value !== 'object' || value === null || !('candidates' in value) || typeof value.candidates !== 'number'
        || !Number.isSafeInteger(value.candidates) || value.candidates < 0 || value.candidates > 0xffffffff
        || !('context' in value) || typeof value.context !== 'string' || !('baton' in value) || typeof value.baton !== 'string'
        || ('transaction' in value && !isPendingReward(value))) throw new Error('Invalid mining result');
    const batch: BatchResult = { candidates: value.candidates, context: value.context, baton: value.baton };
    if (isPendingReward(value)) Object.assign(batch, value);
    return batch;
  } catch (error) {
    // GPU/verification failures cannot be repaired by switching Fulcrum servers.
    throw new MiningError(`Mining stopped: ${error}`, { cause: error });
  }
}
