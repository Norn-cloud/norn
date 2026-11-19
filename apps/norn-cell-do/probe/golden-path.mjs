#!/usr/bin/env node
// The golden-path synthetic probe: one pass through what a customer does, timed
// step by step, reported as pass/fail over OTLP (otlp.mjs) and on stdout.
//
//   ready    the probe cell answers and lists its definitions
//   load     a tiny dataset (three readings) lands through the load action
//   ask      an ad-hoc question over the readings answers with rows
//   act      a governed action: parked for approval (202) or, where no
//            approval gates it (norn dev), committed (200)
//   approve  a second principal approves the parked call and it commits
//   refuse   a call the cell must refuse (a duplicate create) is refused with a code
//   history  the loaded reading's history is readable
//
// Targets:
//   node golden-path.mjs --dev <project>   a running `norn dev` in <project>
//                                          (its descriptor and token; loopback only)
//   node golden-path.mjs                   NORN_PROBE_URL, NORN_PROBE_ORG,
//                                          NORN_PROBE_ENV, NORN_PROBE_TOKEN and,
//                                          for the approval, NORN_PROBE_APPROVER_TOKEN
// NORN_PROBE_REQUIRE_APPROVAL=1 fails the run unless the governed action parks
// and is approved (set it for hosted probe cells, whose approval is installed).
// NORN_PROBE_TARGET names the target in telemetry (default: local-dev or the URL's host).
// Telemetry: OTEL_EXPORTER_OTLP_ENDPOINT / OTEL_EXPORTER_OTLP_HEADERS as in otlp.mjs;
// unset, the probe only prints. Tokens are read, sent to the target and never printed.
//
// The cell must hold the probe definitions (probe/project/src/probe.ts). Each run
// writes three readings and one confirmation under fresh ids.
import { readFileSync } from "node:fs";
import { isAbsolute, join, relative, resolve } from "node:path";
import { telemetry } from "../otlp.mjs";

const READING = "probe.Reading";
const ADD = "probe.reading.add";
const CONFIRM = "probe.confirmation.add";
const STEP_TIMEOUT_MS = 30_000;
const LOOPBACK = /^http:\/\/(127\.0\.0\.1|\[::1\]|localhost):\d{1,5}$/;

function target(argv, env) {
  const at = argv.indexOf("--dev");
  if (at >= 0) {
    const project = resolve(argv[at + 1] ?? ".");
    const descriptor = JSON.parse(readFileSync(join(project, ".norn/dev.json"), "utf8"));
    if (descriptor.format !== "norn-dev/1" || !LOOPBACK.test(descriptor.url)) throw new Error("not a loopback norn-dev/1 descriptor");
    const tokenPath = resolve(project, descriptor.tokenFile);
    const inside = relative(project, tokenPath);
    if (inside.startsWith("..") || isAbsolute(inside)) throw new Error("the dev token lies outside the project");
    return {
      name: env.NORN_PROBE_TARGET ?? "local-dev",
      url: descriptor.url,
      scope: `orgId=${encodeURIComponent(descriptor.scope.orgId)}&environmentId=${encodeURIComponent(descriptor.scope.environmentId)}`,
      token: readFileSync(tokenPath, "utf8").trim(),
      approver: env.NORN_PROBE_APPROVER_TOKEN,
    };
  }
  for (const name of ["NORN_PROBE_URL", "NORN_PROBE_ORG", "NORN_PROBE_ENV", "NORN_PROBE_TOKEN"]) {
    if (!env[name]) throw new Error(`${name} is required (or pass --dev <project>)`);
  }
  const url = new URL(env.NORN_PROBE_URL);
  return {
    name: env.NORN_PROBE_TARGET ?? url.host,
    url: url.origin + url.pathname.replace(/\/+$/, ""),
    scope: `orgId=${encodeURIComponent(env.NORN_PROBE_ORG)}&environmentId=${encodeURIComponent(env.NORN_PROBE_ENV)}`,
    token: env.NORN_PROBE_TOKEN,
    approver: env.NORN_PROBE_APPROVER_TOKEN,
  };
}

/** One request to the target: status and parsed JSON body (null when not JSON). */
async function call(t, method, path, token, body) {
  const sep = path.includes("?") ? "&" : "?";
  const response = await fetch(`${t.url}${path}${sep}${t.scope}`, {
    method,
    headers: { authorization: `Bearer ${token}`, ...(body === undefined ? {} : { "content-type": "application/json" }), "user-agent": "norn-probe/1" },
    body: body === undefined ? undefined : JSON.stringify(body),
    redirect: "manual",
    signal: AbortSignal.timeout(STEP_TIMEOUT_MS),
  });
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    // Not JSON: the status speaks.
  }
  return { status: response.status, json };
}

class StepFailed extends Error {}
const expect = (ok, detail) => {
  if (!ok) throw new StepFailed(detail);
};
/** A refusal's code and status only: never a body that might carry data. */
const said = (reply) => `${reply.status} ${typeof reply.json?.code === "string" ? reply.json.code : ""}`.trim();

export async function run(argv = process.argv.slice(2), env = process.env) {
  const t = target(argv, env);
  const requireApproval = env.NORN_PROBE_REQUIRE_APPROVAL === "1";
  const otel = telemetry(env, "norn-probe");
  const run = `${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
  const ids = [1, 2, 3].map((n) => `probe-${run}-${n}`);
  const now = new Date();
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
      outcome = error instanceof StepFailed && error.message.startsWith("skip:") ? "skipped" : "fail";
      detail = error instanceof StepFailed ? error.message.replace(/^skip:/, "") : `${error?.name ?? "error"}: ${error?.message ?? ""}`.slice(0, 200);
    }
    const ms = Date.now() - started;
    steps.push({ step: name, outcome, ms, detail });
    otel?.count("norn_probe_steps_total", { target: t.name, step: name, outcome });
    if (outcome !== "skipped") otel?.observe("norn_probe_step_duration_ms", { target: t.name, step: name, outcome }, ms);
    otel?.span(`probe ${name}`, { "norn.probe.target": t.name, "norn.probe.step": name, "norn.outcome": outcome }, started, started + ms, outcome === "fail");
    return outcome === "pass";
  };

  const ready = await step("ready", async () => {
    const reply = await call(t, "GET", "/v1/definitions", t.token);
    expect(reply.status === 200, `definitions answered ${said(reply)}`);
    const names = JSON.stringify(reply.json ?? {});
    expect(names.includes(READING) && names.includes(CONFIRM), "the probe definitions are not installed");
  });
  if (ready) {
    await step("load", async () => {
      const day = now.toISOString().slice(0, 10);
      const rows = [["north", "10.50"], ["south", "4.25"], ["north", "1.00"]];
      for (const [i, [region, amount]] of rows.entries()) {
        const reply = await call(t, "POST", `/v1/actions/${ADD}`, t.token, { input: { id: ids[i], region, amount, occurred_at: `${day}T0${i}:00:00Z` }, idempotencyKey: `load-${ids[i]}` });
        expect(reply.status === 200, `load ${i + 1}/3 answered ${said(reply)}`);
      }
      return "3 rows";
    });
    await step("ask", async () => {
      const listed = await call(t, "GET", "/v1/questions:metrics", t.token);
      expect(listed.status === 200, `metrics answered ${said(listed)}`);
      const metrics = (listed.json?.metrics ?? []).filter((m) => m.objectType === READING);
      const metric = metrics.find((m) => String(m.name).includes("amount")) ?? metrics[0];
      expect(metric !== undefined, "no metric over the readings");
      const start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - 1));
      const end = new Date(start.getTime() + 3 * 86_400_000);
      const question = {
        irVersion: "0.1", name: "probe.ask", objectSet: READING,
        aggregate: [{ metric: metric.name }],
        groupBy: [{ path: { property: "region" } }],
        window: { start: start.toISOString().replace(".000", ""), end: end.toISOString().replace(".000", "") },
        freshness: { kind: "live", maxAgeMs: 60_000 },
        calendar: "iso_utc",
      };
      const answer = await call(t, "POST", "/v1/questions:evaluate", t.token, { question });
      expect(answer.status === 200, `evaluate answered ${said(answer)}`);
      const rows = Object.keys(answer.json?.rows ?? {}).length;
      expect(rows > 0, "the answer has no rows");
      return `${rows} rows`;
    });
    await step("act", async () => {
      const reply = await call(t, "POST", `/v1/actions/${CONFIRM}`, t.token, { input: { id: `confirm-${run}`, reading: ids[0] }, idempotencyKey: `confirm-${run}` });
      if (reply.status === 202) {
        state.decision = reply.json?.decision?.id;
        expect(typeof state.decision === "string", "parked without a decision id");
        return "parked for approval";
      }
      expect(!requireApproval, `the governed action answered ${said(reply)}, not 202 (approval required)`);
      expect(reply.status === 200, `the governed action answered ${said(reply)}`);
      return "committed (no approval gates it here)";
    });
    await step("approve", async () => {
      if (state.decision === undefined) {
        expect(!requireApproval, "nothing parked to approve");
        throw new StepFailed("skip:no approval gates the action on this target");
      }
      expect(typeof t.approver === "string" && t.approver !== "", "NORN_PROBE_APPROVER_TOKEN is required to approve");
      const reply = await call(t, "POST", `/v1/decisions/${encodeURIComponent(state.decision)}:decide`, t.approver, { choice: "approve", idempotencyKey: `approve-${run}` });
      expect(reply.status === 200, `decide answered ${said(reply)}`);
      const confirmed = await call(t, "GET", `/v1/objects/probe.Confirmation/confirm-${run}`, t.token);
      expect(confirmed.status === 200, `the approved call did not commit (${said(confirmed)})`);
      return "approved and committed";
    });
    await step("refuse", async () => {
      const reply = await call(t, "POST", `/v1/actions/${ADD}`, t.token, { input: { id: ids[0], region: "north", amount: "1.00", occurred_at: `${now.toISOString().slice(0, 10)}T00:00:00Z` }, idempotencyKey: `dup-${ids[0]}` });
      expect(reply.status >= 400 && reply.status < 500, `a duplicate create answered ${reply.status}`);
      expect(typeof reply.json?.code === "string", "the refusal carries no code");
      otel?.count("norn_probe_refusals_total", { target: t.name, code: reply.json.code });
      return reply.json.code;
    });
    await step("history", async () => {
      const reply = await call(t, "GET", `/v1/objects/${READING}/${ids[0]}/history`, t.token);
      expect(reply.status === 200, `history answered ${said(reply)}`);
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
      console.error(`probe could not start: ${error?.message ?? error}`);
      process.exit(2);
    },
  );
}
