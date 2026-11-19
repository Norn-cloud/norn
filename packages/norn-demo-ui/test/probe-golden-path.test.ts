// The golden-path probe (apps/norn-cell-do/probe/golden-path.mjs) against a fake cell serving a
// real catalog: the host's trusted base (crates/norn-cell-do/src/base.rs, core.Agent.interface
// included) plus probe.ts compiled by `norn compile`, shaped as `GET /v1/definitions` answers a
// provisioned cell. Covers PROBE-SECRETS-1 (tokens and anything a response said never reach
// stdout; tokens travel only over HTTPS or loopback), PROBE-LABELS-1 (fixed target labels),
// PROBE-BOUNDED-1 (every credential, the approver's included on a hosted cell, is checked before
// the first request; a cell holding anything beyond its base and the probe's definitions, or
// definitions other than probe.ts declares, is never written; writes pin the verified version)
// and the golden path's own checks (this run's exact group and sum, the typed confirmation, the
// exact duplicate refusal).

import { afterEach, beforeAll, expect, test } from "bun:test"
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
// @ts-expect-error: the probe is plain JavaScript without declarations.
import { ProbeError, TARGETS, dedicatedVersion, run, target } from "../../../apps/norn-cell-do/probe/golden-path.mjs"

const ROOT = new URL("../../../", import.meta.url).pathname
const TOKEN = "tok-secret-1234"
const APPROVER = "approver-secret-5678"
const HOSTED = { NORN_PROBE_URL: "https://probe.cell.test", NORN_PROBE_ORG: "probe-org", NORN_PROBE_ENV: "probe", NORN_PROBE_TOKEN: TOKEN, NORN_PROBE_APPROVER_TOKEN: APPROVER }
const VERSION = 7

type Artifact = { artifactId: string; kind: string; namespace: string; version: string; payload: any }
type Catalog = { admitted: true; version: number; baseArtifactIds: string[]; artifacts: Artifact[] }

/** The host's trusted base, read from the files base.rs compiles in. */
function trustedBase(): Artifact[] {
  const source = readFileSync(join(ROOT, "crates/norn-cell-do/src/base.rs"), "utf8")
  const files = [...source.matchAll(/include_str!\(\s*"\.\.\/\.\.\/\.\.\/([^"]+)"\s*,?\s*\)/g)].map((m) => m[1]!)
  expect(files.length).toBeGreaterThan(10)
  return files.map((file) => JSON.parse(readFileSync(join(ROOT, file), "utf8")))
}

/** probe.ts compiled by the repository's own compiler. */
function compiledProbe(): Artifact[] {
  const out = join(mkdtempSync(join(tmpdir(), "probe-compile-")), "artifacts.json")
  const child = Bun.spawnSync(["bun", "tools/norn-ts/src/cli.ts", "compile", "apps/norn-cell-do/probe/probe.ts", "--org", "probe-org", "--env", "probe", "--out", out], { cwd: ROOT })
  if (child.exitCode !== 0) throw new Error(`norn compile failed: ${child.stdout.toString()}${child.stderr.toString()}`)
  return JSON.parse(readFileSync(out, "utf8"))
}

let BASE: Artifact[] = []
let PROBE: Artifact[] = []
beforeAll(() => {
  BASE = trustedBase()
  PROBE = compiledProbe()
})

/** A provisioned probe cell's catalog: base first, then the tenant's, as defs_response serves it. */
const catalog = (edit: (probe: Artifact[]) => Artifact[] = (p) => p, base: Artifact[] = BASE): Catalog => ({
  admitted: true,
  version: VERSION,
  baseArtifactIds: base.map((a) => a.artifactId),
  artifacts: [...BASE, ...edit(structuredClone(PROBE))],
})
const action = (probe: Artifact[], id: string) => probe.find((a) => a.kind === "action" && a.payload.actionId === id)!

type Seen = { method: string; path: string; search: URLSearchParams; authorization: string | null; body: any }
type Fault = Partial<{ catalog: Catalog; gated: boolean; staleAnswer: boolean; wrongSum: boolean; staleConfirmation: boolean; untypedConfirmation: boolean; duplicateCode: string; throwWith: string; refuseWith: { status: number; code: string } }>

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
})

/** A probe cell: one in-memory store, the probe's routes, and the faults a test names. */
function fakeCell(fault: Fault = {}) {
  const seen: Seen[] = []
  const objects = new Map<string, any>()
  let parked: any = null
  globalThis.fetch = (async (input: string, init: RequestInit) => {
    const url = new URL(input)
    const body = init.body === undefined ? null : JSON.parse(String(init.body))
    seen.push({ method: init.method ?? "GET", path: url.pathname, search: url.searchParams, authorization: new Headers(init.headers).get("authorization"), body })
    if (fault.throwWith !== undefined) throw new TypeError(fault.throwWith)
    const path = url.pathname
    if (path === "/v1/definitions") return Response.json(fault.catalog ?? catalog())
    if (path.startsWith("/v1/actions/")) {
      // The definitions pin: a call pinned to another version is refused, as actions.rs refuses it.
      if (url.searchParams.has("version") && Number(url.searchParams.get("version")) !== VERSION) return Response.json({ code: "defs.stale" }, { status: 409 })
      if (fault.refuseWith !== undefined) return Response.json({ code: fault.refuseWith.code, message: `refused for ${TOKEN}` }, { status: fault.refuseWith.status })
    }
    if (path === "/v1/actions/probe.reading.put" || path === "/v1/actions/probe.confirmation.put") {
      const type = path.endsWith("reading.put") ? "probe.Reading" : "probe.Confirmation"
      if (type === "probe.Confirmation" && fault.gated) {
        parked = body.input
        return Response.json({ outcome: "pending", decision: { id: "dec-1" } }, { status: 202 })
      }
      objects.set(`${type}/${body.input.id}`, body.input)
      return Response.json({ id: body.input.id })
    }
    if (path === "/v1/decisions/dec-1:decide") {
      objects.set(`probe.Confirmation/${parked.id}`, parked)
      return Response.json({ outcome: "committed" })
    }
    if (path === "/v1/actions/probe.sentinel.add") {
      if (objects.has("probe.Sentinel/probe-sentinel")) return Response.json({ code: fault.duplicateCode ?? "action.write_mode_failed" }, { status: 409 })
      objects.set("probe.Sentinel/probe-sentinel", body.input)
      return Response.json({ id: "probe-sentinel" })
    }
    if (path === "/v1/questions:metrics") return Response.json({ metrics: [{ name: "probe.reading.amount_sum", objectType: "probe.Reading" }] })
    if (path === "/v1/questions:evaluate") {
      // Grouped by run, as NornQueryResult keys rows: canonical JSON of the bucket value.
      const readings = [...objects.values()].filter((o) => o.region !== undefined)
      const runs = fault.staleAnswer ? ["older-run"] : [...new Set(readings.map((o) => o.run))]
      const rows = Object.fromEntries(runs.map((r) => {
        const mine = readings.filter((o) => o.run === r || fault.staleAnswer)
        const sum = mine.reduce((total, o) => total + Number(o.amount), 0) + (fault.wrongSum ? 1 : 0)
        return [JSON.stringify(r), { bucket: r, members: Object.fromEntries(mine.map((o) => [o.id, {}])), totals: [sum], value: sum }]
      }))
      return Response.json({ rows })
    }
    const object = /^\/v1\/objects\/([^/]+)\/([^/]+)(\/history)?$/.exec(path)
    if (object !== null) {
      const found = objects.get(`${object[1]}/${object[2]}`)
      if (found === undefined) return Response.json({ code: "object.not_found" }, { status: 404 })
      if (object[3]) return Response.json({ history: [{}] })
      const values = fault.staleConfirmation && object[1] === "probe.Confirmation" ? { ...found, run: "older-run" } : found
      // The record as NornQueriedRecord carries it; an untyped one carries the run only in some other field.
      if (fault.untypedConfirmation) return Response.json({ note: `run ${found.run}` })
      return Response.json({ id: object[2], objectType: object[1], scope: "probe-org/probe", values, version: 1, withheld: false, withheldProperties: [] })
    }
    return Response.json({ code: "route.not_found" }, { status: 404 })
  }) as unknown as typeof fetch
  return { seen, objects }
}

/** Run the probe with stdout captured. */
async function probe(env: Record<string, string>, argv: string[] = []) {
  const lines: string[] = []
  const original = console.log
  console.log = (line: string) => lines.push(line)
  try {
    const summary = await run(argv, env)
    return { summary, out: lines.join("\n") }
  } finally {
    console.log = original
  }
}

/** A `norn dev` project descriptor on loopback, for --dev runs. */
function devProject(): string[] {
  const project = mkdtempSync(join(tmpdir(), "probe-dev-"))
  mkdirSync(join(project, ".norn"))
  writeFileSync(join(project, ".norn/token"), TOKEN)
  writeFileSync(join(project, ".norn/dev.json"), JSON.stringify({ format: "norn-dev/1", url: "http://127.0.0.1:8787", tokenFile: ".norn/token", scope: { orgId: "org_dev", environmentId: "dev" } }))
  return ["--dev", project]
}

const detail = (summary: any, step: string) => summary.steps.find((s: any) => s.step === step)
const STEPS = ["ready", "load", "ask", "act", "approve", "confirm", "refuse", "history"]

test("B4: a provisioned probe catalog, its trusted base (core.Agent.interface) included, passes readiness", () => {
  const real = catalog()
  expect(real.baseArtifactIds).toContain("core.Agent.interface")
  expect(real.artifacts.some((a) => a.namespace === "core")).toBe(true)
  expect(dedicatedVersion(real)).toBe(VERSION)
})

test("the golden path passes on a hosted gated cell, and ungated under norn dev, reading the confirmation back", async () => {
  const gated = fakeCell({ gated: true })
  const hosted = await probe(HOSTED)
  expect(hosted.summary.outcome).toBe("pass")
  expect(hosted.summary.steps.map((s: any) => [s.step, s.outcome])).toEqual(STEPS.map((s) => [s, "pass"]))
  expect(gated.seen.find((s) => s.path.endsWith(":decide"))!.authorization).toBe(`Bearer ${APPROVER}`)
  // Every write is pinned to the definitions version the ready step verified.
  const writes = gated.seen.filter((s) => s.path.startsWith("/v1/actions/"))
  expect(writes.length).toBeGreaterThan(0)
  for (const write of writes) expect(write.search.get("version")).toBe(String(VERSION))
  expect(hosted.out).not.toContain(TOKEN)
  expect(hosted.out).not.toContain(APPROVER)

  const ungated = fakeCell()
  const dev = await probe({}, devProject())
  expect(dev.summary.outcome).toBe("pass")
  expect(detail(dev.summary, "approve").outcome).toBe("skipped")
  expect(ungated.seen.some((s) => s.method === "GET" && s.path === "/v1/objects/probe.Confirmation/probe-confirmation")).toBe(true)
})

test("PROBE-LABELS-1: stdout and telemetry name the target by a fixed label, never its host or a configured name", async () => {
  const sent: string[] = []
  fakeCell({ gated: true })
  const send = globalThis.fetch
  globalThis.fetch = (async (input: string, init: RequestInit) => {
    if (String(input).startsWith("https://o2.test/")) {
      sent.push(String(init.body))
      return new Response(null)
    }
    return send(input, init)
  }) as unknown as typeof fetch
  const env = { ...HOSTED, NORN_PROBE_URL: "https://acme-corp.cells.example", NORN_PROBE_TARGET: "acme-corp", OTEL_EXPORTER_OTLP_ENDPOINT: "https://o2.test/api/norn", OTEL_SERVICE_NAME: "acme-corp", OTEL_RESOURCE_ATTRIBUTES: "deployment.environment=prod,tenant=acme-corp,host.name=acme-corp.cells.example" }
  const { summary, out } = await probe(env)
  expect(summary.target).toBe(TARGETS.hosted)
  expect(sent.length).toBeGreaterThan(0)
  for (const text of [out, ...sent]) {
    expect(text).not.toContain("acme")
    expect(text).not.toContain("cells.example")
  }
  expect(sent.join("\n")).toContain('"norn-probe"')
  expect(sent.join("\n")).toContain('"deployment.environment","value":{"stringValue":"prod"}')
})

test("PROBE-BOUNDED-1: repeated runs write the same fixed slots", async () => {
  const cell = fakeCell({ gated: true })
  for (let i = 0; i < 3; i++) expect((await probe(HOSTED)).summary.outcome).toBe("pass")
  expect([...cell.objects.keys()].sort()).toEqual(["probe.Confirmation/probe-confirmation", "probe.Reading/probe-1", "probe.Reading/probe-2", "probe.Reading/probe-3", "probe.Sentinel/probe-sentinel"])
})

test("S1: false passes fail: another run's group, a wrong sum, a stale or untyped confirmation, a different refusal", async () => {
  fakeCell({ gated: true, staleAnswer: true })
  expect(detail((await probe(HOSTED)).summary, "ask")).toMatchObject({ outcome: "fail", detail: "probe.answer_not_this_run" })
  fakeCell({ gated: true, wrongSum: true })
  expect(detail((await probe(HOSTED)).summary, "ask")).toMatchObject({ outcome: "fail", detail: "probe.answer_wrong_value" })
  fakeCell({ gated: true, staleConfirmation: true })
  expect(detail((await probe(HOSTED)).summary, "confirm")).toMatchObject({ outcome: "fail", detail: "probe.confirmation_not_this_run" })
  // The run id somewhere in the body is not a typed confirmation.
  fakeCell({ gated: true, untypedConfirmation: true })
  expect(detail((await probe(HOSTED)).summary, "confirm")).toMatchObject({ outcome: "fail", detail: "probe.confirmation_unreadable" })
  fakeCell({ staleConfirmation: true })
  expect(detail((await probe({}, devProject())).summary, "confirm")).toMatchObject({ outcome: "fail", detail: "probe.confirmation_not_this_run" })
})

test("B3: diagnostics carry the status alone, never a remote code or text, and expected codes match exactly", async () => {
  // A remote code shaped like a refusal code, but carrying a tenant slug and a token.
  const leaky = `acme_corp.${TOKEN.replace(/-/g, "_")}`
  fakeCell({ gated: true, duplicateCode: leaky })
  const wrong = await probe(HOSTED)
  expect(detail(wrong.summary, "refuse")).toMatchObject({ outcome: "fail", detail: "probe.duplicate_wrong_refusal 409" })
  // A code that only contains the expected one is not it.
  fakeCell({ gated: true, duplicateCode: "action.write_mode_failed.x" })
  expect(detail((await probe(HOSTED)).summary, "refuse")).toMatchObject({ outcome: "fail", detail: "probe.duplicate_wrong_refusal 409" })
  fakeCell({ refuseWith: { status: 403, code: "acme.secret_slug" } })
  const refused = await probe({}, devProject())
  expect(detail(refused.summary, "load")).toMatchObject({ outcome: "fail", detail: "probe.load_refused 403" })
  for (const out of [wrong.out, refused.out]) {
    expect(out).not.toContain("acme")
    expect(out).not.toContain(TOKEN.replace(/-/g, "_"))
    expect(out).not.toContain(TOKEN)
  }
})

test("PROBE-BOUNDED-1: a cell holding anything beyond its base and probe.ts's definitions is never written", async () => {
  const foreign: Artifact = { ...structuredClone(BASE[0]!), artifactId: "acme.Order.entity", namespace: "acme", name: "Order" } as Artifact
  const cases: [string, Catalog, string][] = [
    ["a tenant artifact", catalog((p) => [...p, foreign]), "probe.cell_not_dedicated"],
    // A core artifact the catalog does not list as its base is not trusted.
    ["an unlisted base artifact", catalog((p) => p, BASE.slice(1)), "probe.cell_not_dedicated"],
    ["another write target", catalog((p) => { action(p, "probe.reading.put").payload.writes = [{ change: "created", entityKind: "core.analytics.Restatement" }]; return p }), "probe.definition_target_mismatch"],
    ["another effect target", catalog((p) => { action(p, "probe.confirmation.put").payload.effectProgram.objectKind = "probe.Reading"; return p }), "probe.definition_effect_mismatch"],
    ["an upserting sentinel", catalog((p) => { action(p, "probe.sentinel.add").payload.effectProgram.writeMode = { kind: "upsert" }; return p }), "probe.definition_effect_mismatch"],
    ["another action version", catalog((p) => { action(p, "probe.confirmation.put").version = "9.9.9"; return p }), "probe.definition_version_mismatch"],
    ["a missing action", catalog((p) => p.filter((a) => a.payload?.actionId !== "probe.sentinel.add")), "probe.definitions_missing"],
    ["no base list", { ...catalog(), baseArtifactIds: undefined } as unknown as Catalog, "probe.definitions_unreadable"],
  ]
  for (const [name, served, code] of cases) {
    const cell = fakeCell({ catalog: served, gated: true })
    const { summary } = await probe(HOSTED)
    expect([name, detail(summary, "ready")]).toEqual([name, expect.objectContaining({ outcome: "fail", detail: code })])
    expect([name, cell.seen.filter((s) => s.method === "POST")]).toEqual([name, []])
  }
})

test("PROBE-BOUNDED-1: every credential, the hosted approver's included, is checked before the first request", () => {
  const cell = fakeCell()
  const refused = (env: Record<string, string | undefined>, argv: string[] = []) => {
    try {
      target(argv, env)
    } catch (error) {
      expect(error).toBeInstanceOf(ProbeError)
      return (error as { code: string }).code
    }
    return null
  }
  // A hosted probe cell is the approval path: no approver, no run.
  expect(refused({ ...HOSTED, NORN_PROBE_APPROVER_TOKEN: undefined })).toBe("probe.approver_token_required")
  expect(refused({ NORN_PROBE_REQUIRE_APPROVAL: "1" }, devProject())).toBe("probe.approver_token_required")
  expect(refused({}, devProject())).toBeNull()
  expect(refused({ ...HOSTED, NORN_PROBE_TOKEN: `${TOKEN}\nx-evil: 1` })).toBe("probe.token_invalid")
  expect(refused({ ...HOSTED, NORN_PROBE_APPROVER_TOKEN: `${APPROVER}\r\n` })).toBe("probe.approver_token_invalid")
  expect(refused({ ...HOSTED, NORN_PROBE_URL: "http://probe.cell.test" })).toBe("probe.url_not_https")
  expect(refused({ ...HOSTED, NORN_PROBE_URL: "https://u:p@probe.cell.test" })).toBe("probe.url_invalid")
  expect(refused({ ...HOSTED, NORN_PROBE_URL: "http://127.0.0.1:8787" })).toBeNull()
  expect(refused({ ...HOSTED, NORN_PROBE_URL: "http://localhost:8787" })).toBeNull()
  expect(cell.seen).toEqual([])
})

test("PROBE-SECRETS-1: a token with an embedded newline never reaches stdout or stderr", async () => {
  const cell = fakeCell()
  const bad = `${TOKEN}\nsecond-line`
  await expect(run([], { ...HOSTED, NORN_PROBE_TOKEN: bad })).rejects.toMatchObject({ code: "probe.token_invalid" })
  // The CLI entry prints the code alone.
  const child = Bun.spawnSync(["node", join(ROOT, "apps/norn-cell-do/probe/golden-path.mjs")], { env: { PATH: process.env.PATH ?? "", ...HOSTED, NORN_PROBE_TOKEN: bad } })
  const printed = child.stdout.toString() + child.stderr.toString()
  expect(child.exitCode).toBe(2)
  expect(printed.trim()).toBe("probe could not start: probe.token_invalid")
  expect(printed).not.toContain(TOKEN)
  expect(cell.seen).toEqual([])
})

test("PROBE-SECRETS-1: a failed request reports a fixed code, never the exception's message", async () => {
  fakeCell({ throwWith: `Invalid header value: Bearer ${TOKEN}` })
  const { summary, out } = await probe(HOSTED)
  expect(detail(summary, "ready")).toMatchObject({ outcome: "fail", detail: "probe.request_failed" })
  expect(out).not.toContain(TOKEN)
})
