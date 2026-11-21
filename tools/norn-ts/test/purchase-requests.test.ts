/**
 * The builder loop on the purchase-requests fixture (#1450), end to end over
 * the real native host (`NORN_DEV_HOST_BIN`) and a real SQLite cell:
 * define → load → workflow → approval, as the dogfood run walked it.
 *
 * - DEV-DEFINITIONS-1: the sources' `Norn.approval` is installed by the dev
 *   host through the hosted admit, sign-off and install path, and retired
 *   when the sources drop it.
 * - APPROVAL-WHEN-1: a submit over 1000 parks; the requester never decides it.
 * - APPROVAL-RECOVERY-1: the sign-off runs nothing; the requester's own retry
 *   of the parked submit commits it once.
 * - DEV-ACCESS-1: the access policy decides every local principal's call,
 *   so a requester never approves their own request (structured refusal).
 * - CALLER-FIELD-1: `requester` is the caller; a caller cannot supply it.
 * - MAPPING-SOURCE-REF-1 / MAPPING-DECIMAL-TEXT-1: the ERPNext export loads
 *   through a mapping bound to the project's source schema by reference,
 *   with its decimal text amounts.
 */
import { expect, test } from "bun:test"
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process"
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

const native = process.env.NORN_DEV_HOST_BIN
if (process.env.CI && !native) throw new Error("dev loop tests require NORN_DEV_HOST_BIN")
const CLI = join(TOOL_ROOT, "src/cli.ts")
const FIXTURE = join(TOOL_ROOT, "..", "..", "fixtures", "examples", "purchase-requests")
const env = { ...process.env, NORN_DEV_HOST_BIN: native ?? "" }

type Event = Record<string, any>

class Dev {
  readonly events: Event[] = []
  private readonly waiters: Array<() => void> = []
  readonly child: ChildProcessWithoutNullStreams
  private exited = false
  constructor(dir: string, extra: string[] = []) {
    this.child = spawn(process.execPath, [CLI, "dev", "--json", "--port", "0", ...extra], { cwd: dir, env })
    let buffer = ""
    this.child.stdout.on("data", (chunk: Buffer) => {
      buffer += chunk.toString()
      let newline: number
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline)
        buffer = buffer.slice(newline + 1)
        if (line.trim()) this.events.push(JSON.parse(line))
        this.waiters.splice(0).forEach((wake) => wake())
      }
    })
    this.child.on("exit", () => { this.exited = true; this.waiters.splice(0).forEach((wake) => wake()) })
  }
  async next(match: (event: Event) => boolean, from: number, timeoutMs = 60_000): Promise<Event> {
    const deadline = Date.now() + timeoutMs
    for (;;) {
      const found = this.events.slice(from).find(match)
      if (found) return found
      if (this.exited) throw new Error(`norn dev exited ${this.child.exitCode ?? this.child.signalCode}: ${JSON.stringify(this.events)}`)
      if (Date.now() > deadline) throw new Error(`timed out: ${JSON.stringify(this.events)}`)
      await new Promise<void>((wake) => { this.waiters.push(wake); setTimeout(wake, 250) })
    }
  }
  async stop(): Promise<void> {
    if (this.exited) return
    const exited = new Promise((done) => this.child.on("exit", done))
    this.child.kill("SIGTERM")
    await exited
  }
}

test("purchase_requests_builder_loop: load, workflow, approval and separation of duties on norn dev", async () => {
  if (!native) return
  const root = mkdtempSync(join(tmpdir(), "norn-purchase-requests-"))
  const dir = join(root, "purchase-requests")
  cpSync(FIXTURE, dir, { recursive: true, filter: (path) => !path.includes(".norn") })
  let dev: Dev | undefined
  try {
    dev = new Dev(dir, ["--load", "data/erpnext.json"])
    const ready = await dev.next((event) => event.event === "ready", 0, 120_000)
    const scope = "orgId=org_purchasing&environmentId=dev"
    const developer = readFileSync(join(dir, ".norn/dev.token"), "utf8").trim()
    const reviewer = readFileSync(join(dir, ".norn/dev.reviewer.token"), "utf8").trim()
    const http = async (token: string, method: string, path: string, body?: unknown) => {
      const response = await fetch(`${ready.url}${path}${path.includes("?") ? "&" : "?"}${scope}`, {
        method, headers: { authorization: `Bearer ${token}` }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      })
      const text = await response.text()
      return { status: response.status, body: text ? JSON.parse(text) : null }
    }
    const act = (token: string, action: string, input: Record<string, unknown>, key: string) =>
      http(token, "POST", `/v1/actions/purchasing.request.${action}`, { input, idempotencyKey: key })
    const request = async (id: string) => (await http(developer, "GET", `/v1/objects/purchasing.PurchaseRequest/${id}`)).body

    // DEV-DEFINITIONS-1: the approval was installed by signing its restatement.
    const installed = await dev.next((event) => event.event === "definition" && event.id === "purchasing.large_requests", 0)
    expect(installed).toMatchObject({ kind: "approval", version: 1, lifecycle: "active" })
    expect(installed.restatement).toContain(`amount > "1000" waits for a person to approve it`)
    const approval = await http(developer, "GET", "/v1/definitions/purchasing.large_requests")
    expect(approval.status).toBe(200)
    expect(approval.body.signoff).toMatchObject({ signer: "local:developer" })

    // The ERPNext export loaded through the mapping bound by reference; its
    // decimal text became exact amounts and its states the lifecycle's.
    expect(dev.events.some((event) => event.event === "loaded")).toBe(true)
    expect((await request("MAT-MR-2026-00002")).values).toMatchObject({ amount: 7800, status: "submitted", requester: "amira@example.test" })

    // CALLER-FIELD-1: the server sets the requester; a caller cannot.
    const forged = await act(developer, "create", { request_id: "PR-F", amount: "50.00", currency: "USD", supplier_id: "SUP-0001", requester: "someone@example.test" }, "c-forged")
    expect({ status: forged.status, code: forged.body.code }).toEqual({ status: 400, code: "action.invalid_input" })
    expect((await act(developer, "create", { request_id: "PR-1", amount: "2500.00", currency: "USD", supplier_id: "SUP-0001" }, "c-1")).status).toBe(200)
    expect((await request("PR-1")).values).toMatchObject({ requester: "local:developer", status: "draft" })

    // The law: a request is in its supplier's currency.
    const mismatch = await act(developer, "create", { request_id: "PR-X", amount: "200.00", currency: "EGP", supplier_id: "SUP-0001" }, "c-x")
    expect(mismatch.status).toBe(422)
    expect(JSON.stringify(mismatch.body)).toContain("currency_matches_supplier")

    // The guard: the small path is only for up to 1000.
    const small = await act(developer, "submit_small", { request_id: "PR-1" }, "s-small")
    expect(small.status).toBe(422)
    expect(JSON.stringify(small.body)).toContain("guard")

    // APPROVAL-WHEN-1: a submit over 1000 waits for a decider.
    const parked = await act(developer, "submit", { request_id: "PR-1" }, "s-1")
    expect(parked.status).toBe(202)
    expect((await request("PR-1")).values.status).toBe("draft")
    const decision = parked.body.decision.id as string

    // The requester never decides their own request: it is not theirs to see.
    expect((await http(developer, "GET", "/v1/decisions?state=pending")).body.total).toBe(0)
    const own = await http(developer, "POST", `/v1/decisions/${decision}:decide`, { choice: "approve", idempotencyKey: "own" })
    expect({ status: own.status, code: own.body.code }).toEqual({ status: 404, code: "decision.not_found" })
    const signed = await http(reviewer, "POST", `/v1/decisions/${decision}:decide`, { choice: "approve", idempotencyKey: "sign" })
    expect(signed.status).toBe(200)
    // APPROVAL-RECOVERY-1: the reviewer's sign-off runs nothing. The call
    // commits only on its requester's own authenticated retry, under the
    // requester's current authority, exactly as approved (same key, input).
    expect(signed.body.call).toEqual({ committed: false, state: "not_executed" })
    expect((await request("PR-1")).values.status).toBe("draft")
    expect((await act(developer, "submit", { request_id: "PR-1" }, "s-1")).status).toBe(200)
    expect((await request("PR-1")).values.status).toBe("submitted")
    const replayed = await http(reviewer, "POST", `/v1/decisions/${decision}:decide`, { choice: "approve", idempotencyKey: "sign" })
    expect(replayed.body.call).toEqual({ committed: true, state: "committed" })

    // ACTION-CATALOG-STATE-1: the cell offers only what can act on PR-1 now.
    const offered = async (id: string) => ((await http(reviewer, "GET", `/v1/actions?objectKind=purchasing.PurchaseRequest&objectId=${id}`)).body.items as Event[])
      .map((item) => item.actionId).sort()
    expect(await offered("PR-1")).toEqual(["purchasing.request.approve", "purchasing.request.create", "purchasing.request.reject"])
    expect((await http(reviewer, "GET", "/v1/actions?objectKind=purchasing.PurchaseRequest&objectId=PR-NONE")).status).toBe(404)

    // DEV-ACCESS-1: the requester never approves their own request.
    const selfApproved = await act(developer, "approve", { request_id: "PR-1" }, "a-own")
    expect({ status: selfApproved.status, code: selfApproved.body.code }).toEqual({ status: 403, code: "authz.denied" })
    expect((await request("PR-1")).values.status).toBe("submitted")
    // Only the requester submits their request.
    expect((await act(reviewer, "submit_small", { request_id: "MAT-MR-2026-00003" }, "s-other")).status).toBe(403)
    // Someone else approves it.
    expect((await act(reviewer, "approve", { request_id: "PR-1" }, "a-1")).status).toBe(200)
    expect((await request("PR-1")).values.status).toBe("approved")

    // Up to 1000 is approved at once, with no decision.
    expect((await act(developer, "create", { request_id: "PR-2", amount: "500.00", currency: "USD", supplier_id: "SUP-0001" }, "c-2")).status).toBe(200)
    expect((await act(developer, "submit_small", { request_id: "PR-2" }, "s-2")).status).toBe(200)
    expect((await request("PR-2")).values.status).toBe("approved")
    expect(await offered("PR-2")).toEqual(["purchasing.request.create"])

    // Dropping the approval from the sources retires it.
    const source = readFileSync(join(dir, "src/purchasing.ts"), "utf8")
    const from = dev.events.length
    writeFileSync(join(dir, "src/purchasing.ts"), source.slice(0, source.indexOf("// A request over 1000 is signed off")))
    const retired = await dev.next((event) => event.event === "definition" && event.lifecycle === "retired", from)
    expect(retired).toMatchObject({ kind: "approval", id: "purchasing.large_requests", version: 2 })
  } finally {
    await dev?.stop()
    rmSync(root, { recursive: true, force: true })
  }
}, 600_000)

test("a refused row names the field, its value and the type expected", async () => {
  if (!native) return
  const root = mkdtempSync(join(tmpdir(), "norn-purchase-requests-bad-"))
  const dir = join(root, "purchase-requests")
  cpSync(FIXTURE, dir, { recursive: true, filter: (path) => !path.includes(".norn") })
  const exported = JSON.parse(readFileSync(join(dir, "data/erpnext.json"), "utf8"))
  exported.snapshot.find((row: Event) => row.record.name === "MAT-MR-2026-00002").record.grand_total = "7,800.00"
  writeFileSync(join(dir, "data/bad.json"), JSON.stringify(exported))
  const dev = new Dev(dir, ["--load", "data/bad.json"])
  try {
    const fatal = await dev.next((event) => event.event === "fatal", 0, 120_000)
    expect(fatal.diagnostic.code).toBe("norn.dev.load_refused")
    expect(fatal.diagnostic.message).toContain("mapping.runtime.invalid_record")
    expect(fatal.diagnostic.message).toContain('field "grand_total" value "7,800.00" is not a number')
  } finally {
    await dev.stop()
    rmSync(root, { recursive: true, force: true })
  }
}, 300_000)

test("norn check, norn explain and norn dev read the project through one loader", async () => {
  const { readProject } = await import("../src/project.ts")
  const { compileProjectPack, compileProject } = await import("../src/worker.ts")
  const { admit, pendingSourceSchemas } = await import("../src/pack.ts")
  const { root, manifest } = readProject(FIXTURE)
  const pack = compileProjectPack(root, manifest)
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  const event = compileProject(root, manifest, 1)
  // The dev worker sends exactly what check and explain compile: the same
  // artifacts (with the cell-owned types a mapping needs) and definitions.
  expect(event.artifacts).toEqual([...pack.baseArtifacts, ...pack.artifacts])
  expect(event.definitions).toEqual(pack.retained)
  expect(pack.retained.map((definition) => definition.id)).toEqual(["purchasing.large_requests"])
  expect(pack.artifacts.map((artifact) => (artifact as { artifactId: string }).artifactId)).toContain("norn.ObservedLinkClaim.entity")
  // The mapping binds its source schema by reference: the hash is pending
  // until the checker's own hash of the schema pins it.
  expect(pendingSourceSchemas(pack.artifacts)).toBe(true)
  const { checker } = await import("./checker.ts")
  if (!checker) return
  const { report, input } = admit(checker, pack)
  expect(report.diagnostics.diagnostics.filter((finding) => finding.severity === "error")).toEqual([])
  expect(report.admitted).toBe(true)
  expect(pendingSourceSchemas(input.artifacts)).toBe(false)
}, 120_000)

// DEV-DEFINITIONS-1: a gate the author declared is never silently dropped. A
// JSON source that names the approval kind but is malformed, an id two JSON
// sources declare, and a default-exported approval each refuse the compile.
test("a malformed, duplicated or default-exported approval refuses the compile", async () => {
  const { readProject } = await import("../src/project.ts")
  const { compileProjectPack } = await import("../src/worker.ts")
  const root = mkdtempSync(join(tmpdir(), "norn-purchase-requests-defs-"))
  const dir = join(root, "purchase-requests")
  const failure = () => {
    const { root: projectRoot, manifest } = readProject(dir)
    const pack = compileProjectPack(projectRoot, manifest)
    if (pack.ok) throw new Error("compiled")
    return pack.failures.flatMap((f) => f.diagnostics).map((d) => d.message).join("\n")
  }
  const reset = () => { rmSync(dir, { recursive: true, force: true }); cpSync(FIXTURE, dir, { recursive: true, filter: (path) => !path.includes(".norn") }) }
  const payload = { action: "purchasing.request.submit", decider: { roles: ["admin"] }, question: "Sign off?", risk: "low" }
  try {
    reset()
    writeFileSync(join(dir, "src/large.json"), JSON.stringify({ kind: "approval", id: "purchasing.other", payload: [] }))
    expect(failure()).toContain("src/large.json declares an approval that is not { kind, id, payload }")

    reset()
    writeFileSync(join(dir, "src/a.json"), JSON.stringify({ kind: "approval", id: "purchasing.other", payload }))
    writeFileSync(join(dir, "src/b.json"), JSON.stringify({ kind: "approval", id: "purchasing.other", payload: { ...payload, risk: "high" } }))
    expect(failure()).toContain("definition purchasing.other is declared by more than one JSON source")

    reset()
    const source = readFileSync(join(dir, "src/purchasing.ts"), "utf8")
    writeFileSync(join(dir, "src/purchasing.ts"), source.replace("export const LargeRequests = Norn.approval(", "export default Norn.approval("))
    expect(failure()).toContain("approvals require named const bindings")
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}, 120_000)

// #1469 S10: a fractional threshold compiles to its exact decimal text, never
// a double (`1000.10`, not `1000.1`); an integer stays a number.
test("a decimal approval threshold compiles exactly as written", async () => {
  const { readProject } = await import("../src/project.ts")
  const { compileProjectPack } = await import("../src/worker.ts")
  const root = mkdtempSync(join(tmpdir(), "norn-purchase-requests-when-"))
  const dir = join(root, "purchase-requests")
  const threshold = (literal: string) => {
    rmSync(dir, { recursive: true, force: true })
    cpSync(FIXTURE, dir, { recursive: true, filter: (path) => !path.includes(".norn") })
    const file = join(dir, "src/purchasing.ts")
    writeFileSync(file, readFileSync(file, "utf8").replace('op: "gt", value: "1000" }', `op: "gt", value: ${literal} }`))
    const { root: projectRoot, manifest } = readProject(dir)
    const pack = compileProjectPack(projectRoot, manifest)
    if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
    return (pack.retained[0]!.payload as { when: { value: unknown } }).when.value
  }
  try {
    expect(threshold("1000.10")).toBe("1000.10")
    expect(threshold("1000")).toBe(1000)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}, 120_000)

// #1469 S10: a JSON source's numbers are checked as written, before any
// rounding: a fractional threshold (`1000.00000000000001` would parse as
// 1000) refuses the compile; an integer keeps its exact value.
test("a JSON approval threshold is refused unless it is exact as written", async () => {
  const { readProject } = await import("../src/project.ts")
  const { compileProjectPack } = await import("../src/worker.ts")
  const root = mkdtempSync(join(tmpdir(), "norn-purchase-requests-json-"))
  const dir = join(root, "purchase-requests")
  const compiled = (value: string) => {
    rmSync(dir, { recursive: true, force: true })
    cpSync(FIXTURE, dir, { recursive: true, filter: (path) => !path.includes(".norn") })
    writeFileSync(join(dir, "src/other.json"), `{ "kind": "approval", "id": "purchasing.other", "payload": { "action": "purchasing.request.approve", "decider": { "roles": ["admin"] }, "question": "Sign off?", "risk": "low", "when": { "property": "amount", "op": "gt", "value": ${value} } } }`)
    const { root: projectRoot, manifest } = readProject(dir)
    return compileProjectPack(projectRoot, manifest)
  }
  const message = (value: string) => {
    const pack = compiled(value)
    if (pack.ok) throw new Error(`compiled ${value}`)
    return pack.failures.flatMap((f) => f.diagnostics).map((d) => d.message).join("\n")
  }
  try {
    expect(message("1000.00000000000001")).toContain("writes the number 1000.00000000000001")
    expect(message("1000.5")).toContain("writes the number 1000.5")
    expect(message("1e3")).toContain("writes the number 1e3")
    const pack = compiled("1000")
    if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
    const other = pack.retained.find((definition) => definition.id === "purchasing.other")!
    expect((other.payload as { when: { value: unknown } }).when.value).toBe(1000)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}, 120_000)

// #1469 S10: the compiler lowers a threshold from its source text, through a
// `const` reference, before any Number conversion.
test("a const or long decimal approval threshold compiles exactly", async () => {
  const { readProject } = await import("../src/project.ts")
  const { compileProjectPack } = await import("../src/worker.ts")
  const root = mkdtempSync(join(tmpdir(), "norn-purchase-requests-const-"))
  const dir = join(root, "purchase-requests")
  const compile = (literal: string, prelude = "") => {
    rmSync(dir, { recursive: true, force: true })
    cpSync(FIXTURE, dir, { recursive: true, filter: (path) => !path.includes(".norn") })
    const file = join(dir, "src/purchasing.ts")
    writeFileSync(file, `${prelude}\n${readFileSync(file, "utf8").replace('op: "gt", value: "1000" }', `op: "gt", value: ${literal} }`)}`)
    const { root: projectRoot, manifest } = readProject(dir)
    return compileProjectPack(projectRoot, manifest)
  }
  const threshold = (literal: string, prelude = "") => {
    const pack = compile(literal, prelude)
    if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
    return (pack.retained[0]!.payload as { when: { value: unknown } }).when.value
  }
  try {
    expect(threshold("1000.00000000000001")).toBe("1000.00000000000001")
    expect(threshold("LIMIT", "const LIMIT = 1000.10")).toBe("1000.10")
    expect(threshold("LIMIT", "const LIMIT = 1000")).toBe(1000)
    const exponent = compile("1e3")
    expect(exponent.ok).toBe(false)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}, 120_000)
