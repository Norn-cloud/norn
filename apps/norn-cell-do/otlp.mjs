// OpenTelemetry export over OTLP/HTTP (JSON encoding) for the Workers hosts: the
// cell (worker.mjs) and Studio. No SDK and no dependency: a Worker isolate needs
// only counters, histograms and finished spans, sent on a timer turn.
//
// OTEL-OFF-1: nothing is recorded or sent unless OTEL_EXPORTER_OTLP_ENDPOINT is
// configured; `telemetry(env)` is then null and every call site is a no-op.
// OTEL-NEVER-FAILS-1: an export never throws into a request and never delays
// its reply; a failed send drops that batch and is logged at most once a minute.
// OTEL-BOUNDED-1: buffers are bounded (series and spans); past the bound new
// data is dropped and counted, never grown.
// OTEL-NO-TENANT-1: callers pass only low-cardinality attributes (route
// templates, status, refusal codes, outcomes). No tenant id, principal, object
// id or payload is ever an attribute; `attrs` refuses values that are not
// short strings, numbers or booleans.
//
// Configuration (standard OpenTelemetry names, read from the Worker's vars):
//   OTEL_EXPORTER_OTLP_ENDPOINT  base URL; /v1/metrics and /v1/traces are appended
//                                (OpenObserve: https://<host>/api/<org>)
//   OTEL_EXPORTER_OTLP_HEADERS   "name=value,name=value", values percent-decoded
//                                (OpenObserve: "Authorization=Basic%20<b64>")
//   OTEL_SERVICE_NAME            overrides the host's default service name
//   OTEL_RESOURCE_ATTRIBUTES     "key=value,..." added to every export
//                                (for example deployment.environment=next)
//   OTEL_TRACES_SAMPLER_ARG      fraction of spans kept, 0..1 (default 1)
//   NORN_OTEL_INTERVAL_MS        export interval, 1 s to 5 min (default 10 s)

/** Milliseconds: request, commit and step latencies. */
export const MS_BUCKETS = [1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000, 120000];
/** Counts: rows per sync, sockets per publish. */
export const COUNT_BUCKETS = [0, 1, 5, 10, 50, 100, 500, 1000, 5000, 10000, 100000];

const MAX_SERIES = 2000;
const MAX_SPANS = 1000;
const MAX_ATTR_TEXT = 128;
const SEND_TIMEOUT_MS = 5000;
const LOG_EVERY_MS = 60_000;

const configs = new WeakMap();

/** "a=b,c=d" as pairs, percent-decoded; malformed entries are skipped. */
function pairs(text) {
  const out = [];
  for (const part of (text ?? "").split(",")) {
    const at = part.indexOf("=");
    if (at <= 0) continue;
    try {
      out.push([decodeURIComponent(part.slice(0, at).trim()), decodeURIComponent(part.slice(at + 1).trim())]);
    } catch {
      // A malformed escape: skip this entry, keep the rest.
    }
  }
  return out;
}

/** The export settings `env` names, or null when export is off (OTEL-OFF-1). */
export function exportConfig(env, service) {
  const endpoint = typeof env?.OTEL_EXPORTER_OTLP_ENDPOINT === "string" ? env.OTEL_EXPORTER_OTLP_ENDPOINT.trim().replace(/\/+$/, "") : "";
  if (endpoint === "") return null;
  let url;
  try {
    url = new URL(endpoint);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") return null;
  const ratio = Number(env.OTEL_TRACES_SAMPLER_ARG ?? "1");
  const interval = Number(env.NORN_OTEL_INTERVAL_MS ?? "10000");
  return {
    endpoint,
    headers: Object.fromEntries(pairs(env.OTEL_EXPORTER_OTLP_HEADERS)),
    service: (env.OTEL_SERVICE_NAME ?? "").trim() || service,
    resource: pairs(env.OTEL_RESOURCE_ATTRIBUTES),
    sampleRatio: Number.isFinite(ratio) ? Math.min(1, Math.max(0, ratio)) : 1,
    intervalMs: Number.isFinite(interval) ? Math.min(300_000, Math.max(1000, interval)) : 10_000,
  };
}

/**
 * The isolate's telemetry for this Worker `env`, or null when export is off.
 * One instance per env object (one per isolate in production), so every
 * Durable Object in the isolate shares one buffer and one timer.
 */
export function telemetry(env, service) {
  if (env === null || typeof env !== "object") return null;
  if (configs.has(env)) return configs.get(env);
  const config = exportConfig(env, service);
  const made = config === null ? null : new Telemetry(config);
  configs.set(env, made);
  return made;
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
  constructor(config, now = Date.now) {
    this.config = config;
    this.now = now;
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

  /** Flush when due; the returned promise never rejects. Pass it to waitUntil. */
  flushIfDue(send = fetch) {
    return this.due() ? this.flush(send) : Promise.resolve();
  }

  resource() {
    const resource = { "service.name": this.config.service, "service.instance.id": this.instance, "telemetry.sdk.name": "norn-otlp", "telemetry.sdk.language": "webjs" };
    for (const [key, value] of this.config.resource) resource[key] = value.slice(0, MAX_ATTR_TEXT);
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
  flush(send = fetch) {
    if (this.sending !== null) return this.sending;
    const { metrics, traces } = this.drain();
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
        if (!response.ok) this.log(`OTLP export ${path} answered ${response.status}`);
      } catch (error) {
        this.log(`OTLP export ${path} failed: ${error?.name ?? "error"}`);
      }
    };
    this.sending = Promise.all([post("/v1/metrics", metrics), post("/v1/traces", traces)]).then(() => {
      this.sending = null;
    });
    return this.sending;
  }

  log(message) {
    const now = this.now();
    if (now - this.lastLog < LOG_EVERY_MS) return;
    this.lastLog = now;
    console.error(message);
  }
}

// Route templates: the path a request named, with every identifier replaced by
// `{}`, so a route is a bounded label (OTEL-NO-TENANT-1). The resource segment
// after /v1/ and a trailing `:verb` are kept when they are plain lowercase
// words; any other segment is an identifier.
const WORD = /^[a-z][a-z_]{0,31}$/;
const SUBRESOURCES = new Set(["history", "lineage", "links", "findings", "reveal", "rows", "plan", "trace", "answer", "definitions", "items"]);

/** `/v1/objects/shop.Sale/42/history` -> `/v1/objects/{}/{}/history`; `/v1/sources/x:sync` -> `/v1/sources/{}:sync`. */
export function routeOf(pathname) {
  const parts = pathname.split("/").slice(1);
  if (parts[0] !== "v1" || parts.length < 2) return "other";
  const out = ["v1"];
  for (let i = 1; i < parts.length && i < 6; i++) {
    const part = parts[i];
    const at = part.indexOf(":");
    const name = at < 0 ? part : part.slice(0, at);
    const verb = at < 0 ? "" : part.slice(at + 1);
    const kept = i === 1 ? (WORD.test(name) ? name : "{}") : SUBRESOURCES.has(name) ? name : "{}";
    out.push(verb === "" ? kept : `${kept}:${WORD.test(verb) ? verb : "{}"}`);
  }
  return `/${out.join("/")}`;
}

/** A refusal's code from a `norn/refusal@2` body, bounded; "other" when it has none. */
export function refusalCode(text) {
  try {
    const code = JSON.parse(text)?.code;
    return typeof code === "string" && /^[a-z][a-z0-9_.]{0,63}$/.test(code) ? code : "other";
  } catch {
    return "other";
  }
}

/** "2xx" .. "5xx". */
export const statusClass = (status) => `${Math.floor(status / 100)}xx`;
