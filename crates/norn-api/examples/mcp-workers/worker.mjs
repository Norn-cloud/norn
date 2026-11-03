import { initSync, McpHost } from './pkg/mcp_workers.js';
import module from './pkg/mcp_workers_bg.wasm';
initSync({ module });
export class Cell {
  constructor(ctx, env) { this.host = new McpHost(env.ORG, env.ENVIRONMENT, env.ORIGIN); }
  async fetch(request) {
    const reader = request.body?.getReader();
    const chunks = []; let size = 0;
    if (reader) for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > 1048576) { await reader.cancel(); return new Response(null, { status: 413 }); }
      chunks.push(value);
    }
    const bytes = new Uint8Array(size); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    const reply = JSON.parse(this.host.fetch(JSON.stringify({ method: request.method,
      path: new URL(request.url).pathname, headers: Object.fromEntries(request.headers),
      body: new TextDecoder('utf-8', { fatal: true }).decode(bytes) }),
      crypto.randomUUID(), BigInt(Date.now())));
    const headers = { 'Content-Type': 'application/json' };
    if (reply.session) headers['Mcp-Session-Id'] = reply.session;
    if (reply.status === 405) headers.Allow = 'POST';
    return new Response(reply.status === 202 ? null : reply.body, { status: reply.status, headers });
  }
}
export default { fetch(request, env) { return env.CELLS.get(env.CELLS.idFromName('single-cell')).fetch(request); } };
