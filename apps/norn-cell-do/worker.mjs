// One Norn tenant cell per Durable Object (#728, #819). The router only picks
// the object; the Rust cell admits scope, resolves the principal and decides
// every read. Nothing here inspects data or grants access.
import { initSync, NornCell } from "./pkg/norn_cell_do.js";
import module from "./pkg/norn_cell_do_bg.wasm";
initSync({ module });

// Published cells, as [orgId, environmentId].
const PUBLISHED = [["org_demo", "dev"]];
// CellScope::key: `%` and `/` escaped in each part.
const escape = (part) => part.replaceAll("%", "%25").replaceAll("/", "%2F");
const key = ([org, env]) => `${escape(org)}/${escape(env)}`;
const CELL_HEADER = "x-norn-cell";

export class Cell {
  constructor(ctx) {
    this.ctx = ctx;
    this.cell = null;
  }
  fetch(request) {
    // Provisioning and dispatch are synchronous Wasm: no request interleaves.
    if (this.cell === null) {
      const [org, env] = JSON.parse(request.headers.get(CELL_HEADER));
      this.cell = new NornCell(this.ctx.storage, org, env);
    }
    const url = new URL(request.url);
    const reply = this.cell.fetch(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined);
    return new Response(reply.body, { status: reply.status, headers: { "content-type": "application/json" } });
  }
}

function cors(request, env) {
  const origin = request.headers.get("origin");
  const allowed = (env.ALLOWED_ORIGINS ?? "").split(",").map((o) => o.trim()).filter(Boolean);
  return origin !== null && allowed.includes(origin)
    ? { "access-control-allow-origin": origin, "access-control-allow-headers": "authorization", "access-control-allow-methods": "GET", vary: "origin" }
    : {};
}

export default {
  async fetch(request, env) {
    const headers = cors(request, env);
    if (request.method === "OPTIONS") return new Response(null, { status: 204, headers });
    const url = new URL(request.url);
    const asked = [url.searchParams.get("orgId"), url.searchParams.get("environmentId")];
    // An unpublished or missing scope still reaches a cell, whose scope
    // admission returns the same opaque denial as any other failed read.
    const cell = PUBLISHED.find(([org, e]) => org === asked[0] && e === asked[1]) ?? PUBLISHED[0];
    const forwarded = new Request(request);
    forwarded.headers.set(CELL_HEADER, JSON.stringify(cell));
    const response = await env.CELLS.get(env.CELLS.idFromName(key(cell))).fetch(forwarded);
    const out = new Response(response.body, response);
    for (const [name, value] of Object.entries(headers)) out.headers.set(name, value);
    return out;
  },
};
