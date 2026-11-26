// One Norn tenant cell per Durable Object (#728, #819). The router only picks
// the object; the Rust cell admits scope, resolves the principal and decides
// every read. Nothing here inspects data or grants access.
import { initSync, NornCell, NornGate } from "./pkg/norn_cell_do.js";
import module from "./pkg/norn_cell_do_bg.wasm";
import * as live from "./subscribe.mjs";
import { Wake } from "./wake.mjs";
import { connectVars, OLAP_SIGNING_KEY } from "./connect_vars.mjs";
import { instrument, noteRefusal, syncFinished } from "./telemetry.mjs";
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
/** HOST-INSTALL-1: a definitions change carries a pack, bounded like a provisioning set. */
const DEFINITIONS_PATHS = new Set(["/v1/cell/definitions:plan", "/v1/cell/definitions:apply"]);
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
// A build with the `fixtures` feature accepts fixture bearer tokens; it serves only a stage named
// here (NORN_STAGE, set by the local smoke), never a deployed one.
const FIXTURE_STAGES = new Set(["smoke", "test"]);
// D102 / MANIFEST-LAWS-1 (S1): engine steps one request may take (frontier, sync batches, the
// question; a landing's land, settles and store). The cell owns the bound, so the engine's residue
// limit and this loop can never disagree.
const ENGINE_STEPS = NornCell.engineSteps();
// A hung engine call must not hold the object.
const ENGINE_TIMEOUT_MS = 120_000;
// TIMER-ALARM-1: the scope this object serves, kept so an alarm (which carries no request) can open its cell.
const SCOPE_KEY = "norn.cell.scope";

export class Cell {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.cell = null;
    this.opening = null;
    // TENANT-ISOLATION-1: the scope's membership gate, which holds no storage (see gate()).
    this.gating = null;
    // The last failed cold open and when it failed (see open()).
    this.openFailure = null;
    // S1: the last failed gate and when it failed, apart from the cell's: a failed open (storage,
    // alarms) never changes what an outsider is answered.
    this.gateFailure = null;
    // TIMER-ALARM-1: the object's alarm follows the cell's deadlines (wake.mjs).
    this.wake = new Wake(ctx.storage);
    // Whether this instance has stored the scope it serves.
    this.scopeKept = false;
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
  /** The cell's scope as the router names it, [orgId, environmentId], or null when absent/malformed. */
  static scopeOf(request) {
    const raw = request.headers.get(CELL_HEADER);
    if (raw === null) return null;
    try {
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) && parsed.length === 2 && typeof parsed[0] === "string" && typeof parsed[1] === "string" ? parsed : null;
    } catch {
      return null;
    }
  }
  /**
   * A failed open (`which` = "openFailure") or gate ("gateFailure") is cached for OPEN_RETRY_MS:
   * the cached error, or null to try again. Each is refused at once with the same error: no JWKS
   * fetch, no provisioning reads and no 5 s hold on the object per request while the issuer is
   * unreachable or the stage is misconfigured.
   */
  failedRecently(which = "openFailure") {
    const failure = this[which];
    if (failure !== null) {
      // A clock that moved backwards (negative elapsed) expires the entry too, so recovery is never delayed past the interval.
      const elapsed = Date.now() - failure.at;
      if (elapsed >= 0 && elapsed < OPEN_RETRY_MS) return failure.error;
      this[which] = null;
    }
    return null;
  }
  /** Refuse a build or stage that may not serve, before anything is opened. */
  checkStage() {
    if (NornCell.fixturesEnabled() && !FIXTURE_STAGES.has(this.env.NORN_STAGE)) {
      throw new Error("this cell build accepts fixture principals and serves only a smoke or test stage (NORN_STAGE)");
    }
  }
  /**
   * TENANT-ISOLATION-1 (S1, S3): the scope's membership gate, or null for a demo scope (seeded by
   * this build, served to anyone). It is built from configuration and the pinned JWKS only: no
   * storage. An outsider is answered by it, so a tenant cell's storage is never opened, read, armed
   * or synced for them, and nothing the cell holds (its saved scope, deadlines, sockets) shapes the
   * answer.
   */
  async gate(scope) {
    if (isDemo(scope)) return null;
    const failed = this.failedRecently("gateFailure");
    if (failed !== null) throw failed;
    this.gating ??= (async () => {
      this.checkStage();
      const [org, env] = scope;
      const issuer = this.env.NORN_HOST_ISSUER;
      const jwks = issuer === undefined ? undefined : await this.jwks(true);
      return new NornGate(org, env, issuer, jwks?.text, this.env.NORN_LINEAGE_KEY, this.env.NORN_SOURCES ?? "[]", connectVars(this.env), jwks?.at ?? Date.now());
    })().catch((error) => {
      this.gating = null;
      this.gateFailure = { error, at: Date.now() };
      console.error(`cell gate refused: ${error}`);
      throw error;
    });
    const gate = await this.gating;
    if (gate.jwksDue(Date.now())) await this.refreshJwks(gate);
    return gate;
  }
  /** The gate's reply to an outsider at `scope`, as the Worker serves it; null for a caller the cell serves. */
  async outside(scope, request, body) {
    const gate = await this.gate(scope);
    if (gate === null) return null;
    const url = new URL(request.url);
    const check = () => gate.check(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, Date.now(), body);
    let reply = check();
    // An unknown key id, at most once per retry interval: refetch the pinned JWKS and decide again.
    if (reply?.refreshJwks && (await this.refreshJwks(gate))) reply = check();
    if (reply === undefined || reply === null) return null;
    return noteRefusal(this.env, new Response(reply.body, { status: reply.status, headers: { "content-type": "application/json" } }), reply.body);
  }
  /** An outsider's live read: the gate's answer, never the cell's (an admitted token closes the socket, to reopen as a member). */
  askOutside(gate, pathname, query, authorization) {
    return gate.check("GET", pathname, query, authorization ?? undefined, Date.now(), undefined) ?? { status: 401, body: "" };
  }
  open(scope) {
    const failed = this.failedRecently();
    if (failed !== null) return Promise.reject(failed);
    // One opening per object; seeding, provisioning and dispatch are synchronous Wasm. A scope
    // nobody provisioned opens vacant: the cell reads its storage and writes nothing.
    this.opening ??= (async () => {
      this.checkStage();
      const [org, env] = scope;
      const issuer = this.env.NORN_HOST_ISSUER;
      const jwks = issuer === undefined ? undefined : await this.jwks(true);
      let cell = null;
      try {
        // NORN_LINEAGE_KEY is the stage secret for keyed lineage handles (D53); the
        // analytics cell refuses to open without one of at least 32 bytes.
        cell = new NornCell(
          this.ctx.storage,
          org,
          env,
          issuer,
          jwks?.text,
          { lineage: this.env.NORN_LINEAGE_KEY, checkpoint: this.env.NORN_CHECKPOINT_KEY },
          jwks?.at ?? Date.now(),
        );
        // #921: the connections (NORN_SOURCES) and the secrets and storage settings they name stay in the cell.
        try {
          cell.configureConnect(this.env.NORN_SOURCES ?? "[]", connectVars(this.env));
        } catch (error) {
          console.error(`source connections not configured: ${error}`);
        }
        // D102: questions are answered on the tenant analytics engine when the Worker has one.
        // OLAP-CRED-1 (#1493): the cell signs each engine call for its own tenant; no shared token.
        // OLAP-01: the signing key goes to the analytics path only; its name is outside the
        // connection secret namespace, so connectVars never hands it to a connector.
        if (this.env.NORN_OLAP_URL !== undefined) {
          try {
            cell.enableAnalytics(this.env[OLAP_SIGNING_KEY], this.env.NORN_OLAP_ENGINE);
          } catch (error) {
            console.error(`analytics not enabled: ${error}`);
          }
        }
        // TIMER-ALARM-1: a serving cell keeps its scope for alarms, and arms for whatever was
        // already due when it opened (a cell from before alarms, a lost alarm).
        await this.keepScope(scope, cell);
        await this.wake.opened(cell, Date.now());
        this.cell = cell;
      } catch (error) {
        cell?.free?.();
        this.cell = null;
        throw error;
      }
    })().catch((error) => {
      this.opening = null;
      this.openFailure = { error, at: Date.now() };
      console.error(`cell open refused: ${error}`);
      throw error;
    });
    return this.opening;
  }
  /** TIMER-ALARM-1: store the scope only for a serving cell, never for an open of a vacant scope. */
  async keepScope(scope, cell = this.cell) {
    if (this.scopeKept || !cell?.serving()) return;
    if (JSON.stringify(await this.ctx.storage.get(SCOPE_KEY)) !== JSON.stringify(scope)) await this.ctx.storage.put(SCOPE_KEY, scope);
    this.scopeKept = true;
  }
  serve(request, body) {
    const url = new URL(request.url);
    return this.cell.fetch(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, Date.now(), body);
  }
  /**
   * D102: perform one engine call the cell asked for, with the credential the
   * cell minted for it; the cell decided the call and checks the answer.
   */
  async engine(call) {
    try {
      const base = (this.env.NORN_OLAP_URL ?? "").replace(/\/+$/, "");
      const response = await fetch(`${base}/v1/${call.op}`, {
        method: "POST",
        headers: { authorization: `Bearer ${call.credential}`, "content-type": "application/json" },
        body: call.body,
        redirect: "manual",
        signal: AbortSignal.timeout(ENGINE_TIMEOUT_MS),
      });
      const text = await response.text();
      // The cell may answer its caller opaquely; the operator sees why.
      if (response.status !== 200) console.error(`analytics engine ${call.op} answered ${response.status}: ${text.slice(0, 500)}`);
      return { status: response.status, body: text };
    } catch (error) {
      console.error(`analytics engine call failed: ${error}`);
      return { status: 503, body: JSON.stringify({ code: "analytics.unavailable", detail: "the analytics engine is unreachable" }) };
    }
  }
  /** The engine's file for a streamed call, or its refusal for the cell to map. */
  async stream(call) {
    try {
      const base = (this.env.NORN_OLAP_URL ?? "").replace(/\/+$/, "");
      const response = await fetch(`${base}/v1/${call.op}`, {
        method: "POST",
        headers: { authorization: `Bearer ${call.credential}`, "content-type": "application/json" },
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
  /** Refetch the pinned JWKS and install it in `into` (the cell by default). On failure the cached keys keep aging; the cell refuses them past its stale bound. */
  async refreshJwks(into = this.cell) {
    try {
      const jwks = await this.jwks(false);
      into.replaceJwks(jwks.text, jwks.at);
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
    const role = live.roleOf(attachment);
    if (role === live.OUTSIDE) {
      // TENANT-ISOLATION-1: an outsider's socket is answered by the gate; the cell is never opened for it.
      const gate = await this.gate(attachment.scope);
      if (gate === null) return ws.close(1011, "no live state");
      return live.message(ws, message, (path, query, authorization) => this.askOutside(gate, path, query, authorization));
    }
    // S3: a socket no membership decision classified (accepted before the gate existed) is never
    // taken for a member: it is closed, and the client reconnects through the gate.
    if (role !== live.MEMBER) return live.reclassify(ws);
    // A member's socket woken from hibernation is a member again before storage is reopened for it.
    if (!(await this.member(attachment.scope, attachment.authorization))) return ws.close(4401, "not a member");
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
  /** Whether `authorization` reaches the cell at `scope` (S3): the gate admits it, or the scope is a demo one. */
  async member(scope, authorization) {
    const gate = await this.gate(scope);
    if (gate === null) return true;
    const [org, env] = scope;
    const query = `orgId=${encodeURIComponent(org)}&environmentId=${encodeURIComponent(env)}`;
    const check = () => gate.check("GET", "/v1/subscribe", query, authorization ?? undefined, Date.now(), undefined);
    let reply = check();
    if (reply?.refreshJwks && (await this.refreshJwks(gate))) reply = check();
    return reply == null;
  }
  /**
   * After a commit, every live subscription on this object is answered again; resolves to the
   * changed snapshots sent. S3: a commit is observable only on the sockets of callers who are
   * members now. A socket accepted as a member is re-checked at the gate first, so a former
   * member (an expired token, a lost membership) is sent nothing; an outsider's answer is the
   * gate's, which no commit changes; an unclassified socket is not touched. No socket is closed
   * here: each ends on its own message (`webSocketMessage`), whether or not anyone commits.
   */
  async publish() {
    try {
      const members = [];
      for (const ws of live.members(this.ctx)) {
        const { scope, authorization } = ws.deserializeAttachment();
        if (await this.member(scope, authorization)) members.push(ws);
      }
      if (members.length === 0) return 0;
      return await live.publish({ getWebSockets: () => members }, (path, query, authorization) => this.ask(path, query, authorization));
    } catch (error) {
      console.error(`live publish failed: ${error}`);
      return 0;
    }
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
      if (scope === null) {
        return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "scope.missing", category: "invalid", message: "missing cell scope", explanation: "requests must carry a valid scope" }, { status: 400 });
      }
      // S3: membership first. An outsider's socket is counted only among outsiders' and answered
      // by the gate, so the cell's own sockets (and whether it exists) never shape the upgrade.
      const gate = await this.gate(scope);
      let gateReply = null;
      if (gate !== null) {
        const check = () => gate.check("GET", url.pathname, url.search.slice(1), authorization, Date.now(), undefined);
        gateReply = check();
        if (gateReply?.refreshJwks && (await this.refreshJwks(gate))) gateReply = check();
      }
      const outsider = gateReply != null;
      if (outsider) return live.accept(this.ctx, scope, authorization, request.headers.get("sec-websocket-protocol") ?? "", { outside: true });
      await this.ready(scope);
      return live.accept(this.ctx, scope, authorization, request.headers.get("sec-websocket-protocol") ?? "");
    }
    // A submission (an ad-hoc question, a definition, a provisioning set) is read once, bounded, before the cell sees it.
    const limit = url.pathname === PROVISION_PATH || DEFINITIONS_PATHS.has(url.pathname) ? MAX_PROVISION_BODY : MAX_BODY;
    const body = request.method === "POST" ? await readBounded(request, limit) : undefined;
    if (body === null) {
      return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "request.too_large", category: "limit", message: "the request body is too large", explanation: `submissions are at most ${limit} bytes` }, { status: 413 });
    }
    const scope = Cell.scopeOf(request);
    if (scope === null) {
      return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "scope.missing", category: "invalid", message: "missing cell scope", explanation: "requests must carry a valid scope" }, { status: 400 });
    }
    // S1: membership first. An outsider is answered before the cell's storage is opened: no saved
    // scope, deadline, alarm or sync is read or touched for them, at a busy cell or a vacant scope.
    const outside = await this.outside(scope, request, body);
    if (outside !== null) return outside;
    await this.open(scope);
    // Keys at their maximum age are refetched before serving, whether or not any
    // token names an unknown key id, so a removed key stops verifying.
    if (this.cell.jwksDue(Date.now())) await this.refreshJwks();
    // TIMER-ALARM-1: a request that may write is guarded by a durable wake before it runs,
    // and the alarm follows the cell's next deadline after it, whatever its reply.
    if (request.method === "GET" || request.method === "HEAD") return this.respond(request, body);
    await this.wake.beforeWrite(this.cell, Date.now());
    try {
      return await this.respond(request, body);
    } finally {
      await this.wake.afterWrite(this.cell, Date.now());
      // A provisioning request makes the cell serving: keep its scope from then on.
      await this.keepScope(scope);
    }
  }
  /** Serve one request on the opened cell. */
  async respond(request, body) {
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
    // After a commit, every live subscription on this object is answered again.
    if (request.method !== "GET" && request.method !== "HEAD" && reply.status < 400) this.keep(this.publish());
    // A refusal's text is noted for its code's count (telemetry.mjs), so telemetry never reads the reply.
    return noteRefusal(this.env, new Response(reply.body, { status: reply.status, headers: { "content-type": "application/json" } }), reply.body);
  }
  /** Run one claimed sync to its end: every step is the cell's, every fetch its plan. */
  async runSync(id) {
    // TIMER-ALARM-1: a sync writes; it is guarded like a request, and re-arms at its end.
    await this.wake.beforeWrite(this.cell, Date.now());
    try {
      await this.syncRun(id);
    } catch (error) {
      // A failed sync ends as a failed run; the schedule retries it.
      console.error(`source sync failed: ${error}`);
    } finally {
      // A scheduling error propagates.
      await this.wake.afterWrite(this.cell, Date.now());
    }
  }
  async syncRun(id) {
    // The runtime's clock stands still inside a synchronous call, so each step
    // is timed across a timer turn: cellMs is the cell's own work (mapping and
    // the commit's statements); fetchMs is each request, including any wait for
    // the previous step's writes to become durable before the request may leave.
    const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
    const started = Date.now();
    let cellMs = 0;
    let fetchMs = 0;
    let maxCellMs = 0;
    let maxFetchMs = 0;
    let fetches = 0;
    let steps = 0;
    // SYNC-YIELD-2: keep each step near the step budget by pacing its rows from the last step's time per row.
    const budgetMs = stepBudgetMs(this.env);
    let rows = FIRST_STEP_ROWS;
    // The slowest steps, by kind: begin (the first), page (it mapped rows), fetch (it took a response, mapped none), end (it finished).
    const slow = [];
    const timed = async (response) => {
      const before = Date.now();
      const next = JSON.parse(this.cell.syncStep(id, response?.status, response?.bytes, before, rows));
      // SYNC-YIELD-1: the timer turn also lets requests waiting on this cell run between pages.
      await tick();
      const spent = Date.now() - before;
      cellMs += spent;
      maxCellMs = Math.max(maxCellMs, spent);
      steps += 1;
      const kind = steps === 1 ? "begin" : next.done ? "end" : next.mapped > 0 ? "page" : "fetch";
      slow.push({ kind, ms: spent, mapped: next.mapped ?? 0 });
      slow.sort((a, b) => b.ms - a.ms);
      slow.length = Math.min(slow.length, 3);
      if (next.mapped > 0) rows = Math.max(1, Math.min(MAX_STEP_ROWS, Math.floor((budgetMs * next.mapped) / Math.max(1, spent))));
      return next;
    };
    let step = await timed(undefined);
    while (step.request || step.continue) {
      if (step.continue) {
        step = await timed(undefined);
        continue;
      }
      const sent = Date.now();
      const response = await perform(step.request);
      const spent = Date.now() - sent;
      fetchMs += spent;
      maxFetchMs = Math.max(maxFetchMs, spent);
      fetches += 1;
      step = await timed(response);
    }
    // Counts and times only: what a run cost, never what it read or carried.
    const done = step.done ?? {};
    const wallMs = Date.now() - started;
    console.log(JSON.stringify({ sync: id, outcome: done.outcome, rowsRead: done.rowsRead, rowsLanded: done.rowsLanded, rowsQuarantined: done.rowsQuarantined, steps, fetches, wallMs, cellMs, fetchMs, maxCellMs, maxFetchMs, slow }));
    syncFinished(this.env, { ...done, wallMs }, this.ctx);
    // A sync commits as it goes; live subscriptions are answered again once it ends.
    if ((done.rowsLanded ?? 0) > 0) await this.publish();
  }
  /**
   * TIMER-ALARM-1: the cell's clock. The wake (wake.mjs) sets a recovery
   * wake, runs everything due now through the cell (reservation expiries,
   * flow timers, decision deadlines), claims a due sync, and sets the alarm
   * to the next deadline, all before any external I/O; then the claimed sync
   * runs. Delivery is at least once: a repeated alarm finds nothing due. A
   * failure throws, so the runtime retries, and the recovery wake stays.
   */
  async alarm() {
    const scope = await this.ctx.storage.get(SCOPE_KEY);
    // An object no serving cell ever opened holds nothing to wake.
    if (scope === undefined) return;
    await this.open(scope);
    if (!this.cell.serving()) {
      // A scope kept by an earlier build for a vacant cell: forget it.
      await this.ctx.storage.delete(SCOPE_KEY);
      return;
    }
    const { committed, failed, sync } = await this.wake.alarm(this.cell, Date.now());
    // The operator sees what stayed due and why; it is retried after its backoff.
    if (failed !== null) console.error(`due work not handled: ${failed}`);
    if (committed) await this.publish();
    if (sync) await this.runSync(sync);
  }
  /** Keep background work alive past the reply where the runtime offers it. */
  keep(promise) {
    const guarded = promise.catch((error) => console.error(`background task failed: ${error}`));
    if (typeof this.ctx.waitUntil === "function") this.ctx.waitUntil(guarded);
  }
}

// OTEL-OFF-1: request, sync, alarm and live publish telemetry, exported only when configured (telemetry.mjs).
instrument(Cell);

// SYNC-YIELD-2: a sync step aims to hold the cell about this long (NORN_SYNC_STEP_MS, 250 ms to 30 s); rows per step follow from the last step's time per row.
const STEP_BUDGET_MS = 1500;
/** The server's step budget: a longer step pays a page commit's fixed cost over more rows, a shorter one keeps the cell responsive. */
function stepBudgetMs(env) {
  const configured = Number(env.NORN_SYNC_STEP_MS);
  return Number.isFinite(configured) && configured > 0 ? Math.min(30_000, Math.max(250, configured)) : STEP_BUDGET_MS;
}
const FIRST_STEP_ROWS = 8;
const MAX_STEP_ROWS = 500;
// A source page or an uploaded object, read once and bounded (bytes).
const MAX_SOURCE_BODY = 20 * 1024 * 1024;
// One outbound source or storage request may take this long.
const SOURCE_FETCH_TIMEOUT_MS = 30_000;

/** Perform exactly the request the cell planned; the status and bytes go back to the cell, unread here. */
async function perform(request) {
  try {
    const method = (request.method ?? "GET").toUpperCase();
    const body = method === "GET" || method === "HEAD" || request.body === "" ? undefined : request.body ?? undefined;
    const response = await fetch(request.url, { method, headers: request.headers, body, redirect: "manual", signal: AbortSignal.timeout(SOURCE_FETCH_TIMEOUT_MS) });
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
  const bytes = await readBoundedBytes(request, limit);
  return bytes === null ? null : new TextDecoder().decode(bytes);
}

function cors(request, env) {
  const origin = request.headers.get("origin");
  const allowed = (env.ALLOWED_ORIGINS ?? "").split(",").map((o) => o.trim()).filter(Boolean);
  if (origin !== null && allowed.includes(origin)) {
    return { "access-control-allow-origin": origin, "access-control-allow-headers": "authorization, content-type", "access-control-allow-methods": "GET, POST", vary: "origin" };
  }
  return allowed.length > 0 ? { vary: "origin" } : {};
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
    // A live socket passes through untouched.
    if (response.status === 101) return response;
    const out = new Response(response.body, response);
    for (const [name, value] of Object.entries(headers)) out.headers.set(name, value);
    return out;
  },
};
