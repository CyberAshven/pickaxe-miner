// Overlap server reads with GPU work, but deliver updates only
// between searches. Keep the one-second freshness boundary and fetch anew
// after a winner; never mutate the borrowed WASM miner from a promise callback.
type Outcome<T> = { ok: true; value: T; completedAt: number } | { ok: false; error: unknown };

export class SnapshotRefresh<T> {
  private pending: Promise<void> | undefined;
  private outcome: Outcome<T> | undefined;
  private startedAt = -Infinity;
  private deliveredAt = -Infinity;
  private closed = false;
  private noticed = false;

  // `interval`: the safety read's spacing (a number, or read each time).
  // A server notice brings the next read forward, at most once per `gap`.
  constructor(private fetch: () => Promise<T>, private now: () => number,
              private interval: number | (() => number) = 1000, private gap = 1000) {}

  private period(): number { return typeof this.interval === 'number' ? this.interval : this.interval(); }

  /** The server reported a change (a new block or a covenant spend). */
  notice() { this.noticed = true; }

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
    const since = this.now() - this.startedAt;
    if (!this.pending && (since >= this.period() || (this.noticed && since >= this.gap))) {
      this.noticed = false;
      this.start();
    }
    if (this.pending && (this.outcome || this.now() - this.deliveredAt >= this.period())) {
      await this.pending;
      // A suspended tab or a long shader compile must not make an old reply fresh.
      if (this.outcome?.ok && this.now() - this.outcome.completedAt >= this.period()) return this.fresh();
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
