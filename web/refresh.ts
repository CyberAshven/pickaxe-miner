// #### PR #22: overlap server reads with GPU work, but deliver updates only
// between searches. Keep the one-second freshness boundary and fetch anew
// after a winner; never mutate the borrowed WASM miner from a promise callback.
type Outcome<T> = { ok: true; value: T; completedAt: number } | { ok: false; error: unknown };

export class SnapshotRefresh<T> {
  private pending: Promise<void> | undefined;
  private outcome: Outcome<T> | undefined;
  private startedAt = -Infinity;
  private deliveredAt = -Infinity;
  private closed = false;

  constructor(private fetch: () => Promise<T>, private now: () => number, private interval = 1000) {}

  private start() {
    if (this.closed) throw new Error('Snapshot reader is closed');
    this.startedAt = this.now();
    this.pending = Promise.resolve().then(this.fetch).then(value => {
      if (!this.closed) this.outcome = { ok: true, value, completedAt: this.now() };
    }, error => {
      // Handle background failures immediately; surface them at the next batch.
      if (!this.closed) this.outcome = { ok: false, error };
    });
  }

  private take(): T {
    const outcome = this.outcome;
    this.pending = undefined;
    this.outcome = undefined;
    if (this.closed) throw new Error('Snapshot reader is closed');
    if (!outcome) throw new Error('Snapshot reader has no completed response');
    if (!outcome.ok) throw outcome.error;
    this.deliveredAt = outcome.completedAt;
    return outcome.value;
  }

  async beforeBatch(): Promise<T | undefined> {
    if (this.closed) throw new Error('Snapshot reader is closed');
    if (!this.pending && this.now() - this.startedAt >= this.interval) this.start();
    if (this.pending && (this.outcome || this.now() - this.deliveredAt >= this.interval)) {
      await this.pending;
      // A suspended tab or a long shader compile must not make an old reply fresh.
      if (this.outcome?.ok && this.now() - this.outcome.completedAt >= this.interval) return this.fresh();
      return this.take();
    }
    return undefined;
  }

  async fresh(): Promise<T> {
    // Drain/discard any pre-winner read, then begin a new coherent read. At most
    // one request sequence is in flight, including settlement and reconnects.
    await this.pending;
    this.pending = undefined;
    this.outcome = undefined;
    this.start();
    await this.pending;
    return this.take();
  }

  close() { this.closed = true; this.outcome = undefined; }
  async drain() { await this.pending; }
}
