// One Norn tenant cell per Durable Object (#728, #819). The router only picks
// the object; the Rust cell admits scope, resolves the principal and decides
// every read. Nothing here inspects data or grants access.
import { initSync, NornCell } from "./pkg/norn_cell_do.js";
import module from "./pkg/norn_cell_do_bg.wasm";
import * as live from "./subscribe.mjs";
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
/** The largest submission a cell accepts (an ad-hoc QuestionIR), in UTF-16 units. */
const MAX_BODY = 65536;

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
  /** The cell's scope as the router names it, [orgId, environmentId]. */
  static scopeOf(request) {
    return JSON.parse(request.headers.get(CELL_HEADER));
  }
  open(scope) {
    // A failed open is cached for OPEN_RETRY_MS and refused at once with the same
    // error: no JWKS fetch, no provisioning reads and no 5 s hold on the object per
    // request while the issuer is unreachable or the stage is misconfigured.
    if (this.openFailure !== null) {
      // A clock that moved backwards (negative elapsed) expires the entry too, so recovery is never delayed past the interval.
      const elapsed = Date.now() - this.openFailure.at;
      if (elapsed >= 0 && elapsed < OPEN_RETRY_MS) return Promise.reject(this.openFailure.error);
      this.openFailure = null;
    }
    // One opening per object; provisioning and dispatch are synchronous Wasm.
    this.opening ??= (async () => {
      const [org, env] = scope;
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
  serve(request, body) {
    const url = new URL(request.url);
    return this.cell.fetch(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, Date.now(), body);
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
  /** The cell's own GET, as `principal` asks it now: what a live subscription forwards. */
  ask(pathname, query, authorization) {
    return this.cell.fetch("GET", pathname, query, authorization ?? undefined, Date.now(), undefined);
  }
  /** Open the cell for a live socket woken from hibernation, refreshing aged keys first. */
  async ready(scope) {
    await this.open(scope);
    if (this.cell.jwksDue(Date.now())) await this.refreshJwks();
  }
  async webSocketMessage(ws, message) {
    const attachment = ws.deserializeAttachment();
    if (!attachment) return ws.close(1011, "no live state");
    await this.ready(attachment.scope);
    await live.message(ws, message, (path, query, authorization) => this.ask(path, query, authorization));
  }
  async webSocketClose(ws, code) {
    try {
      ws.close(code === 1005 ? 1000 : code, "closed");
    } catch {
      // Already closed.
    }
  }
  /** After a commit, every live subscription on this object is answered again. */
  publish() {
    if (this.ctx.getWebSockets().length === 0) return;
    live.publish(this.ctx, (path, query, authorization) => this.ask(path, query, authorization)).catch((error) => console.error(`live publish failed: ${error}`));
  }
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/v1/subscribe") {
      if (request.headers.get("upgrade")?.toLowerCase() !== "websocket") {
        return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "live.upgrade_required", category: "invalid", message: "live reads are a WebSocket", explanation: `open a WebSocket with the ${live.PROTOCOL} protocol` }, { status: 426 });
      }
      const authorization = request.headers.get("authorization");
      if (authorization === null) {
        return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "authn.required", category: "permission", message: "sign in first", explanation: "a live socket carries the caller's token" }, { status: 401 });
      }
      const scope = Cell.scopeOf(request);
      await this.ready(scope);
      return live.accept(this.ctx, scope, authorization);
    }
    // A submission (an ad-hoc question) is read once, bounded, before the cell sees it.
    const body = request.method === "POST" ? await readBounded(request, MAX_BODY) : undefined;
    if (body === null) {
      return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "request.too_large", category: "limit", message: "the request body is too large", explanation: `submissions are at most ${MAX_BODY} bytes` }, { status: 413 });
    }
    await this.open(Cell.scopeOf(request));
    // Keys at their maximum age are refetched before serving, whether or not any
    // token names an unknown key id, so a removed key stops verifying.
    if (this.cell.jwksDue(Date.now())) await this.refreshJwks();
    let reply = this.serve(request, body);
    // An unknown key id, at most once per retry interval: refetch the pinned JWKS and retry once.
    if (reply.refreshJwks && (await this.refreshJwks())) reply = this.serve(request, body);
    if (request.method !== "GET" && reply.status < 400) this.publish();
    return new Response(reply.body, { status: reply.status, headers: { "content-type": "application/json" } });
  }
}

/** Read a request body as text, counting bytes as they arrive; null once it passes `limit` (the stream is cancelled, never buffered whole). */
async function readBounded(request, limit) {
  const reader = request.body?.getReader();
  if (reader === undefined) return "";
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      return null;
    }
    chunks.push(value);
  }
  return new Blob(chunks).text();
}

function cors(request, env) {
  const origin = request.headers.get("origin");
  const allowed = (env.ALLOWED_ORIGINS ?? "").split(",").map((o) => o.trim()).filter(Boolean);
  return origin !== null && allowed.includes(origin)
    ? { "access-control-allow-origin": origin, "access-control-allow-headers": "authorization, content-type", "access-control-allow-methods": "GET, POST", vary: "origin" }
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
    // A live socket passes through untouched.
    if (response.status === 101) return response;
    const out = new Response(response.body, response);
    for (const [name, value] of Object.entries(headers)) out.headers.set(name, value);
    return out;
  },
};
