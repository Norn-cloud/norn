// One Norn tenant cell per Durable Object (#728, #819). The router only picks
// the object; the Rust cell admits scope, resolves the principal and decides
// every read. Nothing here inspects data or grants access.
import { initSync, NornCell } from "./pkg/norn_cell_do.js";
import module from "./pkg/norn_cell_do_bg.wasm";
initSync({ module });

// Published cells, as [orgId, environmentId].
const PUBLISHED = [["org_demo", "dev"], ["org_demo", "analytics"]];
// CellScope::key: `%` and `/` escaped in each part.
const escape = (part) => part.replaceAll("%", "%25").replaceAll("/", "%2F");
const key = ([org, env]) => `${escape(org)}/${escape(env)}`;
const CELL_HEADER = "x-norn-cell";
// A hung JWKS fetch must not hold the object: the cached keys age out instead.
const JWKS_TIMEOUT_MS = 5000;
// Same spacing as the Rust side's JWKS_RETRY_SECS: a failed cold open is not retried sooner.
const OPEN_RETRY_MS = 60_000;

export class Cell {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.cell = null;
    this.opening = null;
    // The last failed cold open and when it failed (see open()).
    this.openFailure = null;
  }
  /** The pinned JWKS document. The URL is configuration, never taken from a token. */
  async jwks() {
    // Never follow a redirect: anything but 200 from the pinned URL fails.
    const response = await fetch(this.env.NORN_HOST_JWKS_URL, { redirect: "manual", signal: AbortSignal.timeout(JWKS_TIMEOUT_MS) });
    if (!response.ok) throw new Error(`JWKS fetch failed: HTTP ${response.status}`);
    return response.text();
  }
  open(request) {
    // A failed open is cached for OPEN_RETRY_MS and refused at once with the same
    // error: no JWKS fetch, no provisioning reads and no 5 s hold on the object per
    // request while the issuer is unreachable or the stage is misconfigured.
    if (this.openFailure !== null) {
      if (Date.now() - this.openFailure.at < OPEN_RETRY_MS) return Promise.reject(this.openFailure.error);
      this.openFailure = null;
    }
    // One opening per object; provisioning and dispatch are synchronous Wasm.
    this.opening ??= (async () => {
      const [org, env] = JSON.parse(request.headers.get(CELL_HEADER));
      const issuer = this.env.NORN_HOST_ISSUER;
      const jwks = issuer === undefined ? undefined : await this.jwks();
      // NORN_LINEAGE_KEY is the stage secret for keyed lineage handles (D53); the
      // analytics cell refuses to open without one of at least 32 bytes.
      this.cell = new NornCell(this.ctx.storage, org, env, issuer, jwks, this.env.NORN_LINEAGE_KEY, Date.now());
    })().catch((error) => {
      this.opening = null;
      this.openFailure = { error, at: Date.now() };
      console.error(`cell open refused: ${error}`);
      throw error;
    });
    return this.opening;
  }
  serve(request) {
    const url = new URL(request.url);
    return this.cell.fetch(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, Date.now());
  }
  /** Refetch the pinned JWKS and install it. On failure the cached keys keep aging; the cell refuses them past its stale bound. */
  async refreshJwks() {
    try {
      this.cell.replaceJwks(await this.jwks(), Date.now());
      return true;
    } catch (error) {
      console.error(`JWKS refresh failed: ${error}`);
      return false;
    }
  }
  async fetch(request) {
    await this.open(request);
    // Keys at their maximum age are refetched before serving, whether or not any
    // token names an unknown key id, so a removed key stops verifying.
    if (this.cell.jwksDue(Date.now())) await this.refreshJwks();
    let reply = this.serve(request);
    // An unknown key id, at most once per retry interval: refetch the pinned JWKS and retry once.
    if (reply.refreshJwks && (await this.refreshJwks())) reply = this.serve(request);
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
