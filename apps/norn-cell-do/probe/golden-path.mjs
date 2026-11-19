#!/usr/bin/env node
// The golden-path synthetic probe: one pass through what a customer does, timed
// step by step, reported as pass/fail over OTLP (otlp.mjs) and on stdout.
//
//   ready    the probe cell answers and holds the probe definitions as probe.ts
//            declares them (each action's id, version, effect and write target),
//            and nothing else beyond the catalog's trusted base, so the probe
//            never writes into a customer cell. Every later action call pins
//            the definitions version read here (`?version=`), so a catalog that
//            changes after this check refuses the write (defs.stale).
//   load     a tiny dataset (three readings) lands through the load action
//   ask      an ad-hoc question grouped by run answers this run's group with
//            the sum of the three readings it wrote (15.75, three members)
//   act      a governed action: parked for approval (202) or, where no
//            approval gates it (norn dev), committed (200)
//   approve  a second principal approves the parked call
//   confirm  the confirmation reads back as this run wrote it (typed id,
//            object type and values), in both cases
//   refuse   a duplicate create is refused with exactly action.write_mode_failed
//   history  the loaded reading's history is readable
//
// Targets:
//   node golden-path.mjs --dev <project>   a running `norn dev` in <project>
//                                          (its descriptor and token; loopback only)
//   node golden-path.mjs                   a hosted probe cell: NORN_PROBE_URL (https, or
//                                          http on loopback), NORN_PROBE_ORG, NORN_PROBE_ENV,
//                                          NORN_PROBE_TOKEN and NORN_PROBE_APPROVER_TOKEN
// A hosted probe cell is the approval path: its approval gates the governed
// action, so the run fails unless the action parks and a second principal
// approves it, and NORN_PROBE_APPROVER_TOKEN is required before any request.
// Under --dev, NORN_PROBE_REQUIRE_APPROVAL=1 asks the same of a local cell.
// Telemetry: OTEL_EXPORTER_OTLP_ENDPOINT / OTEL_EXPORTER_OTLP_HEADERS as in otlp.mjs;
// unset, the probe only prints.
//
// PROBE-SECRETS-1: tokens are read, sent to the target and never printed. The
// probe prints only fixed diagnostic codes and numeric statuses: never an
// exception message (which can carry a header value) and never anything a
// response said (a remote code or text can carry a slug, key or token). It
// sends a token only over HTTPS or to loopback.
// PROBE-LABELS-1: the target is named by a fixed label (TARGETS: local-dev or
// hosted) on stdout, metrics and spans, never by a URL, host or configured name.
// PROBE-BOUNDED-1: a run writes into fixed slots, so a probe cell holds the same
// five objects however many runs it serves: three readings (probe-1..3), one
// confirmation and one sentinel, each upserted with this run's id. A cell has
// no delete or deprovision API yet, so each run's ledger entries, receipts and
// (hosted) one parked decision remain, as the cell's audit trail requires.
//
// The cell must hold the probe definitions (probe/probe.ts; probe/dev.sh installs them).
import { readFileSync } from "node:fs";
import { isAbsolute, join, relative, resolve } from "node:path";
import { isLoopback, telemetry } from "../otlp.mjs";

const READING = "probe.Reading";
const PUT = "probe.reading.put";
const CONFIRMATION = "probe.Confirmation";
const CONFIRM = "probe.confirmation.put";
const SENTINEL = "probe.sentinel.add";
const SENTINEL_TYPE = "probe.Sentinel";
const CONFIRMATION_ID = "probe-confirmation";
const SENTINEL_ID = "probe-sentinel";
const SLOTS = ["probe-1", "probe-2", "probe-3"];
/** The version probe.ts gives every probe definition. */
const PROBE_VERSION = "0.2.0";
/** What each probe action must be (probe.ts): its write target, and the write mode the refuse step relies on. */
const ACTIONS = {
  [PUT]: { target: READING, writeMode: "upsert" },
  [CONFIRM]: { target: CONFIRMATION, writeMode: "upsert" },
  [SENTINEL]: { target: SENTINEL_TYPE, writeMode: "create_only" },
};
const ENTITIES = [READING, CONFIRMATION, SENTINEL_TYPE];
/** The three readings' amounts and their sum, as the ask step expects them back. */
const ROWS = [["north", "10.50"], ["south", "4.25"], ["north", "1.00"]];
const EXPECTED_SUM = 15.75;
/** The only names a target has on stdout and in telemetry (PROBE-LABELS-1). */
export const TARGETS = { dev: "local-dev", hosted: "hosted" };
const DUPLICATE = "action.write_mode_failed";
const STEP_TIMEOUT_MS = 30_000;
const LOOPBACK = /^http:\/\/(127\.0\.0\.1|\[::1\]|localhost):\d{1,5}$/;
/** A bearer token: visible ASCII only (RFC 6750 b64token and more), so no header can be split or rejected with its value. */
const TOKEN = /^[\x21-\x7e]{1,8192}$/;

/** A failure the probe reports by a fixed code only. */
export class ProbeError extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

const fail = (code) => {
  throw new ProbeError(code);
};

function token(value, code) {
  if (typeof value !== "string" || !TOKEN.test(value)) fail(code);
  return value;
}

/** The target, with every credential the run needs checked before anything is written. */
export function target(argv, env) {
  const requireApproval = env.NORN_PROBE_REQUIRE_APPROVAL === "1";
  const at = argv.indexOf("--dev");
  let found;
  if (at >= 0) {
    const project = resolve(argv[at + 1] ?? ".");
    let descriptor;
    try {
      descriptor = JSON.parse(readFileSync(join(project, ".norn/dev.json"), "utf8"));
    } catch {
      fail("probe.dev_descriptor_unreadable");
    }
    if (descriptor?.format !== "norn-dev/1" || typeof descriptor.url !== "string" || !LOOPBACK.test(descriptor.url)) fail("probe.dev_descriptor_invalid");
    if (typeof descriptor.tokenFile !== "string") fail("probe.dev_descriptor_invalid");
    const tokenPath = resolve(project, descriptor.tokenFile);
    const inside = relative(project, tokenPath);
    if (inside.startsWith("..") || isAbsolute(inside)) fail("probe.dev_token_outside_project");
    let text;
    try {
      text = readFileSync(tokenPath, "utf8").trim();
    } catch {
      fail("probe.dev_token_unreadable");
    }
    found = {
      name: TARGETS.dev,
      hosted: false,
      url: descriptor.url,
      scope: `orgId=${encodeURIComponent(descriptor.scope?.orgId ?? "")}&environmentId=${encodeURIComponent(descriptor.scope?.environmentId ?? "")}`,
      token: token(text, "probe.token_invalid"),
      approver: env.NORN_PROBE_APPROVER_TOKEN,
    };
  } else {
    for (const name of ["NORN_PROBE_URL", "NORN_PROBE_ORG", "NORN_PROBE_ENV", "NORN_PROBE_TOKEN"]) {
      if (!env[name]) fail(`probe.${name.slice("NORN_PROBE_".length).toLowerCase()}_required`);
    }
    let url;
    try {
      url = new URL(env.NORN_PROBE_URL);
    } catch {
      fail("probe.url_invalid");
    }
    if (url.username !== "" || url.password !== "") fail("probe.url_invalid");
    // PROBE-SECRETS-1: a token travels over HTTPS, or to this machine.
    if (url.protocol !== "https:" && !(url.protocol === "http:" && isLoopback(url.hostname))) fail("probe.url_not_https");
    found = {
      name: TARGETS.hosted,
      hosted: true,
      url: url.origin + url.pathname.replace(/\/+$/, ""),
      scope: `orgId=${encodeURIComponent(env.NORN_PROBE_ORG)}&environmentId=${encodeURIComponent(env.NORN_PROBE_ENV)}`,
      token: token(env.NORN_PROBE_TOKEN, "probe.token_invalid"),
      approver: env.NORN_PROBE_APPROVER_TOKEN,
    };
  }
  // A hosted probe cell is the approval path: the approver's token is checked before any request.
  const approval = requireApproval || found.hosted;
  if (approval || (found.approver !== undefined && found.approver !== "")) {
    found.approver = token(found.approver, approval && !found.approver ? "probe.approver_token_required" : "probe.approver_token_invalid");
  } else {
    found.approver = undefined;
  }
  return { ...found, requireApproval: approval };
}

/** One request to the target: status and parsed JSON body (null when not JSON). Failures are fixed codes. */
async function call(t, method, path, token, body) {
  const sep = path.includes("?") ? "&" : "?";
  let response;
  let text;
  try {
    response = await fetch(`${t.url}${path}${sep}${t.scope}`, {
      method,
      headers: { authorization: `Bearer ${token}`, ...(body === undefined ? {} : { "content-type": "application/json" }), "user-agent": "norn-probe/1" },
      body: body === undefined ? undefined : JSON.stringify(body),
      redirect: "manual",
      signal: AbortSignal.timeout(STEP_TIMEOUT_MS),
    });
    text = await response.text();
  } catch (error) {
    fail(error?.name === "TimeoutError" ? "probe.request_timeout" : "probe.request_failed");
  }
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    // Not JSON: the status speaks.
  }
  return { status: response.status, json };
}

const expect = (ok, code) => {
  if (!ok) fail(code);
};
/** A reply as the probe reports it: its numeric status, never anything the body said (PROBE-SECRETS-1). */
const said = (reply) => String(Number.isInteger(reply.status) ? reply.status : 0);
const SKIP = "skip";
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/**
 * The catalog's version when it holds the probe definitions as probe.ts declares
 * them and, beyond its trusted base (`baseArtifactIds`), nothing else: a cell
 * dedicated to the probe (PROBE-BOUNDED-1). Fails with a fixed code otherwise.
 */
export function dedicatedVersion(catalog) {
  const artifacts = Array.isArray(catalog?.artifacts) ? catalog.artifacts : null;
  const base = Array.isArray(catalog?.baseArtifactIds) ? catalog.baseArtifactIds : null;
  expect(artifacts !== null && base !== null && base.every((id) => typeof id === "string"), "probe.definitions_unreadable");
  expect(Number.isSafeInteger(catalog.version) && catalog.version >= 0, "probe.definitions_unreadable");
  const trusted = new Set(base);
  const own = artifacts.filter((a) => !trusted.has(a?.artifactId));
  // Every artifact beyond the trusted base is the probe's.
  expect(own.every((a) => a?.namespace === "probe"), "probe.cell_not_dedicated");
  for (const name of ENTITIES) {
    expect(own.some((a) => a?.kind === "entity" && a?.payload?.objectType?.name === name && a?.version === PROBE_VERSION), "probe.definitions_missing");
  }
  for (const [id, wanted] of Object.entries(ACTIONS)) {
    const found = own.filter((a) => a?.kind === "action" && a?.payload?.actionId === id);
    expect(found.length === 1, "probe.definitions_missing");
    const [action] = found;
    expect(action.version === PROBE_VERSION, "probe.definition_version_mismatch");
    const effect = action.payload.effectProgram;
    const writeMode = effect?.writeMode?.kind ?? "upsert";
    expect(isObject(effect) && effect.kind === "upsert_input_object" && effect.objectKind === wanted.target && effect.objectIdField === "id" && writeMode === wanted.writeMode, "probe.definition_effect_mismatch");
    const writes = Array.isArray(action.payload.writes) ? action.payload.writes : [];
    expect(writes.length > 0 && writes.every((w) => w?.entityKind === wanted.target), "probe.definition_target_mismatch");
  }
  return catalog.version;
}

export async function run(argv = process.argv.slice(2), env = process.env) {
  const t = target(argv, env);
  const otel = telemetry(env, "norn-probe");
  const run = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
  const now = new Date();
  const day = now.toISOString().slice(0, 10);
  const steps = [];
  const state = {};
  const runStarted = Date.now();

  const step = async (name, body) => {
    const started = Date.now();
    let outcome = "pass";
    let detail = "";
    try {
      detail = (await body()) ?? "";
    } catch (error) {
      // PROBE-SECRETS-1: a fixed code, never an exception's message.
      const code = error instanceof ProbeError ? error.code : "probe.step_error";
      outcome = code === SKIP ? "skipped" : "fail";
      detail = code === SKIP ? "" : code;
    }
    const ms = Date.now() - started;
    steps.push({ step: name, outcome, ms, detail });
    otel?.count("norn_probe_steps_total", { target: t.name, step: name, outcome });
    if (outcome !== "skipped") otel?.observe("norn_probe_step_duration_ms", { target: t.name, step: name, outcome }, ms);
    otel?.span(`probe ${name}`, { "norn.probe.target": t.name, "norn.probe.step": name, "norn.outcome": outcome }, started, started + ms, outcome === "fail");
    return outcome === "pass";
  };

  const readConfirmation = async () => {
    const confirmed = await call(t, "GET", `/v1/objects/${CONFIRMATION}/${CONFIRMATION_ID}`, t.token);
    expect(confirmed.status === 200, `probe.confirmation_unreadable ${said(confirmed)}`);
    // The typed record: its identity and type, and the values this run wrote.
    const record = confirmed.json;
    expect(isObject(record) && record.id === CONFIRMATION_ID && record.objectType === CONFIRMATION && isObject(record.values), "probe.confirmation_unreadable");
    expect(record.values.run === run && record.values.id === CONFIRMATION_ID && record.values.reading === SLOTS[0], "probe.confirmation_not_this_run");
  };
  /** An action call pinned to the definitions version the ready step verified. */
  const act = (action, token, body) => call(t, "POST", `/v1/actions/${action}?version=${state.version}`, token, body);

  const ready = await step("ready", async () => {
    const reply = await call(t, "GET", "/v1/definitions", t.token);
    expect(reply.status === 200, `probe.definitions_unreadable ${said(reply)}`);
    // PROBE-BOUNDED-1: the probe writes only into a cell dedicated to it, as probe.ts defines it.
    state.version = dedicatedVersion(reply.json);
  });
  if (ready) {
    await step("load", async () => {
      for (const [i, [region, amount]] of ROWS.entries()) {
        const reply = await act(PUT, t.token, { input: { id: SLOTS[i], run, region, amount, occurred_at: `${day}T0${i}:00:00Z` }, idempotencyKey: `load-${run}-${i}` });
        expect(reply.status === 200, `probe.load_refused ${said(reply)}`);
      }
      return "3 rows";
    });
    await step("ask", async () => {
      const listed = await call(t, "GET", "/v1/questions:metrics", t.token);
      expect(listed.status === 200, `probe.metrics_unreadable ${said(listed)}`);
      const metrics = (listed.json?.metrics ?? []).filter((m) => m.objectType === READING);
      const metric = metrics.find((m) => String(m.name).includes("amount")) ?? metrics[0];
      expect(metric !== undefined, "probe.no_metric");
      const start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - 1));
      const end = new Date(start.getTime() + 3 * 86_400_000);
      const question = {
        irVersion: "0.1", name: "probe.ask", objectSet: READING,
        aggregate: [{ metric: metric.name }],
        // Grouped by run: the answer must hold a row that only this run's readings can make.
        groupBy: [{ path: { property: "run" } }],
        window: { start: start.toISOString().replace(".000", ""), end: end.toISOString().replace(".000", "") },
        freshness: { kind: "live", maxAgeMs: 60_000 },
        calendar: "iso_utc",
      };
      const answer = await call(t, "POST", "/v1/questions:evaluate", t.token, { question });
      expect(answer.status === 200, `probe.evaluate_refused ${said(answer)}`);
      const rows = isObject(answer.json?.rows) ? Object.values(answer.json.rows) : [];
      expect(rows.length > 0, "probe.answer_empty");
      // Exactly this run's group: its bucket is the run id, and it holds the three readings this run wrote.
      const mine = rows.filter((row) => isObject(row) && row.bucket === run);
      expect(mine.length === 1, "probe.answer_not_this_run");
      const [row] = mine;
      expect(row.suppressed !== true && isObject(row.members) && Object.keys(row.members).length === ROWS.length, "probe.answer_wrong_members");
      expect(typeof row.value === "number" && Math.abs(row.value - EXPECTED_SUM) < 1e-9, "probe.answer_wrong_value");
      return `${rows.length} rows`;
    });
    await step("act", async () => {
      const reply = await act(CONFIRM, t.token, { input: { id: CONFIRMATION_ID, reading: SLOTS[0], run }, idempotencyKey: `confirm-${run}` });
      if (reply.status === 202) {
        state.decision = reply.json?.decision?.id;
        expect(typeof state.decision === "string", "probe.parked_without_decision");
        return "parked for approval";
      }
      expect(!t.requireApproval, `probe.not_parked ${said(reply)}`);
      expect(reply.status === 200, `probe.act_refused ${said(reply)}`);
      state.committed = true;
      return "committed (no approval gates it here)";
    });
    await step("approve", async () => {
      if (state.decision === undefined) {
        expect(!t.requireApproval, "probe.nothing_parked");
        fail(SKIP);
      }
      expect(t.approver !== undefined, "probe.approver_token_required");
      const reply = await call(t, "POST", `/v1/decisions/${encodeURIComponent(state.decision)}:decide`, t.approver, { choice: "approve", idempotencyKey: `approve-${run}` });
      expect(reply.status === 200, `probe.decide_refused ${said(reply)}`);
      state.committed = true;
      return "approved";
    });
    await step("confirm", async () => {
      // Gated or not, the confirmation must read back as this run wrote it.
      expect(state.committed === true, "probe.nothing_committed");
      await readConfirmation();
    });
    await step("refuse", async () => {
      // The sentinel exists after the first run ever; create it if this is that run.
      const add = (key) => act(SENTINEL, t.token, { input: { id: SENTINEL_ID }, idempotencyKey: `${key}-${run}` });
      let reply = await add("sentinel");
      if (reply.status === 200) reply = await add("sentinel-again");
      expect(reply.status >= 400 && reply.status < 500, `probe.duplicate_accepted ${said(reply)}`);
      // Exactly the expected code; any other is reported by status alone (PROBE-SECRETS-1).
      expect(reply.json?.code === DUPLICATE, `probe.duplicate_wrong_refusal ${said(reply)}`);
      otel?.count("norn_probe_refusals_total", { target: t.name, code: DUPLICATE });
      return DUPLICATE;
    });
    await step("history", async () => {
      const reply = await call(t, "GET", `/v1/objects/${READING}/${SLOTS[0]}/history`, t.token);
      expect(reply.status === 200, `probe.history_unreadable ${said(reply)}`);
    });
  }

  const failed = steps.filter((s) => s.outcome === "fail");
  const outcome = failed.length === 0 ? "pass" : "fail";
  const ms = Date.now() - runStarted;
  otel?.count("norn_probe_runs_total", { target: t.name, outcome });
  otel?.observe("norn_probe_run_duration_ms", { target: t.name, outcome }, ms);
  otel?.span("probe run", { "norn.probe.target": t.name, "norn.outcome": outcome, "norn.probe.failed_step": failed[0]?.step ?? "" }, runStarted, runStarted + ms, outcome === "fail");
  if (otel !== null) await otel.flush();
  const summary = { probe: "golden-path", target: t.name, outcome, ms, steps };
  console.log(JSON.stringify(summary));
  return summary;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  run().then(
    (summary) => process.exit(summary.outcome === "pass" ? 0 : 1),
    (error) => {
      // PROBE-SECRETS-1: a fixed code only.
      console.error(`probe could not start: ${error instanceof ProbeError ? error.code : "probe.start_error"}`);
      process.exit(2);
    },
  );
}
