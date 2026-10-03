// Browser transport only. Mining rules and transaction construction live in Rust.
export class Electrum {
  constructor(socket) {
    this.socket = socket;
    this.next = 1;
    this.pending = new Map();
    this.buffer = '';
    socket.onmessage = event => {
      try {
        this.buffer += event.data;
        if (this.buffer.length > 4 * 1024 * 1024) throw new Error('Server response too large');
        const lines = this.buffer.split('\n');
        this.buffer = lines.pop();
        for (const line of lines) if (line.trim()) this.receive(JSON.parse(line));
        if (this.buffer.trim()) {
          let value;
          try { value = JSON.parse(this.buffer); } catch { return; }
          this.buffer = '';
          this.receive(value);
        }
      } catch (error) { this.close(error); }
    };
    socket.onclose = () => this.close(new Error('Connection closed'));
    socket.onerror = () => this.close(new Error('WebSocket connection failed'));
  }
  receive(value) {
    const request = this.pending.get(value.id);
    if (!request) return;
    this.pending.delete(value.id);
    clearTimeout(request.timer);
    if (value.error) request.reject(Object.assign(new Error(value.error.message), { code: value.error.code }));
    else request.resolve(value.result);
  }
  rpc(method, params = []) {
    return new Promise((resolve, reject) => {
      if (this.socket.readyState !== 1) return reject(new Error('Not connected'));
      const id = this.next++;
      const timer = setTimeout(() => this.close(new Error('Server timed out')), 15000);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params }) + '\n');
    });
  }
  close(error = new Error('Connection closed')) {
    for (const request of this.pending.values()) { clearTimeout(request.timer); request.reject(error); }
    this.pending.clear();
    this.socket.onclose = null;
    this.socket.close();
  }
  static async connect(endpoint) {
    const url = new URL(endpoint);
    if (url.protocol !== 'wss:' && !(url.protocol === 'ws:' && ['localhost', '127.0.0.1'].includes(url.hostname))) {
      throw new Error('Use a secure wss:// server, or ws://localhost for a local server');
    }
    const socket = new WebSocket(url);
    await new Promise((resolve, reject) => {
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
