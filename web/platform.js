// Check standard capabilities, never browser names or user-agent strings.
export function unsupportedReason(environment = globalThis) {
  if (!environment.isSecureContext) return 'Open Pickaxe over HTTPS or localhost to use your GPU.';
  if (!environment.navigator?.gpu) return 'WebGPU is unavailable in this browser. Check that your browser, operating system and graphics driver support WebGPU and GPU acceleration is enabled.';
  if (!environment.navigator?.locks?.request) return 'This browser cannot safely coordinate mining tabs because Web Locks is unavailable.';
  if (!environment.WebAssembly) return 'This browser does not provide WebAssembly for the shared mining engine.';
  return null;
}

export class MiningError extends Error {}

export async function searchBatch(miner) {
  try {
    return JSON.parse(await miner.search());
  } catch (error) {
    // GPU/verification failures cannot be repaired by switching Fulcrum servers.
    throw new MiningError(`Mining stopped: ${error}`, { cause: error });
  }
}
