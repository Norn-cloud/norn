// OpenTelemetry export over OTLP/HTTP (JSON encoding) for the Workers hosts: the
// cell (worker.mjs) and Studio. No SDK and no dependency: a Worker isolate needs
// only counters, histograms and finished spans, sent on a timer turn.
//
// OTEL-OFF-1: nothing is recorded or sent unless OTEL_EXPORTER_OTLP_ENDPOINT is
// configured; `telemetry(env)` is then null and every call site is a no-op.
// OTEL-NEVER-FAILS-1: telemetry never throws into a handler and never delays or
// replaces its result. Bad configuration turns export off; recording, draining
// and sending run in a background task the triggering event hands to
// waitUntil; a failed send drops that batch and is logged at most once a minute.
// OTEL-BOUNDED-1: buffers are bounded (series and spans); past the bound new
// data is dropped and counted, never grown.
// OTEL-NO-TENANT-1: callers pass only low-cardinality attributes taken from
// fixed tables (route templates, allowlisted methods, status classes, refusal
// codes, outcomes). No tenant id, principal, object id, path segment or payload
// is ever an attribute or a span name; `attrs` refuses values that are not
// short strings, numbers or booleans.
// OTEL-NO-TENANT-2: the resource names only fixed things. The service name is
// one of SERVICES; OTEL_RESOURCE_ATTRIBUTES keeps only RESOURCE_KEYS whose value
// is one of ENVIRONMENTS, and drops every other key and value, so no host,
// tenant, slug or label configured at deploy reaches a collector.
// OTEL-TLS-1: credentials (OTEL_EXPORTER_OTLP_HEADERS) travel only over HTTPS,
// or to a loopback collector; a credentialed plain-HTTP remote endpoint, URL
// userinfo or a header that is not a valid HTTP field turns export off.
//
// Configuration (standard OpenTelemetry names, read from the Worker's vars):
//   OTEL_EXPORTER_OTLP_ENDPOINT  base URL; /v1/metrics and /v1/traces are appended
//                                (OpenObserve: https://<host>/api/<org>)
//   OTEL_EXPORTER_OTLP_HEADERS   "name=value,name=value", values percent-decoded
//                                (OpenObserve: "Authorization=Basic%20<b64>")
//   OTEL_SERVICE_NAME            overrides the host's default service name, when
//                                it is one of SERVICES
//   OTEL_RESOURCE_ATTRIBUTES     "key=value,..."; only deployment.environment
//                                (or deployment.environment.name) with a value
//                                in ENVIRONMENTS is exported, for example
//                                deployment.environment=next
//   OTEL_TRACES_SAMPLER_ARG      fraction of spans kept, 0..1 (default 1)
//   NORN_OTEL_INTERVAL_MS        export interval, 1 s to 5 min (default 10 s); a
//                                batch waits at most MAX_WAIT_MS for its flush

/** Milliseconds: request and step latencies. */
export const MS_BUCKETS = [1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, 120000];
/** Counts: rows per sync, sockets per publish. */
export const COUNT_BUCKETS = [0, 1, 5, 10, 50, 100, 500, 1000, 5000, 10000, 100000];

const MAX_SERIES = 2000;
const MAX_SPANS = 1000;
const MAX_ATTR_TEXT = 128;
const SEND_TIMEOUT_MS = 5000;
const LOG_EVERY_MS = 60_000;
/** The longest a buffered observation waits for its flush: inside the time a Worker keeps a waitUntil task. */
const MAX_WAIT_MS = 20_000;

const configs = new WeakMap();

/** The service names a resource may carry (OTEL-NO-TENANT-2). */
export const SERVICES = new Set(["norn-cell", "norn-studio", "norn-probe"]);
/** The stage names an environment attribute or resource value may take (OTEL-NO-TENANT-2). */
export const ENVIRONMENTS = new Set(["dev", "test", "preview", "next", "staging", "prod", "analytics"]);
/** The resource attribute keys OTEL_RESOURCE_ATTRIBUTES may set; their values must be ENVIRONMENTS. */
const RESOURCE_KEYS = new Set(["deployment.environment", "deployment.environment.name"]);

/** "a=b,c=d" as pairs, percent-decoded; null when any entry is malformed. */
function pairs(text) {
  const out = [];
  if (typeof text !== "string") return text === undefined ? out : null;
  for (const part of text.split(",")) {
    if (part.trim() === "") continue;
    const at = part.indexOf("=");
    if (at <= 0) return null;
    try {
      out.push([decodeURIComponent(part.slice(0, at).trim()), decodeURIComponent(part.slice(at + 1).trim())]);
    } catch {
      return null;
    }
  }
  return out;
}

/** An HTTP field name (RFC 9110 token) and a value without control characters or non-Latin-1 text. */
const FIELD_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]{1,64}$/;
const FIELD_VALUE = /^[\x20-\x7e\x80-\xff]{0,4096}$/;

/** Whether `hostname` (as URL parses it) is this machine. */
export function isLoopback(hostname) {
  return hostname === "localhost" || hostname === "[::1]" || /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(hostname);
}

/** A string setting, trimmed; "" when unset or not a string. */
const text = (value) => (typeof value === "string" ? value.trim() : "");

/** The export settings `env` names, or null when export is off or misconfigured (OTEL-OFF-1, OTEL-TLS-1). */
export function exportConfig(env, service) {
  const endpoint = text(env?.OTEL_EXPORTER_OTLP_ENDPOINT).replace(/\/+$/, "");
  if (endpoint === "") return null;
  let url;
  try {
    url = new URL(endpoint);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return null;
  if (url.username !== "" || url.password !== "" || url.search !== "" || url.hash !== "") return null;
  const headerPairs = pairs(env.OTEL_EXPORTER_OTLP_HEADERS);
  if (headerPairs === null || headerPairs.some(([name, value]) => !FIELD_NAME.test(name) || !FIELD_VALUE.test(value))) return null;
  if (headerPairs.length > 0 && url.protocol === "http:" && !isLoopback(url.hostname)) return null;
  // OTEL-NO-TENANT-2: allowlisted keys with allowlisted values; everything else is dropped.
  const resource = (pairs(env.OTEL_RESOURCE_ATTRIBUTES) ?? []).filter(([key, value]) => RESOURCE_KEYS.has(key) && ENVIRONMENTS.has(value));
  const named = text(env.OTEL_SERVICE_NAME);
  const ratio = Number(text(env.OTEL_TRACES_SAMPLER_ARG) || "1");
  const interval = Number(text(env.NORN_OTEL_INTERVAL_MS) || "10000");
  return {
    endpoint,
    headers: Object.fromEntries(headerPairs),
    service: SERVICES.has(named) ? named : SERVICES.has(service) ? service : "norn",
    resource,
    sampleRatio: Number.isFinite(ratio) ? Math.min(1, Math.max(0, ratio)) : 1,
    intervalMs: Number.isFinite(interval) ? Math.min(300_000, Math.max(1000, interval)) : 10_000,
  };
}

/**
 * The isolate's telemetry for this Worker `env`, or null when export is off.
 * One instance per env object (one per isolate in production), so every
 * Durable Object in the isolate shares one buffer and one timer. Never throws.
 */
export function telemetry(env, service) {
  if (env === null || typeof env !== "object") return null;
  try {
    if (configs.has(env)) return configs.get(env);
    const config = exportConfig(env, service);
    const made = config === null ? null : new Telemetry(config);
    configs.set(env, made);
    return made;
  } catch {
    return null;
  }
}

/** Only short strings, finite numbers and booleans survive (OTEL-NO-TENANT-1 is the caller's; this bounds the shape). */
function attrs(input) {
  const out = {};
  for (const [key, value] of Object.entries(input ?? {})) {
    if (typeof value === "string") out[key] = value.slice(0, MAX_ATTR_TEXT);
    else if (typeof value === "boolean" || (typeof value === "number" && Number.isFinite(value))) out[key] = value;
  }
  return out;
}

function anyValue(value) {
  if (typeof value === "string") return { stringValue: value };
  if (typeof value === "boolean") return { boolValue: value };
  return Number.isInteger(value) ? { intValue: String(value) } : { doubleValue: value };
}

const keyValues = (object) => Object.entries(object).map(([key, value]) => ({ key, value: anyValue(value) }));
const nanos = (ms) => `${BigInt(Math.round(ms)) * 1_000_000n}`;
const hex = (bytes) => Array.from(crypto.getRandomValues(new Uint8Array(bytes)), (b) => b.toString(16).padStart(2, "0")).join("");

export class Telemetry {
  constructor(config, now = Date.now, send = (url, init) => fetch(url, init)) {
    this.config = config;
    this.now = now;
    this.send = send;
    this.instance = hex(8);
    this.started = now();
    this.windowStart = this.started;
    this.lastFlush = this.started;
    this.lastLog = 0;
    // name -> { kind, unit, bounds?, series: Map(key -> { attrs, value | counts, sum, count, min, max }) }
    this.metrics = new Map();
    this.seriesCount = 0;
    this.spans = [];
    this.dropped = 0;
    this.sending = null;
    this.waiting = null;
  }

  series(name, kind, unit, input, bounds) {
    let metric = this.metrics.get(name);
    if (metric === undefined) {
      metric = { kind, unit, bounds, series: new Map() };
      this.metrics.set(name, metric);
    }
    const clean = attrs(input);
    const key = JSON.stringify(Object.entries(clean).sort(([a], [b]) => (a < b ? -1 : 1)));
    let point = metric.series.get(key);
    if (point === undefined) {
      if (this.seriesCount >= MAX_SERIES) {
        this.dropped += 1;
        return null;
      }
      point = kind === "sum" ? { attrs: clean, value: 0 } : { attrs: clean, counts: new Array(bounds.length + 1).fill(0), sum: 0, count: 0, min: Infinity, max: -Infinity };
      metric.series.set(key, point);
      this.seriesCount += 1;
    }
    return point;
  }

  /** Add `value` (default 1) to a monotonic counter. */
  count(name, input, value = 1, unit = "1") {
    if (!Number.isFinite(value) || value < 0) return;
    const point = this.series(name, "sum", unit, input);
    if (point !== null) point.value += value;
  }

  /** Record one observation in an explicit-bucket histogram. */
  observe(name, input, value, unit = "ms", bounds = MS_BUCKETS) {
    if (!Number.isFinite(value)) return;
    const point = this.series(name, "histogram", unit, input, bounds);
    if (point === null) return;
    let bucket = bounds.findIndex((bound) => value <= bound);
    if (bucket < 0) bucket = bounds.length;
    point.counts[bucket] += 1;
    point.sum += value;
    point.count += 1;
    point.min = Math.min(point.min, value);
    point.max = Math.max(point.max, value);
  }

  /** Keep one finished span (sampled by OTEL_TRACES_SAMPLER_ARG). `error` marks its status. */
  span(name, input, startMs, endMs, error = false) {
    if (this.config.sampleRatio < 1 && Math.random() >= this.config.sampleRatio) return;
    if (this.spans.length >= MAX_SPANS) {
      this.dropped += 1;
      return;
    }
    this.spans.push({
      traceId: hex(16),
      spanId: hex(8),
      name,
      kind: 2,
      startTimeUnixNano: nanos(startMs),
      endTimeUnixNano: nanos(Math.max(startMs, endMs)),
      attributes: keyValues(attrs(input)),
      status: { code: error ? 2 : 1 },
    });
  }

  /** Whether the export interval has passed since the last flush. */
  due() {
    return this.now() - this.lastFlush >= this.config.intervalMs || this.spans.length >= MAX_SPANS / 2;
  }

  /** Whether anything is buffered. */
  buffered() {
    return this.seriesCount > 0 || this.spans.length > 0 || this.dropped > 0;
  }

  /**
   * The background work for whatever is buffered: a flush now when one is due,
   * otherwise one bounded wait (at most MAX_WAIT_MS, never past the interval)
   * and then a flush, so a quiet isolate still exports its first batch. The
   * triggering event passes the promise to waitUntil; it never rejects.
   */
  pump() {
    try {
      if (this.sending !== null) return this.sending.then(() => this.pump());
      if (this.waiting !== null || !this.buffered()) return this.waiting ?? Promise.resolve();
      if (this.due()) return this.flush();
      const wait = Math.max(0, Math.min(MAX_WAIT_MS, this.config.intervalMs - (this.now() - this.lastFlush)));
      this.waiting = new Promise((resolve) => setTimeout(resolve, wait)).then(() => {
        this.waiting = null;
        return this.flush();
      });
      return this.waiting;
    } catch {
      return Promise.resolve();
    }
  }

  resource() {
    const resource = { "service.name": this.config.service, "service.instance.id": this.instance, "telemetry.sdk.name": "norn-otlp", "telemetry.sdk.language": "webjs" };
    for (const [key, value] of this.config.resource) if (RESOURCE_KEYS.has(key) && ENVIRONMENTS.has(value)) resource[key] = value;
    return { attributes: keyValues(resource) };
  }

  /** The buffered data as OTLP payloads, resetting the buffers (delta temporality). */
  drain() {
    const end = this.now();
    const start = this.windowStart;
    this.windowStart = end;
    this.lastFlush = end;
    const metrics = [];
    for (const [name, metric] of this.metrics) {
      const dataPoints = [];
      for (const point of metric.series.values()) {
        const base = { attributes: keyValues(point.attrs), startTimeUnixNano: nanos(start), timeUnixNano: nanos(end) };
        if (metric.kind === "sum") dataPoints.push({ ...base, asDouble: point.value });
        else if (point.count > 0) dataPoints.push({ ...base, count: String(point.count), sum: point.sum, min: point.min, max: point.max, bucketCounts: point.counts.map(String), explicitBounds: metric.bounds });
      }
      if (dataPoints.length === 0) continue;
      metrics.push(metric.kind === "sum"
        ? { name, unit: metric.unit, sum: { aggregationTemporality: 1, isMonotonic: true, dataPoints } }
        : { name, unit: metric.unit, histogram: { aggregationTemporality: 1, dataPoints } });
    }
    if (this.dropped > 0) {
      metrics.push({ name: "norn_otel_dropped_total", unit: "1", sum: { aggregationTemporality: 1, isMonotonic: true, dataPoints: [{ attributes: [], startTimeUnixNano: nanos(start), timeUnixNano: nanos(end), asDouble: this.dropped }] } });
    }
    const spans = this.spans;
    this.metrics = new Map();
    this.seriesCount = 0;
    this.spans = [];
    this.dropped = 0;
    const scope = { name: "norn", version: "1" };
    const resource = this.resource();
    return {
      metrics: metrics.length === 0 ? null : { resourceMetrics: [{ resource, scopeMetrics: [{ scope, metrics }] }] },
      traces: spans.length === 0 ? null : { resourceSpans: [{ resource, scopeSpans: [{ scope, spans }] }] },
    };
  }

  /** Send everything buffered now. Never rejects (OTEL-NEVER-FAILS-1); one send at a time. */
  flush(send = this.send) {
    if (this.sending !== null) return this.sending;
    let batch;
    try {
      batch = this.drain();
    } catch {
      return Promise.resolve();
    }
    const post = async (path, body) => {
      if (body === null) return;
      try {
        const response = await send(`${this.config.endpoint}${path}`, {
          method: "POST",
          headers: { ...this.config.headers, "content-type": "application/json" },
          body: JSON.stringify(body),
          redirect: "manual",
          signal: AbortSignal.timeout(SEND_TIMEOUT_MS),
        });
        // The body is never read beyond draining it; a refusal names only its status.
        await response.body?.cancel();
        if (!response.ok) this.log(`OTLP export ${path} answered ${Number(response.status) || 0}`);
      } catch (error) {
        this.log(`OTLP export ${path} failed: ${error?.name === "TimeoutError" ? "timeout" : "error"}`);
      }
    };
    this.sending = Promise.all([post("/v1/metrics", batch.metrics), post("/v1/traces", batch.traces)]).then(() => {
      this.sending = null;
    });
    return this.sending;
  }

  log(message) {
    try {
      const now = this.now();
      if (now - this.lastLog < LOG_EVERY_MS) return;
      this.lastLog = now;
      console.error(message);
    } catch {
      // Logging is best effort.
    }
  }
}

// Route templates (OTEL-NO-TENANT-1): a path is counted under the fixed
// template it matches in an explicit table, or under `other`. Nothing from the
// path itself is ever kept: `{type}` stands for any one non-empty segment and
// `{id}:verb` for one ending in that verb; a final `*` for any further path.

/** A matcher over `templates`, tried in order: the first template a path matches, or "other". */
export function routeTable(templates) {
  const compiled = templates.map((template) => ({
    template,
    parts: template.split("/").slice(1).map((segment) => {
      if (segment === "*") return { rest: true };
      const placeholder = /^\{[a-z]+\}(?::([a-z_]+))?$/.exec(segment);
      return placeholder === null ? { literal: segment } : { verb: placeholder[1] ?? null };
    }),
  }));
  const fits = (parts, segments) => {
    for (let i = 0; i < parts.length; i++) {
      const part = parts[i];
      if (part.rest) return segments.length > i && segments[i] !== "";
      const segment = segments[i];
      if (segment === undefined) return false;
      if (part.literal !== undefined) {
        if (segment !== part.literal) return false;
      } else if (part.verb === null ? segment === "" : !(segment.length > part.verb.length + 1 && segment.endsWith(`:${part.verb}`))) {
        return false;
      }
    }
    return segments.length === parts.length;
  };
  return (pathname) => {
    if (typeof pathname !== "string" || !pathname.startsWith("/") || pathname.length > 2048) return "other";
    const segments = pathname.split("/").slice(1);
    for (const { template, parts } of compiled) if (fits(parts, segments)) return template;
    return "other";
  };
}

/** Every route the cell host serves (crates/norn-cell-do/src/server.rs `Route::of`, and worker.mjs). Verb forms precede the plain placeholder they share a position with. */
export const CELL_ROUTES = [
  "/v1/access",
  "/v1/actions",
  "/v1/actions/{action}",
  "/v1/cell:provision",
  "/v1/cell/definitions:plan",
  "/v1/cell/definitions:apply",
  "/v1/definitions",
  "/v1/definitions:admit",
  "/v1/definitions:install",
  "/v1/definitions:list",
  "/v1/definitions/{id}:retire",
  "/v1/definitions/{id}",
  "/v1/objects/{type}:profile",
  "/v1/objects/{type}:facets",
  "/v1/objects/{type}:quarantined",
  "/v1/objects/{type}:export",
  "/v1/objects/{type}",
  "/v1/objects/{type}/{id}",
  "/v1/objects/{type}/{id}/links",
  "/v1/objects/{type}/{id}/lineage",
  "/v1/objects/{type}/{id}/findings",
  "/v1/objects/{type}/{id}/history",
  "/v1/objects/{type}/{id}/reveal",
  "/v1/guard:prompt",
  "/v1/guard/{id}:restore",
  "/v1/questions:evaluate",
  "/v1/questions:trace",
  "/v1/questions:metrics",
  "/v1/questions:catalog",
  "/v1/questions/{question}/answer",
  "/v1/questions/{question}/trace",
  "/v1/sources/{source}:sync",
  "/v1/uploads:begin",
  "/v1/uploads:read",
  "/v1/uploads/{upload}:complete",
  "/v1/uploads/{upload}:load",
  "/v1/uploads/{upload}:define",
  "/v1/observations:load",
  "/v1/datasets:load",
  "/v1/datasets/{type}/rows/{row}:lineage",
  "/v1/mcp",
  "/v1/runs",
  "/v1/runs/{run}",
  "/v1/decisions",
  "/v1/decisions:requested",
  "/v1/decisions/{decision}:decide",
  "/v1/transforms:compile",
  "/v1/transforms/{transform}:refresh",
  "/v1/transforms/{transform}/rows",
  "/v1/transforms/{transform}/rows/{row}/lineage",
  "/v1/transforms/{transform}/plan",
  "/v1/inbox",
  "/v1/subscribe",
];

/** `/v1/objects/shop.Sale/42/history` -> `/v1/objects/{type}/{id}/history`; an unknown path -> `other`. */
export const cellRoute = routeTable(CELL_ROUTES);

const METHODS = new Set(["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]);

/** The request method when it is a standard one, else "other". */
export const methodOf = (method) => (METHODS.has(method) ? method : "other");

/** "1xx" .. "5xx"; "other" for anything outside 100..599. */
export const statusClass = (status) => (Number.isInteger(status) && status >= 100 && status <= 599 ? `${Math.floor(status / 100)}xx` : "other");
