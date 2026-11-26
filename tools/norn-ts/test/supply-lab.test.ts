import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalJson } from "../src/canonical.ts"
import { compileDashboards, compileQuestions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"

// Supply lab (registry/packs/supply-lab): its questions and dashboard sit in
// the pack directory beside its definitions, but they are not installable
// artifacts, so the registry-packs test skips them. This test holds their
// generated goldens, which the Rust stress harness
// (crates/norn-cell/tests/supply_lab_stress.rs) admits against the derived
// metric space; NORN_PACKS_UPDATE=1 regenerates them.
const PACK = join(TOOL_ROOT, "../../registry/packs/supply-lab")
const DIR = join(PACK, "questions")
const SOURCE = join(PACK, "questions.ts")
const update = process.env.NORN_PACKS_UPDATE === "1"

function golden(name: string, value: unknown): void {
  const path = join(DIR, name)
  if (update) writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`)
  expect(canonicalJson(JSON.parse(readFileSync(path, "utf8")))).toBe(canonicalJson(value as never))
}

test("supply-lab questions lower to their committed QuestionIR", () => {
  const result = compileQuestions(SOURCE)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.questions.map(q => (q as { name: string }).name)).toEqual([
    "supply.on_hand_weekly", "supply.movement_volume_weekly", "supply.fill_rate_weekly", "supply.received_weekly",
    "supply.inspected_by_receipt_week",
  ])
  golden("supply.questions.json", result.questions)
})

test("supply-lab dashboard lowers its five tiles", () => {
  const result = compileDashboards(SOURCE)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.dashboards).toHaveLength(1)
  golden("supply.dashboard.json", result.dashboards)
})

// The supply-lab workspace on the real native host (`NORN_DEV_HOST_BIN`):
// `norn init --template supply-lab`, `norn dev`, `norn seed`. Every seeded
// call is sent by a named local principal holding only the role the pack's
// access policy names (DEV-PRINCIPALS-1), so the pack acts, and refuses by
// role, exactly as its Cedar policy says.
const native = process.env.NORN_DEV_HOST_BIN
if (process.env.CI && !native) throw new Error("the supply-lab workspace test requires NORN_DEV_HOST_BIN")
const CLI = join(TOOL_ROOT, "src/cli.ts")

test("supply_lab_workspace: init, dev and seed admit the pack, seed the reference data, and refuse by role", async () => {
  if (!native) return
  const { spawn, spawnSync } = await import("node:child_process")
  const { mkdtempSync, rmSync } = await import("node:fs")
  const { tmpdir } = await import("node:os")
  const root = mkdtempSync(join(tmpdir(), "norn-supply-lab-"))
  const dir = join(root, "supply-lab")
  const env = { ...process.env, NORN_DEV_HOST_BIN: native }
  const init = spawnSync(process.execPath, [CLI, "init", dir, "--template", "supply-lab", "--no-install", "--json"], { env, encoding: "utf8" })
  expect(init.status).toBe(0)
  const created = JSON.parse(init.stdout)
  const profile = JSON.parse(readFileSync(join(TOOL_ROOT, "templates/supply-lab/template.json"), "utf8"))
  expect(created.manifest.dev.principals).toEqual(profile.principals)
  const calls = JSON.parse(readFileSync(join(PACK, "seed/supply.calls.json"), "utf8")).calls as Array<{ actor: string; action: string }>
  // Every seeded actor is a named principal, and holds the role the policy names for its action.
  const policy = JSON.parse(readFileSync(join(PACK, "artifacts/supply.access.policy.json"), "utf8")).payload.rules as Array<{ actions: string[]; allOf: Array<{ role: { anyOf: string[] } }> }>
  for (const call of calls) {
    const roles = profile.principals.find((principal: { actor: string }) => principal.actor === call.actor)?.roles as string[] | undefined
    const rule = policy.find((candidate) => candidate.actions.includes(call.action))
    expect({ call: call.action, allowed: rule?.allOf[0]?.role.anyOf.some((role) => roles?.includes(role)) }).toEqual({ call: call.action, allowed: true })
  }
  const child = spawn(process.execPath, [CLI, "dev", "--json", "--port", "0"], { cwd: dir, env })
  const events: Array<Record<string, any>> = []
  let buffer = ""
  child.stdout.on("data", (chunk: Buffer) => {
    buffer += chunk.toString()
    let newline: number
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline)
      buffer = buffer.slice(newline + 1)
      if (line.trim()) events.push(JSON.parse(line))
    }
  })
  const exited = new Promise((done) => child.on("exit", done))
  try {
    const deadline = Date.now() + 180_000
    let ready: Record<string, any> | undefined
    while (!(ready = events.find((event) => event.event === "ready"))) {
      if (child.exitCode !== null || events.some((event) => event.event === "fatal")) throw new Error(`norn dev stopped: ${JSON.stringify(events)}`)
      if (Date.now() > deadline) throw new Error(`norn dev never served: ${JSON.stringify(events)}`)
      await Bun.sleep(250)
    }
    expect(ready.principals.map((principal: { actor: string; roles: string[] }) => ({ actor: principal.actor, roles: principal.roles }))).toEqual(profile.principals)
    // DEV-READ-DEFINITIONS-1: the bundled questions and dashboard are sources, installed by norn dev itself.
    const bundled = [...JSON.parse(readFileSync(join(DIR, "supply.questions.json"), "utf8")).map((q: { name: string }) => q.name), "supply.dashboard.supply_lab"].sort()
    const installed = () => events.filter((event) => event.event === "definition" && event.lifecycle === "active" && event.kind !== "approval").map((event) => event.id).sort()
    const settle = Date.now() + 60_000
    while (installed().length < bundled.length && Date.now() < settle && !events.some((event) => event.event === "definitions_refused")) await Bun.sleep(250)
    expect({ installed: installed(), refused: events.filter((event) => event.event === "definitions_refused") }).toEqual({ installed: bundled, refused: [] })
    const seeded = spawnSync(process.execPath, [CLI, "seed", "--json"], { cwd: dir, env, encoding: "utf8", timeout: 300_000 })
    expect({ status: seeded.status, out: seeded.stdout }).toMatchObject({ status: 0 })
    const result = JSON.parse(seeded.stdout)
    expect(result).toMatchObject({ ok: true, calls: calls.length })

    const scope = `orgId=${created.manifest.scope.orgId}&environmentId=${created.manifest.scope.environmentId}`
    const token = (actor: string) => {
      const file = actor === ready!.actor ? ready!.tokenFile : actor === ready!.reviewer.actor ? ready!.reviewer.tokenFile : ready!.principals.find((principal: { actor: string }) => principal.actor === actor).tokenFile
      return readFileSync(join(dir, file), "utf8").trim()
    }
    const http = async (actor: string, method: string, path: string, body?: unknown) => {
      const response = await fetch(`${ready!.url}${path}${path.includes("?") ? "&" : "?"}${scope}`, { method, headers: { authorization: `Bearer ${token(actor)}` }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) })
      const text = await response.text()
      return { status: response.status, body: text ? JSON.parse(text) : null }
    }
    // The reference data is in the cell: as many records of each seeded type as the seed created.
    const created_ = (action: string) => calls.filter((call) => call.action === action).length
    const total = async (type: string) => (await http("clerk:hana", "GET", `/v1/objects/${type}?limit=200`)).body.records.length
    expect(await total("supply.Warehouse")).toBe(created_("supply.warehouse.register"))
    expect(await total("supply.StockMovement")).toBe(created_("supply.movement.record"))
    expect(await total("supply.ShipmentReceipt")).toBe(created_("supply.receipt.record") + created_("supply.receipt.record_blind"))

    // DEV-PRINCIPALS-1: the clerk may record a movement, and may not draft an order (the planner's).
    const movement = { movement_id: "MV-LIVE-1", warehouse_id: "WH-A", sku_id: "SKU-WATER", qty: "5", unit: "each", movement_kind: "issue", occurred_at: "2026-10-20T12:00:00Z" }
    expect((await http("clerk:hana", "POST", "/v1/actions/supply.movement.record", { input: movement, idempotencyKey: "live-1" })).status).toBe(200)
    const order = { po_id: "PO-LIVE", supplier_id: "SUP-NILE", promised_at: "2026-11-01T00:00:00Z", currency: "EGP" }
    const refused = await http("clerk:hana", "POST", "/v1/actions/supply.po.create", { input: order, idempotencyKey: "live-2" })
    expect({ status: refused.status, code: refused.body.code }).toEqual({ status: 403, code: "authz.denied" })
    // The developer holds the hosted roles, none of the pack's: the policy refuses it too.
    expect((await http(ready.actor, "POST", "/v1/actions/supply.po.create", { input: order, idempotencyKey: "live-3" })).status).toBe(403)
    expect((await http("planner:amal", "POST", "/v1/actions/supply.po.create", { input: order, idempotencyKey: "live-4" })).status).toBe(200)

    // An installed question answers over the seeded data, as Studio asks it (`POST /v1/questions:evaluate`).
    const saved = await http("clerk:hana", "GET", "/v1/definitions/supply.movement_volume_weekly")
    expect(saved.status).toBe(200)
    const answer = await http("clerk:hana", "POST", "/v1/questions:evaluate", { question: saved.body.payload })
    expect({ status: answer.status, rows: Object.keys(answer.body.rows ?? {}).length > 0 }).toEqual({ status: 200, rows: true })
    // Seeding again commits nothing new.
    const again = JSON.parse(spawnSync(process.execPath, [CLI, "seed", "--json"], { cwd: dir, env, encoding: "utf8", timeout: 300_000 }).stdout)
    expect(again).toMatchObject({ ok: true, calls: calls.length })
    expect(await total("supply.StockMovement")).toBe(created_("supply.movement.record") + 1)
    // Dropping a question from the sources retires it.
    const { unlinkSync } = await import("node:fs")
    const from = events.length
    unlinkSync(join(dir, "definitions/questions/supply.received_weekly.json"))
    const retireBy = Date.now() + 60_000
    while (!events.slice(from).some((event) => event.event === "definition" && event.id === "supply.received_weekly" && event.lifecycle === "retired") && Date.now() < retireBy) await Bun.sleep(250)
    expect(events.slice(from).some((event) => event.event === "definition" && event.id === "supply.received_weekly" && event.lifecycle === "retired")).toBe(true)

    // DEV-PRINCIPALS-1: a named principal holds only its roles' grants. An approval that only an
    // `admin` decides gates drafting an order: the planner's draft waits on it, the clerk neither
    // lists nor decides it, and the reviewer (every hosted role) does.
    const gated = events.length
    writeFileSync(join(dir, "definitions/questions/lab.po_gate.json"), JSON.stringify({ kind: "approval", id: "lab.po_gate", payload: {
      action: "supply.po.create", decider: { roles: ["admin"] }, question: "Draft this purchase order?", risk: "high" } }))
    const gateBy = Date.now() + 60_000
    const gateActive = () => events.slice(gated).some((event) => event.event === "definition" && event.id === "lab.po_gate" && event.lifecycle === "active")
    while (!gateActive() && Date.now() < gateBy) await Bun.sleep(250)
    expect({ active: gateActive(), rejected: events.slice(gated).filter((event) => event.event === "rejected") }).toEqual({ active: true, rejected: [] })
    const waiting = await http("planner:amal", "POST", "/v1/actions/supply.po.create", { input: { ...order, po_id: "PO-GATED" }, idempotencyKey: "live-5" })
    expect(waiting.status).toBe(202)
    const decision = waiting.body.decision.id as string
    const pending = async (actor: string) => (await http(actor, "GET", "/v1/decisions?state=pending")).body.total
    expect(await pending("clerk:hana")).toBe(0)
    const decide = (actor: string) => http(actor, "POST", `/v1/decisions/${decision}:decide`, { choice: "approve", idempotencyKey: `decide-${actor}` })
    expect((await decide("clerk:hana")).status).toBe(404)
    expect(await pending(ready.reviewer.actor)).toBe(1)
    // Loading observations is a source write, the hosted admins' grant: the clerk is answered as
    // someone outside the cell; the developer reaches the loader, which refuses an empty load.
    const load = { profile: "synthetic-erpnext-three/1", snapshot: [] }
    const outsider = await fetch(`${ready.url}/v1/observations:load?${scope}`, { method: "POST", body: JSON.stringify(load) })
    expect((await http("clerk:hana", "POST", "/v1/observations:load", load)).status).toBe(outsider.status)
    expect((await http(ready.actor, "POST", "/v1/observations:load", load)).status).toBe(400)
  } finally {
    child.kill("SIGTERM")
    await exited
    rmSync(root, { recursive: true, force: true })
  }
}, 600_000)
