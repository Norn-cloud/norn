// The golden-path probe (apps/norn-cell-do/probe/golden-path.mjs) against a fake hosted cell:
// PROBE-SECRETS-1 (tokens are never printed, travel only over HTTPS or loopback, and a token
// that could split a header is refused before any request), PROBE-BOUNDED-1 (every credential
// is checked before the first write; a cell holding anything but the probe is never written)
// and the golden path's own checks (this run's rows, the confirmation read back, the exact
// duplicate refusal).

import { afterEach, expect, test } from "bun:test"
// @ts-expect-error: the probe is plain JavaScript without declarations.
import { ProbeError, run, target } from "../../../apps/norn-cell-do/probe/golden-path.mjs"

const TOKEN = "tok-secret-1234"
const APPROVER = "approver-secret-5678"
const HOSTED = { NORN_PROBE_URL: "https://probe.cell.test", NORN_PROBE_ORG: "probe-org", NORN_PROBE_ENV: "probe", NORN_PROBE_TOKEN: TOKEN }
/** The catalog's artifacts, shaped as norn dev answers them: an entity names its object type, an action its id. */
const artifact = (name: string) => {
  const namespace = name.split(".")[0]
  return /^[a-z]+\.[A-Z]/.test(name)
    ? { artifactId: `${name}.entity`, kind: "entity", namespace, payload: { objectType: { name } } }
    : { artifactId: `${name}.action`, kind: "action", namespace, payload: { actionId: name } }
}
const DEFINITIONS = ["probe.Reading", "probe.reading.put", "probe.Confirmation", "probe.confirmation.put", "probe.Sentinel", "probe.sentinel.add"]

type Seen = { method: string; path: string; authorization: string | null; body: any }
type Fault = Partial<{ extraArtifact: string; gated: boolean; staleAnswer: boolean; staleConfirmation: boolean; duplicateCode: string; throwWith: string }>

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
})

/** A hosted probe cell: one in-memory store, the probe's routes, and the faults a test names. */
function fakeCell(fault: Fault = {}) {
  const seen: Seen[] = []
  const objects = new Map<string, any>()
  let parked: any = null
  globalThis.fetch = (async (input: string, init: RequestInit) => {
    const url = new URL(input)
    const body = init.body === undefined ? null : JSON.parse(String(init.body))
    seen.push({ method: init.method ?? "GET", path: url.pathname, authorization: new Headers(init.headers).get("authorization"), body })
    if (fault.throwWith !== undefined) throw new TypeError(fault.throwWith)
    const path = url.pathname
    if (path === "/v1/definitions") return Response.json({ artifacts: [...DEFINITIONS, ...(fault.extraArtifact ? [fault.extraArtifact] : [])].map(artifact) })
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
      const runs = new Set([...objects.values()].filter((o) => o.region !== undefined).map((o) => o.run))
      return Response.json({ rows: fault.staleAnswer ? { '["older-run"]': { value: "15.75" } } : Object.fromEntries([...runs].map((r) => [JSON.stringify([r]), { value: "15.75" }])) })
    }
    const object = /^\/v1\/objects\/([^/]+)\/([^/]+)(\/history)?$/.exec(path)
    if (object !== null) {
      const found = objects.get(`${object[1]}/${object[2]}`)
      if (found === undefined) return Response.json({ code: "object.not_found" }, { status: 404 })
      if (object[3]) return Response.json({ history: [{}] })
      return Response.json({ object: fault.staleConfirmation && object[1] === "probe.Confirmation" ? { ...found, run: "older-run" } : found })
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

const detail = (summary: any, step: string) => summary.steps.find((s: any) => s.step === step)

test("the golden path passes against a cell that behaves, ungated and gated, and reads the confirmation back", async () => {
  const ungated = fakeCell()
  const { summary, out } = await probe(HOSTED)
  expect(summary.outcome).toBe("pass")
  expect(summary.steps.map((s: any) => [s.step, s.outcome])).toEqual([["ready", "pass"], ["load", "pass"], ["ask", "pass"], ["act", "pass"], ["approve", "skipped"], ["confirm", "pass"], ["refuse", "pass"], ["history", "pass"]])
  expect(ungated.seen.some((s) => s.method === "GET" && s.path === "/v1/objects/probe.Confirmation/probe-confirmation")).toBe(true)
  expect(out).not.toContain(TOKEN)

  const gated = fakeCell({ gated: true })
  const second = await probe({ ...HOSTED, NORN_PROBE_REQUIRE_APPROVAL: "1", NORN_PROBE_APPROVER_TOKEN: APPROVER })
  expect(second.summary.outcome).toBe("pass")
  expect(detail(second.summary, "approve").outcome).toBe("pass")
  expect(gated.seen.find((s) => s.path.endsWith(":decide"))!.authorization).toBe(`Bearer ${APPROVER}`)
  expect(second.out).not.toContain(APPROVER)
})

test("PROBE-BOUNDED-1: repeated runs write the same fixed slots", async () => {
  const cell = fakeCell()
  for (let i = 0; i < 3; i++) expect((await probe(HOSTED)).summary.outcome).toBe("pass")
  expect([...cell.objects.keys()].sort()).toEqual(["probe.Confirmation/probe-confirmation", "probe.Reading/probe-1", "probe.Reading/probe-2", "probe.Reading/probe-3", "probe.Sentinel/probe-sentinel"])
})

test("false passes fail: rows from another run, a stale confirmation, a different refusal", async () => {
  fakeCell({ staleAnswer: true })
  const stale = await probe(HOSTED)
  expect(detail(stale.summary, "ask")).toMatchObject({ outcome: "fail", detail: "probe.answer_not_this_run" })
  fakeCell({ staleConfirmation: true })
  expect(detail((await probe(HOSTED)).summary, "confirm")).toMatchObject({ outcome: "fail", detail: "probe.confirmation_not_this_run" })
  fakeCell({ staleConfirmation: true, gated: true })
  expect(detail((await probe({ ...HOSTED, NORN_PROBE_APPROVER_TOKEN: APPROVER })).summary, "confirm")).toMatchObject({ outcome: "fail", detail: "probe.confirmation_not_this_run" })
  fakeCell({ duplicateCode: "commit.duplicate_key" })
  expect(detail((await probe(HOSTED)).summary, "refuse")).toMatchObject({ outcome: "fail", detail: "probe.duplicate_wrong_refusal 409 commit.duplicate_key" })
})

test("PROBE-BOUNDED-1: a cell holding anything but the probe is never written", async () => {
  const cell = fakeCell({ extraArtifact: "acme.Order" })
  const { summary } = await probe(HOSTED)
  expect(detail(summary, "ready")).toMatchObject({ outcome: "fail", detail: "probe.cell_not_dedicated" })
  expect(cell.seen.filter((s) => s.method === "POST")).toEqual([])
})

test("PROBE-BOUNDED-1: every credential is checked before the first request", () => {
  const cell = fakeCell()
  const refused = (env: Record<string, string>) => {
    try {
      target([], env)
    } catch (error) {
      expect(error).toBeInstanceOf(ProbeError)
      return (error as { code: string }).code
    }
    return null
  }
  expect(refused({ ...HOSTED, NORN_PROBE_REQUIRE_APPROVAL: "1" })).toBe("probe.approver_token_required")
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
  const child = Bun.spawnSync(["node", new URL("../../../apps/norn-cell-do/probe/golden-path.mjs", import.meta.url).pathname], { env: { PATH: process.env.PATH ?? "", ...HOSTED, NORN_PROBE_TOKEN: bad } })
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
