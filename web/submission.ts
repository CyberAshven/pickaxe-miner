import { rpcErrorCode } from './rpc.js';
import type { PendingReward } from './platform.js';
interface SubmissionOptions {
  session: { rpc(method: string, params?: readonly unknown[]): Promise<unknown> };
  snapshot(): Promise<{ baton: string }>;
  pending: PendingReward;
  stopped(): boolean;
  wait(ms: number): Promise<void>;
  // Returns after the server's next block notice, a bounded wait, or Stop.
  waitForBlock(): Promise<void>;
  progress(message: string): void;
}
// A server's refusal of a claim whose baton (or lock time) is not confirmed yet,
// e.g. "non-BIP68-final (code 64)". Nothing holds the claim, and the same bytes
// are valid after the next block. A lost reply has no server error code.
export function refusedAsNotFinal(error: unknown): boolean {
  return rpcErrorCode(error) != null && error instanceof Error
    && /non-bip68-final|bad-txns-nonfinal|non-final/i.test(error.message);
}
// Keep an accepted or uncertain transaction pending until its baton moves.
// Job identities and verified transaction bytes are supplied by the Rust core.
export async function resolveSubmission({ session, snapshot, pending, stopped, wait, waitForBlock, progress }: SubmissionOptions): Promise<'pending' | 'accepted' | 'stale'> {
  if (!/^[0-9a-f]{64}$/.test(pending.txid) || !/^[0-9a-f]+$/.test(pending.transaction)
      || !/^[0-9a-f]{64}:\d+$/.test(pending.baton)) {
    throw new Error('Invalid pending reward; preserve the saved transaction for recovery.');
  }
  for (;;) {
    if (stopped()) return 'pending';
    let known = false;
    try {
      const transaction = await session.rpc('blockchain.transaction.get', [pending.txid]);
      if (typeof transaction !== 'string' || transaction.toLowerCase() !== pending.transaction) {
        throw new Error('Server returned different bytes for the pending transaction');
      }
      known = true;
    } catch (error) { if (rpcErrorCode(error) == null) throw error; }
    if ((await snapshot()).baton !== pending.baton) return known ? 'accepted' : 'stale';
    if (known) break;
    try {
      const txid = await session.rpc('blockchain.transaction.broadcast', [pending.transaction]);
      if (txid !== pending.txid) throw new Error('Broadcast returned a different transaction ID');
      break;
    } catch (error) {
      // Not a failed connection: wait for the next block, then check and send the same bytes again.
      if (!refusedAsNotFinal(error)) throw error;
      progress('Reward waits for its baton to confirm; sending it again after the next block…');
      await waitForBlock();
    }
  }
  // Re-check every 2 s for 30 s, as the native miner does for an accepted claim.
  for (let i = 0; i < 15; i++) {
    if (stopped()) return 'pending';
    if ((await snapshot()).baton !== pending.baton) return 'accepted';
    progress('Waiting for the accepted baton update…');
    await wait(2000);
  }
  throw new Error('Server has not indexed the accepted baton yet; try another server.');
}
