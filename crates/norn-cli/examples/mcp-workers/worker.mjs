import { initSync, McpHost } from './pkg/mcp_workers.js';
import module from './pkg/mcp_workers_bg.wasm';
initSync({ module });
export class Cell {
  constructor(ctx, env) { this.host = new McpHost(env.ORG, env.ENVIRONMENT, env.ORIGIN); }
  async fetch(request) {
    const reader = request.body?.getReader();
    const bytes = new Uint8Array(1048576); let size = 0;
    if (reader) for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (size + value.byteLength > bytes.length) { await reader.cancel(); return new Response(null, { status: 413 }); }
      bytes.set(value, size); size += value.byteLength;
    }
    let body;
    try { body = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, size)); }
    catch { return new Response(null, { status: 400 }); }
    const reply = JSON.parse(this.host.fetch(JSON.stringify({ method: request.method,
      path: new URL(request.url).pathname, headers: Object.fromEntries(request.headers),
      body }),
      crypto.randomUUID(), BigInt(Date.now())));
    const headers = { 'Content-Type': 'application/json' };
    if (reply.session) headers['Mcp-Session-Id'] = reply.session;
    if (reply.status === 405) headers.Allow = new URL(request.url).pathname === '/catalog' ? 'GET' : 'POST';
    return new Response(reply.status === 202 ? null : reply.body, { status: reply.status, headers });
  }
}
export default { fetch(request, env) { return env.CELLS.get(env.CELLS.idFromName('single-cell')).fetch(request); } };
