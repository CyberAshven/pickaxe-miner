import { rpcErrorCode } from './rpc.js';
import type { PendingReward } from './platform.js';
interface SubmissionOptions {
  session: { rpc(method: string, params?: readonly unknown[]): Promise<unknown> };
  snapshot(): Promise<{ baton: string }>;
  pending: PendingReward;
  stopped(): boolean;
  wait(ms: number): Promise<void>;
  progress(message: string): void;
}
// Keep an accepted or uncertain transaction pending until its baton moves.
// Job identities and verified transaction bytes are supplied by the Rust core.
export async function resolveSubmission({ session, snapshot, pending, stopped, wait, progress }: SubmissionOptions): Promise<'pending' | 'accepted' | 'stale'> {
  if (!/^[0-9a-f]{64}$/.test(pending.txid) || !/^[0-9a-f]+$/.test(pending.transaction)
      || !/^[0-9a-f]{64}:\d+$/.test(pending.baton)) {
    throw new Error('Invalid pending reward; preserve the saved transaction for recovery.');
  }
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
  if (!known) {
    const txid = await session.rpc('blockchain.transaction.broadcast', [pending.transaction]);
    if (txid !== pending.txid) throw new Error('Broadcast returned a different transaction ID');
  }
  for (let i = 0; i < 30; i++) {
    if (stopped()) return 'pending';
    if ((await snapshot()).baton !== pending.baton) return 'accepted';
    progress('Waiting for the accepted baton update…');
    await wait(1000);
  }
  throw new Error('Server has not indexed the accepted baton yet; try another server.');
}
