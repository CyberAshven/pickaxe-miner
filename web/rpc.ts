// Browser transport only. Mining rules and transaction construction live in Rust.
export function rpcErrorCode(error: unknown): number | undefined {
  return typeof error === 'object' && error !== null && 'code' in error && typeof error.code === 'number' ? error.code : undefined;
}
type Request = { resolve: (value: unknown) => void; reject: (error: unknown) => void; timer: ReturnType<typeof setTimeout>; raw: boolean };
export class Electrum {
  private socket: WebSocket;
  private next: number;
  readonly pending: Map<number, Request>;
  private buffer: string;
  constructor(socket: WebSocket) {
    this.socket = socket;
    this.next = 1;
    this.pending = new Map();
    this.buffer = '';
    socket.onmessage = event => {
      try {
        if (typeof event.data !== 'string') throw new Error('Expected a text response');
        this.buffer += event.data;
        if (this.buffer.length > 4 * 1024 * 1024) throw new Error('Server response too large');
        const lines = this.buffer.split('\n');
        this.buffer = lines.pop() ?? '';
        for (const line of lines) if (line.trim()) this.receive(JSON.parse(line), line);
        if (this.buffer.trim()) {
          let value;
          try { value = JSON.parse(this.buffer); } catch { return; }
          const raw = this.buffer;
          this.buffer = '';
          this.receive(value, raw);
        }
      } catch (error) { this.close(error); }
    };
    socket.onclose = () => this.close(new Error('Connection closed'));
    socket.onerror = () => this.close(new Error('WebSocket connection failed'));
  }
  receive(value: unknown, raw: string) {
    if (typeof value !== 'object' || value === null || !('id' in value) || typeof value.id !== 'number') return;
    const request = this.pending.get(value.id);
    if (!request) return;
    this.pending.delete(value.id);
    clearTimeout(request.timer);
    if ('error' in value && value.error) {
      const error = value.error;
      if (typeof error !== 'object' || !('message' in error) || typeof error.message !== 'string'
          || !('code' in error) || typeof error.code !== 'number') {
        request.reject(new Error('Malformed server error')); return;
      }
      request.reject(Object.assign(new Error(error.message), { code: error.code }));
    } else if ('result' in value) request.resolve(request.raw ? raw : value.result);
    else request.reject(new Error('Server omitted the response result'));
  }
  async rpcRaw(method: string, params: readonly unknown[] = []): Promise<string> {
    const result = await this.rpc(method, params, true);
    if (typeof result !== 'string') throw new Error('Expected raw RPC text');
    return result;
  }
  rpc(method: string, params: readonly unknown[] = [], raw = false): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (this.socket.readyState !== 1) return reject(new Error('Not connected'));
      const id = this.next++;
      const timer = setTimeout(() => this.close(new Error('Server timed out')), 15000);
      this.pending.set(id, { resolve, reject, timer, raw });
      this.socket.send(JSON.stringify({ id, method, params }) + '\n');
    });
  }
  close(error: unknown = new Error('Connection closed')) {
    for (const request of this.pending.values()) { clearTimeout(request.timer); request.reject(error); }
    this.pending.clear();
    this.socket.onclose = null;
    this.socket.close();
  }
  static async connect(endpoint: string): Promise<Electrum> {
    const url = new URL(endpoint);
    if (url.protocol !== 'wss:' && !(url.protocol === 'ws:' && ['localhost', '127.0.0.1'].includes(url.hostname))) {
      throw new Error('Use a secure wss:// server, or ws://localhost for a local server');
    }
    const socket = new WebSocket(url);
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => { socket.close(); reject(new Error('Connection timed out')); }, 15000);
      socket.onopen = () => { clearTimeout(timer); resolve(); };
      socket.onerror = () => { clearTimeout(timer); socket.close(); reject(new Error('Connection failed')); };
    });
    const session = new Electrum(socket);
    try { await session.rpc('server.version', ['pickaxe-browser', '1.4.1']); }
    catch (error) { session.close(); throw error; }
    return session;
  }
}
