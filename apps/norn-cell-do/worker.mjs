// One Norn tenant cell per Durable Object (#728, #819). The router only picks
// the object; the Rust cell admits scope, resolves the principal and decides
// every read. Nothing here inspects data or grants access.
import { initSync, NornCell } from "./pkg/norn_cell_do.js";
import module from "./pkg/norn_cell_do_bg.wasm";
initSync({ module });

// The demo cell, as [orgId, environmentId]: where a request that names no well-formed scope lands.
const DEMO = ["org_demo", "dev"];
// A scope part a cell can be named by. Anything else is routed to the demo cell, whose scope
// admission answers the same opaque denial as any other failed read.
const ORG = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const ENVIRONMENT = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
// CellScope::key: `%` and `/` escaped in each part.
const escape = (part) => part.replaceAll("%", "%25").replaceAll("/", "%2F");
const key = ([org, env]) => `${escape(org)}/${escape(env)}`;
const CELL_HEADER = "x-norn-cell";
// A hung JWKS fetch must not hold the object: the cached keys age out instead.
const JWKS_TIMEOUT_MS = 5000;
// Same spacing as the Rust side's JWKS_RETRY_SECS: a failed cold open is not retried sooner.
const OPEN_RETRY_MS = 60_000;
/** The largest submission a cell accepts (an ad-hoc QuestionIR), in bytes. */
const MAX_BODY = 65536;
const PROVISION_PATH = "/v1/cell:provision";
/** The largest provisioning submission (a tenant's artifact set), in bytes; the cell checks the same bound. This is read before the cell authorizes the caller. */
const MAX_PROVISION_BODY = 1048576;
// The environments a tenant cell may exist in: pinned configuration (NORN_TENANT_ENVIRONMENTS,
// comma-separated), never taken from a request. An organization has at most one cell per
// declared environment; a scope naming any other environment never reaches a cell of its own.
const DEFAULT_ENVIRONMENTS = "dev,prod";
const environments = (env) => (env.NORN_TENANT_ENVIRONMENTS ?? DEFAULT_ENVIRONMENTS).split(",").map((e) => e.trim()).filter((e) => ENVIRONMENT.test(e));
const isDemo = ([org, e]) => org === DEMO[0] && (e === "dev" || e === "analytics");
// The last JWKS document this isolate fetched and when. A cold open reuses one younger than
// OPEN_RETRY_MS, so opening many cells (any caller can name a scope) costs the issuer one fetch a
// minute, not one per cell. The cell is told the document's real fetch time, so its age and stale
// bounds count from the fetch, and every refresh fetches anew.
let lastJwks = null;
// D102: engine steps one request may take (frontier, sync batches, the question).
const ENGINE_STEPS = 64;
// A hung engine call must not hold the object.
const ENGINE_TIMEOUT_MS = 120_000;

export class Cell {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.cell = null;
    this.opening = null;
    // The last failed cold open and when it failed (see open()).
    this.openFailure = null;
  }
  /** The pinned JWKS document and when it was fetched. The URL is configuration, never taken from a token. `reuse` accepts this isolate's last document while it is younger than OPEN_RETRY_MS (cold opens only). */
  async jwks(reuse) {
    if (reuse && lastJwks !== null && lastJwks.url === this.env.NORN_HOST_JWKS_URL) {
      // A clock that moved backwards (negative age) never reuses.
      const age = Date.now() - lastJwks.at;
      if (age >= 0 && age < OPEN_RETRY_MS) return lastJwks;
    }
    // Never follow a redirect: anything but 200 from the pinned URL fails.
    const response = await fetch(this.env.NORN_HOST_JWKS_URL, { redirect: "manual", signal: AbortSignal.timeout(JWKS_TIMEOUT_MS) });
    if (!response.ok) throw new Error(`JWKS fetch failed: HTTP ${response.status}`);
    lastJwks = { url: this.env.NORN_HOST_JWKS_URL, text: await response.text(), at: Date.now() };
    return lastJwks;
  }
  open(request) {
    // A failed open is cached for OPEN_RETRY_MS and refused at once with the same
    // error: no JWKS fetch, no provisioning reads and no 5 s hold on the object per
    // request while the issuer is unreachable or the stage is misconfigured.
    if (this.openFailure !== null) {
      // A clock that moved backwards (negative elapsed) expires the entry too, so recovery is never delayed past the interval.
      const elapsed = Date.now() - this.openFailure.at;
      if (elapsed >= 0 && elapsed < OPEN_RETRY_MS) return Promise.reject(this.openFailure.error);
      this.openFailure = null;
    }
    // One opening per object; seeding, provisioning and dispatch are synchronous Wasm. A scope
    // nobody provisioned opens vacant: the cell reads its storage and writes nothing.
    this.opening ??= (async () => {
      const [org, env] = JSON.parse(request.headers.get(CELL_HEADER));
      const issuer = this.env.NORN_HOST_ISSUER;
      const jwks = issuer === undefined ? undefined : await this.jwks(true);
      // NORN_LINEAGE_KEY is the stage secret for keyed lineage handles (D53); the
      // analytics cell refuses to open without one of at least 32 bytes.
      this.cell = new NornCell(this.ctx.storage, org, env, issuer, jwks?.text, this.env.NORN_LINEAGE_KEY, jwks?.at ?? Date.now());
      // #921: the connections (NORN_SOURCES) and the secrets and storage settings they name stay in the cell.
      try {
        this.cell.configureConnect(this.env.NORN_SOURCES ?? "[]", connectVars(this.env));
      } catch (error) {
        console.error(`source connections not configured: ${error}`);
      }
      // D102: questions are answered on the tenant analytics engine when the Worker has one.
      if (this.env.NORN_OLAP_URL !== undefined) this.cell.enableAnalytics();
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
  /**
   * D102: perform one engine call the cell asked for. The engine trusts only
   * this Worker's token; the cell decided the call and checks the answer.
   */
  async engine(call) {
    try {
      const response = await fetch(`${this.env.NORN_OLAP_URL}/v1/${call.op}`, {
        method: "POST",
        headers: { authorization: `Bearer ${this.env.NORN_SECRET_DUCKDB_TOKEN}`, "content-type": "application/json" },
        body: call.body,
        redirect: "manual",
        signal: AbortSignal.timeout(ENGINE_TIMEOUT_MS),
      });
      return { status: response.status, body: await response.text() };
    } catch (error) {
      console.error(`analytics engine call failed: ${error}`);
      return { status: 503, body: JSON.stringify({ code: "analytics.unavailable", detail: "the analytics engine is unreachable" }) };
    }
  }
  /** The engine's file for a streamed call, or its refusal for the cell to map. */
  async stream(call) {
    try {
      const response = await fetch(`${this.env.NORN_OLAP_URL}/v1/${call.op}`, {
        method: "POST",
        headers: { authorization: `Bearer ${this.env.NORN_SECRET_DUCKDB_TOKEN}`, "content-type": "application/json" },
        body: call.body,
        redirect: "manual",
        signal: AbortSignal.timeout(ENGINE_TIMEOUT_MS),
      });
      if (response.status !== 200) return { status: response.status, body: await response.text() };
      const headers = { "content-type": response.headers.get("content-type") ?? "application/octet-stream", "cache-control": "no-store", ...call.stream };
      return new Response(response.body, { status: 200, headers });
    } catch (error) {
      console.error(`analytics engine stream failed: ${error}`);
      return { status: 503, body: JSON.stringify({ code: "analytics.unavailable", detail: "the analytics engine is unreachable" }) };
    }
  }
  /**
   * Run the cell's engine calls until it answers; each re-entry is decided afresh.
   * A call marked `stream` (an export the cell authorized) is answered by
   * streaming the engine's file to the caller with the cell's headers.
   */
  async settle(request, body, reply) {
    const url = new URL(request.url);
    for (let step = 0; reply.engine !== undefined && step < ENGINE_STEPS; step++) {
      const call = JSON.parse(reply.engine);
      if (call.stream !== undefined) {
        const streamed = await this.stream(call);
        if (streamed instanceof Response) return streamed;
        reply = this.cell.resumeEngine(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, Date.now(), body, reply.engine, streamed.status, streamed.body);
        continue;
      }
      const answer = await this.engine(call);
      reply = this.cell.resumeEngine(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, Date.now(), body, reply.engine, answer.status, answer.body);
    }
    if (reply.engine !== undefined) {
      return { status: 503, body: JSON.stringify({ format: "norn/refusal@2", retryable: true, reason: null, code: "analytics.unsettled", category: "host", message: "the analytics engine did not settle", explanation: "retry the request" }) };
    }
    return reply;
  }
  /** Refetch the pinned JWKS and install it. On failure the cached keys keep aging; the cell refuses them past its stale bound. */
  async refreshJwks() {
    try {
      const jwks = await this.jwks(false);
      this.cell.replaceJwks(jwks.text, jwks.at);
      return true;
    } catch (error) {
      console.error(`JWKS refresh failed: ${error}`);
      return false;
    }
  }
  async fetch(request) {
    // A submission (an ad-hoc question, a definition, a provisioning set) is read once, bounded, before the cell sees it.
    const limit = new URL(request.url).pathname === PROVISION_PATH ? MAX_PROVISION_BODY : MAX_BODY;
    const body = request.method === "POST" ? await readBounded(request, limit) : undefined;
    if (body === null) {
      return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "request.too_large", category: "limit", message: "the request body is too large", explanation: `submissions are at most ${limit} bytes` }, { status: 413 });
    }
    await this.open(request);
    // Keys at their maximum age are refetched before serving, whether or not any
    // token names an unknown key id, so a removed key stops verifying.
    if (this.cell.jwksDue(Date.now())) await this.refreshJwks();
    let reply = this.serve(request, body);
    // An unknown key id, at most once per retry interval: refetch the pinned JWKS and retry once.
    if (reply.refreshJwks && (await this.refreshJwks())) reply = this.serve(request, body);
    // A continuation (an upload's object read): fetch exactly what the cell planned, hand it back.
    while (reply.thenToken) {
      const { status, bytes } = await perform(JSON.parse(reply.thenRequest));
      reply = this.cell.resume(reply.thenToken, status, bytes, Date.now());
    }
    if (reply.startSync) this.keep(this.runSync(reply.startSync));
    // SYNC-ONE-1: the due-check and its claim are one synchronous cell call, so two requests never start one connection twice.
    const due = this.cell.syncDue(Date.now());
    if (due) this.keep(this.runSync(due));
    // The operator sees why a cell does not open or serves no action; a caller only ever sees a refusal.
    const policyError = reply.policyError ?? null;
    if (policyError !== this.policyError) {
      if (policyError !== null) console.error(`cell needs an operator: ${policyError}`);
      this.policyError = policyError;
    }
    reply = await this.settle(request, body, reply);
    if (reply instanceof Response) return reply;
    return new Response(reply.body, { status: reply.status, headers: { "content-type": "application/json" } });
  }
  /** Run one claimed sync to its end: every step is the cell's, every fetch its plan. */
  async runSync(id) {
    const started = Date.now();
    let fetchMs = 0;
    let fetches = 0;
    let yields = 0;
    let step = JSON.parse(this.cell.syncStep(id, undefined, undefined, Date.now()));
    while (step.request || step.continue) {
      if (step.continue) {
        // SYNC-YIELD-1: let the requests waiting on this cell run between mapped pages.
        await new Promise((resolve) => setTimeout(resolve, 0));
        yields += 1;
        step = JSON.parse(this.cell.syncStep(id, undefined, undefined, Date.now()));
        continue;
      }
      const sent = Date.now();
      const { status, bytes } = await perform(step.request);
      fetchMs += Date.now() - sent;
      fetches += 1;
      step = JSON.parse(this.cell.syncStep(id, status, bytes, Date.now()));
    }
    // Counts and times only: what a run cost, never what it read or carried.
    const done = step.done ?? {};
    const wallMs = Date.now() - started;
    console.log(JSON.stringify({ sync: id, outcome: done.outcome, rowsRead: done.rowsRead, rowsLanded: done.rowsLanded, rowsQuarantined: done.rowsQuarantined, fetches, yields, wallMs, fetchMs, cellMs: wallMs - fetchMs }));
  }
  /** Keep background work alive past the reply where the runtime offers it. */
  keep(promise) {
    const guarded = promise.catch((error) => console.error(`source sync failed: ${error}`));
    if (typeof this.ctx.waitUntil === "function") this.ctx.waitUntil(guarded);
  }
}

// A source page or an uploaded object, read once and bounded (bytes).
const MAX_SOURCE_BODY = 20 * 1024 * 1024;
// One outbound source or storage request may take this long.
const SOURCE_FETCH_TIMEOUT_MS = 30_000;

/** The server variables the cell's connections and uploads read, by name: secrets and storage settings only. Never logged. */
function connectVars(env) {
  return JSON.stringify(Object.fromEntries(Object.entries(env).filter(([name, value]) => typeof value === "string" && (name.startsWith("NORN_SECRET_") || name.startsWith("NORN_S3_")))));
}

/** Perform exactly the request the cell planned; the status and bytes go back to the cell, unread here. */
async function perform(request) {
  try {
    const response = await fetch(request.url, { method: request.method, headers: request.headers, body: request.body ?? undefined, redirect: "manual", signal: AbortSignal.timeout(SOURCE_FETCH_TIMEOUT_MS) });
    const bytes = await readBoundedBytes(response, MAX_SOURCE_BODY);
    return bytes === null ? { status: 413, bytes: new Uint8Array() } : { status: response.status, bytes };
  } catch {
    // A network failure is a refused read: status 0, no body.
    return { status: 0, bytes: new Uint8Array() };
  }
}

/** A response body as bytes, counted as they arrive; null once it passes `limit` (the stream is cancelled). */
async function readBoundedBytes(response, limit) {
  const reader = response.body?.getReader();
  if (reader === undefined) return new Uint8Array();
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
  return new Uint8Array(await new Blob(chunks).arrayBuffer());
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
    const orgs = url.searchParams.getAll("orgId");
    const asks = url.searchParams.getAll("environmentId");
    // Each well-formed scope in a declared environment has its own cell, named by its key; whether
    // that cell is provisioned is the cell's to say. A missing, repeated, malformed or undeclared
    // scope still reaches a cell (the demo one), whose scope admission returns the same opaque
    // denial as any other failed read.
    const asked = [orgs[0], asks[0]];
    const named = orgs.length === 1 && asks.length === 1 && ORG.test(asked[0]) && ENVIRONMENT.test(asked[1]) && (isDemo(asked) || environments(env).includes(asked[1]));
    const cell = named ? asked : DEMO;
    const forwarded = new Request(request);
    forwarded.headers.set(CELL_HEADER, JSON.stringify(cell));
    const response = await env.CELLS.get(env.CELLS.idFromName(key(cell))).fetch(forwarded);
    const out = new Response(response.body, response);
    for (const [name, value] of Object.entries(headers)) out.headers.set(name, value);
    return out;
  },
};
