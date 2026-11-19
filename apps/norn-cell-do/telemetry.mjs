// The cell host's telemetry: what a request, a commit, a sync, an alarm and a
// live publish cost and how they ended, exported over OTLP (otlp.mjs). It wraps
// the Durable Object's methods from outside, so the host's own code is
// unchanged and nothing is recorded when export is off (OTEL-OFF-1).
//
// Attributes are route templates, methods, status classes, outcomes and refusal
// codes only (OTEL-NO-TENANT-1): never the scope, a principal, an object id or a
// body. The environment id (`dev`, `prod`, `analytics`) is configuration, not
// tenant data, and is kept so stages can be told apart.
import { COUNT_BUCKETS, refusalCode, routeOf, statusClass, telemetry } from "./otlp.mjs";

export const SERVICE = "norn-cell";
const CELL_HEADER = "x-norn-cell";

/** What a request does, from its method and route template. */
export function kindOf(method, route) {
  if (route === "/v1/subscribe") return "subscribe";
  if (method === "GET") return "read";
  if (route === "/v1/actions/{}") return "action";
  if (route === "/v1/decisions/{}:decide") return "decision";
  if (route === "/v1/cell:provision") return "provision";
  if (route === "/v1/cell/definitions:plan") return "plan";
  if (route === "/v1/cell/definitions:apply") return "apply";
  if (route === "/v1/definitions:admit") return "admission";
  if (route === "/v1/definitions:install") return "install";
  if (route.startsWith("/v1/questions")) return "question";
  if (route.endsWith(":sync")) return "sync";
  if (route.endsWith(":load") || route.startsWith("/v1/uploads")) return "load";
  return "write";
}

/** Kinds that read and decide nothing durable: their latency is not a commit's. */
const NOT_COMMITS = new Set(["read", "subscribe", "question", "plan", "admission"]);

/** How a request ended: committed, pending (parked for approval), refused or error. */
export function outcomeOf(status) {
  if (status === 202) return "pending";
  if (status < 400) return "ok";
  if (status < 500) return "refused";
  return "error";
}

/** The environment part of the router's cell header, if it is a plain configured name. */
function environmentOf(request) {
  try {
    const environment = JSON.parse(request.headers.get(CELL_HEADER))?.[1];
    return typeof environment === "string" && /^[a-z][a-z0-9_-]{0,31}$/.test(environment) ? environment : "other";
  } catch {
    return "other";
  }
}

function keep(ctx, promise) {
  if (typeof ctx?.waitUntil === "function") ctx.waitUntil(promise);
}

/** Record one finished request. `response` is null when the handler threw. */
export async function recordRequest(t, request, response, startedMs, endedMs) {
  const url = new URL(request.url);
  const route = routeOf(url.pathname);
  const method = request.method;
  const kind = kindOf(method, route);
  const status = response === null ? 500 : response.status;
  const outcome = response === null ? "error" : outcomeOf(status);
  const environment = environmentOf(request);
  const ms = endedMs - startedMs;
  // A refusal names its code; only a JSON body is read, and only on a 4xx/5xx.
  let code = "";
  if (response !== null && status >= 400 && (response.headers.get("content-type") ?? "").includes("application/json")) {
    code = refusalCode(await response.clone().text());
  }
  const base = { route, method, kind, environment };
  t.count("norn_cell_requests_total", { ...base, status_class: response === null ? "5xx" : statusClass(status), outcome });
  t.observe("norn_cell_request_duration_ms", { ...base, outcome }, ms);
  if (code !== "") t.count("norn_cell_refusals_total", { route, kind, code, status_class: statusClass(status), environment });
  if (!NOT_COMMITS.has(kind)) t.observe("norn_cell_commit_duration_ms", { kind, outcome, environment }, ms);
  if (kind === "action") t.count("norn_cell_actions_total", { outcome, environment });
  if (kind === "decision") t.count("norn_cell_decisions_total", { outcome, environment });
  if (kind === "provision" || kind === "plan" || kind === "apply" || kind === "admission" || kind === "install") {
    t.observe("norn_cell_definitions_stage_ms", { stage: kind, outcome, environment }, ms);
  }
  t.span(`cell ${kind}`, { "http.route": route, "http.request.method": method, "http.response.status_code": status, "norn.kind": kind, "norn.outcome": outcome, "norn.refusal.code": code, "norn.environment": environment }, startedMs, endedMs, outcome === "error");
}

/**
 * A finished source sync, from the counts runSync already logs (never what it
 * read). Its duration is the sync lag: from the sync starting to its rows
 * being committed and readable.
 */
export function syncFinished(env, record) {
  const t = telemetry(env, SERVICE);
  if (t === null) return;
  const outcome = typeof record?.outcome === "string" && /^[a-z_]{1,32}$/.test(record.outcome) ? record.outcome : "other";
  // ok, partial (some rows quarantined) or failed, with the failure's code.
  const code = Array.isArray(record?.error) && typeof record.error[0] === "string" && /^[a-z][a-z0-9_.]{0,63}$/.test(record.error[0]) ? record.error[0] : "";
  t.count("norn_cell_sync_runs_total", { outcome, code });
  t.observe("norn_cell_sync_duration_ms", { outcome }, record?.wallMs);
  t.count("norn_cell_sync_rows_read_total", { outcome }, record?.rowsRead ?? 0);
  t.count("norn_cell_sync_rows_landed_total", { outcome }, record?.rowsLanded ?? 0);
  t.count("norn_cell_sync_rows_quarantined_total", { outcome }, record?.rowsQuarantined ?? 0);
  t.observe("norn_cell_sync_rows_landed", { outcome }, record?.rowsLanded ?? 0, "1", COUNT_BUCKETS);
  const ended = Date.now();
  t.span("cell sync", { "norn.outcome": outcome, "norn.sync.rows_landed": record?.rowsLanded ?? 0, "norn.sync.rows_quarantined": record?.rowsQuarantined ?? 0, "norn.refusal.code": code }, ended - (record?.wallMs ?? 0), ended, outcome === "failed");
}

/**
 * Wrap the cell object's handlers. `fetch` is timed and classified; `publish`
 * counts its fan-out; `alarm`, when the class has one (TIMER-ALARM-1), records
 * how late it fired against the deadline it was armed for (`this.armed`).
 */
export function instrument(Cell) {
  const proto = Cell.prototype;
  const fetch = proto.fetch;
  proto.fetch = async function instrumentedFetch(request) {
    const t = telemetry(this.env, SERVICE);
    if (t === null) return fetch.call(this, request);
    const started = Date.now();
    let response = null;
    try {
      response = await fetch.call(this, request);
      return response;
    } finally {
      try {
        await recordRequest(t, request, response, started, Date.now());
      } catch (error) {
        t.log(`telemetry record failed: ${error?.name ?? "error"}`);
      }
      keep(this.ctx, t.flushIfDue());
    }
  };
  const publish = proto.publish;
  if (typeof publish === "function") {
    proto.publish = function instrumentedPublish(...args) {
      const t = telemetry(this.env, SERVICE);
      if (t !== null) {
        const sockets = this.ctx.getWebSockets().length;
        if (sockets > 0) {
          t.count("norn_cell_live_publishes_total", {});
          t.count("norn_cell_live_messages_total", {}, sockets);
          t.observe("norn_cell_live_fanout", {}, sockets, "1", COUNT_BUCKETS);
        }
      }
      return publish.apply(this, args);
    };
  }
  const alarm = proto.alarm;
  if (typeof alarm === "function") {
    proto.alarm = async function instrumentedAlarm(info) {
      const t = telemetry(this.env, SERVICE);
      if (t === null) return alarm.call(this, info);
      const armed = typeof this.armed === "number" ? this.armed : null;
      const started = Date.now();
      const retry = (info?.retryCount ?? 0) > 0;
      let failed = true;
      try {
        const result = await alarm.call(this, info);
        failed = false;
        return result;
      } finally {
        if (armed !== null) t.observe("norn_cell_alarm_lag_ms", { retry }, Math.max(0, started - armed));
        t.count("norn_cell_alarms_total", { outcome: failed ? "error" : "ok", retry });
        t.span("cell alarm", { "norn.outcome": failed ? "error" : "ok", "norn.alarm.retry": retry, "norn.alarm.lag_ms": armed === null ? -1 : Math.max(0, started - armed) }, started, Date.now(), failed);
        keep(this.ctx, t.flushIfDue());
      }
    };
  }
  const runSync = proto.runSync;
  if (typeof runSync === "function") {
    proto.runSync = async function instrumentedSync(...args) {
      try {
        return await runSync.apply(this, args);
      } finally {
        const t = telemetry(this.env, SERVICE);
        if (t !== null) keep(this.ctx, t.flushIfDue());
      }
    };
  }
  return Cell;
}
