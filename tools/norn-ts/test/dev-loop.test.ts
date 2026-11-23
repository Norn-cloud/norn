/**
 * #207 `norn dev`: the developer loop end to end, over the real native host
 * and a real SQLite cell (`NORN_DEV_HOST_BIN`, as core.sh's integration
 * phase builds and exports it).
 *
 * One scenario walks init → dev → call → break → still serving → repair →
 * resave → restart, and checks the invariants each step names: DEV-LASTGOOD-1,
 * DEV-IDEMPOTENT-1, DEV-ONE-AUTHORITY-1 (installs come only from source
 * saves), DEV-LOOPBACK-1, one session per cell, default deny without the
 * session token, and HOST-REOPEN-1.
 */
import { expect, test } from "bun:test"
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

const native = process.env.NORN_DEV_HOST_BIN
if (process.env.CI && !native) throw new Error("dev loop tests require NORN_DEV_HOST_BIN")
const CLI = join(TOOL_ROOT, "src/cli.ts")
const env = { ...process.env, NORN_DEV_HOST_BIN: native ?? "" }

type Event = Record<string, any>

/** A running `norn dev --json`, read one NDJSON event at a time. */
class Dev {
  readonly events: Event[] = []
  private readonly waiters: Array<() => void> = []
  readonly child: ChildProcessWithoutNullStreams
  constructor(readonly dir: string, extra: string[] = []) {
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
    // The host's and worker's stderr, kept (and drained, so a full pipe
    // never stalls them) for the failure report.
    this.child.stderr.on("data", (chunk: Buffer) => { this.stderr = (this.stderr + chunk.toString()).slice(-16_384) })
    this.child.on("exit", (code, signal) => {
      this.exit = { code, signal }
      this.waiters.splice(0).forEach((wake) => wake())
    })
  }
  stderr = ""
  exit: { code: number | null; signal: string | null } | undefined
  /** `error` with what `norn dev` printed and how it ended, so a failure names the host's side. */
  explain(error: unknown): Error {
    const message = error instanceof Error ? error.message : String(error)
    const exit = this.exit ? `exited code=${this.exit.code} signal=${this.exit.signal}` : "still running"
    return new Error(`${message}\n--- norn dev (pid ${this.child.pid}, ${exit})\n--- stderr (tail):\n${this.stderr}\n--- events:\n${this.events.map((event) => JSON.stringify(event)).join("\n")}`, { cause: error })
  }
  /** The next event after index `from` matching `kind`; fails on timeout or exit. */
  async next(kind: string, from: number, timeoutMs = 60_000): Promise<Event> {
    const deadline = Date.now() + timeoutMs
    for (;;) {
      const found = this.events.slice(from).find((event) => event.event === kind)
      if (found) return found
      if (this.child.exitCode !== null) throw new Error(`norn dev exited ${this.child.exitCode} waiting for ${kind}: ${JSON.stringify(this.events)}`)
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${kind}: ${JSON.stringify(this.events)}`)
      await new Promise<void>((wake) => { this.waiters.push(wake); setTimeout(wake, 250) })
    }
  }
  async stop(): Promise<void> {
    if (this.child.exitCode !== null) return
    const exited = new Promise((done) => this.child.on("exit", done))
    this.child.kill("SIGTERM")
    await exited
  }
}

const customer = (identity: string) => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "shop.Customer", title: "Customer", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String, name: Schema.String }),
  identity: [{ name: "primary", properties: ["${identity}"] }],
})
`

test("developer_loop_roundtrip: author, observe, break, repair and restart through norn dev", async () => {
  if (!native) return
  const root = mkdtempSync(join(tmpdir(), "norn-dev-loop-"))
  const dir = join(root, "shop")
  let dev: Dev | undefined
  try {
    expect(spawnSync(process.execPath, [CLI, "init", dir, "--no-install"], { env }).status).toBe(0)

    // DEV-LOOPBACK-1: the all-grants developer never listens beyond loopback.
    const exposed = spawnSync(process.execPath, [CLI, "dev", "--json", "--host", "0.0.0.0"], { cwd: dir, env, encoding: "utf8" })
    expect(exposed.status).toBe(3)
    expect(exposed.stdout).toContain("norn.dev.non_loopback")

    dev = new Dev(dir)
    const ready = await dev.next("ready", 0)
    expect(ready.active.version).toBe(1)
    expect(ready.scope).toEqual({ orgId: "local", environmentId: "dev" })
    expect(ready.actor).toBe("local:developer")
    expect(ready.stability).toBe("experimental")
    expect(JSON.parse(readFileSync(join(dir, ".norn/dev.json"), "utf8"))).toMatchObject({ format: "norn-dev/1", stability: "experimental" })
    expect(statSync(join(dir, ".norn/dev.token")).mode & 0o777).toBe(0o600)
    const token = readFileSync(join(dir, ".norn/dev.token"), "utf8").trim()
    const scope = "orgId=local&environmentId=dev"
    const call = async (sku: string, headers: Record<string, string> = { authorization: `Bearer ${token}` }, pin = "") => {
      const response = await fetch(`${ready.url}/v1/actions/shop.product.add?${scope}${pin}`, {
        method: "POST", headers,
        body: `{"input":{"sku":"${sku}","name":"Ring","price":"19.99"},"idempotencyKey":"add-${sku}"}`,
      })
      // Status and body together, so a failure shows the refusal.
      return { status: response.status, body: await response.text() }
    }

    // One session per cell: a second host refuses and names the running one.
    const second = spawnSync(process.execPath, [CLI, "dev", "--json", "--port", "0"], { cwd: dir, env, encoding: "utf8" })
    expect(second.status).toBe(3)
    expect(second.stdout).toContain("norn.cell.locked")

    // Default deny: without the session token nothing is called.
    expect(await call("SKU-0", {})).toMatchObject({ status: 403 })
    expect(await call("SKU-1")).toMatchObject({ status: 200 })
    const read = await fetch(`${ready.url}/v1/objects/shop.Product/SKU-1?${scope}`, { headers: { authorization: `Bearer ${token}` } })
    expect({ status: read.status, body: await read.text() }).toMatchObject({ status: 200, body: expect.stringContaining("19.99") })

    // DEV-LASTGOOD-1: a refused change keeps r1 serving, pointing at the source line.
    const before = dev.events.length
    writeFileSync(join(dir, "src/customer.ts"), customer("uuid"))
    const rejected = await dev.next("rejected", before)
    expect(rejected.serving).toEqual({ version: 1 })
    expect(rejected.diagnostics[0].source).toMatchObject({ file: "src/customer.ts", line: 7 })
    expect(await call("SKU-2")).toMatchObject({ status: 200 })

    // Repair: the next revision installs from the saved source.
    const repairing = dev.events.length
    writeFileSync(join(dir, "src/customer.ts"), customer("email"))
    const revision = await dev.next("revision", repairing)
    expect(revision.version).toBe(2)
    // A call pinned to the revision it was rendered from is stale now.
    expect(await call("SKU-3", { authorization: `Bearer ${token}` }, "&version=1")).toMatchObject({ status: 409 })

    // DEV-IDEMPOTENT-1: a save that changes no definition installs nothing.
    const resaving = dev.events.length
    writeFileSync(join(dir, "src/customer.ts"), `// unchanged definitions\n${customer("email")}`)
    await dev.next("unchanged", resaving)
    expect(dev.events.slice(resaving).some((event) => event.event === "revision")).toBe(false)

    // HOST-REOPEN-1: a restart reopens the cell at r2 with its objects.
    await dev.stop()
    dev = new Dev(dir)
    const reopened = await dev.next("ready", 0)
    expect(reopened.active.version).toBe(2)
    const after = await fetch(`${reopened.url}/v1/objects/shop.Product/SKU-1?${scope}`, {
      headers: { authorization: `Bearer ${readFileSync(join(dir, ".norn/dev.token"), "utf8").trim()}` },
    })
    expect({ status: after.status, body: await after.text() }).toMatchObject({ status: 200, body: expect.stringContaining("19.99") })
  } catch (error) {
    throw dev ? dev.explain(error) : error
  } finally {
    await dev?.stop()
    rmSync(root, { recursive: true, force: true })
  }
}, 240_000)

test("three_systems_dev_load: the packs' synthetic exports load whole, at startup and over observations:load, and a refused load exits with its reason", async () => {
  if (!native) return
  const root = mkdtempSync(join(tmpdir(), "norn-dev-packs-"))
  const dir = join(root, "packs")
  let dev: Dev | undefined
  try {
    expect(spawnSync(process.execPath, [CLI, "init", dir, "--template", "three-systems", "--no-install"], { env }).status).toBe(0)
    // Two exports load at startup (`--load`); the third arrives while serving.
    const data = ["shopify", "odoo"].flatMap((system) => ["--load", `data/${system}.json`])
    dev = new Dev(dir, data)
    const loaded = await dev.next("loaded", 0, 180_000)
    expect(loaded.files.map((file: Event) => file.rows)).toEqual([11, 13])
    const ready = await dev.next("ready", 0, 180_000)
    const token = readFileSync(join(dir, ".norn/dev.token"), "utf8").trim()
    const scope = "orgId=local&environmentId=dev"
    // POST /v1/observations:load with the developer's token: the ERPNext
    // export, in two slices under the route's commit bound (its snapshot rows,
    // then its incremental rows), lands whole through the mapping, and loading
    // it again lands nothing.
    const erpnext = JSON.parse(readFileSync(join(dir, "data/erpnext.json"), "utf8")) as { snapshot: unknown[]; incremental: unknown[] }
    expect(erpnext.incremental.length).toBeGreaterThan(0)
    const slices = [{ ...erpnext, incremental: [] }, { ...erpnext, snapshot: [] }]
    const rows = erpnext.snapshot.length + erpnext.incremental.length
    const load = async () => {
      const total = { status: [] as number[], rows: 0, landed: 0, alreadyLanded: 0, quarantined: 0, landedBySlice: [] as number[] }
      for (const slice of slices) {
        const response = await fetch(`${ready.url}/v1/observations:load?${scope}`, {
          method: "POST",
          headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
          body: JSON.stringify(slice),
        })
        const body = await response.json() as { rows: number; landed: number; alreadyLanded: number; quarantined: number }
        expect({ status: response.status, body }).toMatchObject({ status: 200 })
        total.status.push(response.status)
        total.rows += body.rows
        total.landed += body.landed
        total.landedBySlice.push(body.landed)
        total.alreadyLanded += body.alreadyLanded
        total.quarantined += body.quarantined
      }
      return total
    }
    const first = await load()
    expect(first).toMatchObject({ status: [200, 200], rows, quarantined: 0 })
    // Each slice lands its own rows: all 7 snapshot rows, then 5 of the 6
    // incremental rows. The sixth is the export's deliberate replay of the
    // delivery, so it alone is already landed. The north holding's 200 -> 180
    // update must land as a new version, never as already landed.
    expect(first.landedBySlice).toEqual([7, 5])
    expect(first.alreadyLanded).toBe(1)
    expect(first.landed + first.alreadyLanded).toBe(rows)
    const again = await load()
    expect(again).toMatchObject({ status: [200, 200], rows, landed: 0, alreadyLanded: rows, quarantined: 0 })
    for (const type of ["canon.Holding", "canon.OrderLine", "canon.Delivery", "canon.Posting", "canon.SettlementObservation"]) {
      const response = await fetch(`${ready.url}/v1/objects/${type}?${scope}`, { headers: { authorization: `Bearer ${token}` } })
      const body = await response.json() as { records?: unknown[]; quarantineCount?: number }
      expect({ type, status: response.status, some: (body.records?.length ?? 0) > 0, quarantined: body.quarantineCount }).toEqual({ type, status: 200, some: true, quarantined: 0 })
    }
    await dev.stop()
    dev = undefined

    // DEV-LOAD-REFUSED-1: an export the cell cannot read ends norn dev at once, with its reason.
    writeFileSync(join(dir, "data/broken.json"), JSON.stringify({ rows: [] }))
    const broken = spawnSync(process.execPath, [CLI, "dev", "--json", "--port", "0", "--load", "data/broken.json"], { cwd: dir, env, encoding: "utf8", timeout: 120_000 })
    expect(broken.status).toBe(3)
    expect(broken.stdout).toContain("norn.dev.load_refused")
    const fresh = join(root, "fresh")
    expect(spawnSync(process.execPath, [CLI, "init", fresh, "--template", "three-systems", "--no-install"], { env }).status).toBe(0)
    writeFileSync(join(fresh, "data/broken.json"), JSON.stringify({ rows: [] }))
    const refusedFirst = spawnSync(process.execPath, [CLI, "dev", "--json", "--port", "0", "--load", "data/broken.json"], { cwd: fresh, env, encoding: "utf8", timeout: 180_000 })
    expect(refusedFirst.status).toBe(3)
    expect(refusedFirst.stdout).toContain("norn.dev.load_refused")
  } finally {
    await dev?.stop()
    rmSync(root, { recursive: true, force: true })
  }
}, 600_000)

test("norn dev outside a project or without a host refuses with an actionable code", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-dev-setup-"))
  try {
    const outside = spawnSync(process.execPath, [CLI, "dev"], { cwd: root, env, encoding: "utf8" })
    expect(outside.status).toBe(1)
    expect(outside.stdout).toContain("cli.project.not_found")
    expect(spawnSync(process.execPath, [CLI, "init", join(root, "p"), "--no-install"], { env }).status).toBe(0)
    const missing = spawnSync(process.execPath, [CLI, "dev"], { cwd: join(root, "p"), env: { ...process.env, NORN_DEV_HOST_BIN: "" }, encoding: "utf8" })
    expect(missing.status).toBe(3)
    expect(missing.stdout).toContain("cli.setup.host_unavailable")
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("norn.json can point norn dev only inside the project", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-dev-confine-"))
  try {
    const dir = join(root, "p")
    expect(spawnSync(process.execPath, [CLI, "init", dir, "--no-install"], { env }).status).toBe(0)
    const manifest = JSON.parse(readFileSync(join(dir, "norn.json"), "utf8"))
    for (const change of [{ cell: join(root, "other.sqlite") }, { cell: "../other.sqlite" }, { cell: ".norn/../../other.sqlite" }, { sources: ["../elsewhere"] }, { sources: [root] }]) {
      writeFileSync(join(dir, "norn.json"), JSON.stringify({ ...manifest, ...change }))
      const refused = spawnSync(process.execPath, [CLI, "dev"], { cwd: dir, env, encoding: "utf8" })
      expect({ change, status: refused.status, code: JSON.parse(refused.stdout).code }).toEqual({ change, status: 1, code: "cli.project.invalid_manifest" })
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("the compile worker survives a symlink cycle in its sources", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-dev-cycle-"))
  try {
    const dir = join(root, "p")
    expect(spawnSync(process.execPath, [CLI, "init", dir, "--no-install"], { env }).status).toBe(0)
    symlinkSync(join(dir, "src"), join(dir, "src", "loop"))
    const once = spawnSync(process.execPath, [join(TOOL_ROOT, "src/worker.ts"), "--project", dir, "--once"], { env, encoding: "utf8", timeout: 60_000 })
    expect(once.status).toBe(0)
    expect(JSON.parse(once.stdout.trim().split("\n").at(-1)!)).toMatchObject({ event: "compiled", gen: 1, ok: true })
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}, 90_000)

/** One `worker.ts --once` run: its exit status and its last event. */
function compileOnce(dir: string): { status: number | null; event: Event } {
  const once = spawnSync(process.execPath, [join(TOOL_ROOT, "src/worker.ts"), "--project", dir, "--once"], { env, encoding: "utf8", timeout: 60_000 })
  return { status: once.status, event: JSON.parse(once.stdout.trim().split("\n").at(-1)!) }
}

test("the compile worker reads sources only inside the project, and its digest covers every compiled module", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-dev-sources-"))
  try {
    const dir = join(root, "p")
    expect(spawnSync(process.execPath, [CLI, "init", dir, "--no-install"], { env }).status).toBe(0)
    const manifest = JSON.parse(readFileSync(join(dir, "norn.json"), "utf8"))

    // A top-level module that is a symlink inside the project is compiled and digested.
    mkdirSync(join(dir, "shared"))
    writeFileSync(join(dir, "shared/customer.ts"), customer("email"))
    symlinkSync(join(dir, "shared/customer.ts"), join(dir, "src/customer.ts"))
    const linked = compileOnce(dir)
    expect(linked).toMatchObject({ status: 0, event: { ok: true } })
    expect(JSON.stringify(linked.event.artifacts)).toContain("shop.Customer")
    writeFileSync(join(dir, "shared/customer.ts"), `// edited\n${customer("email")}`)
    const edited = compileOnce(dir)
    expect(edited.event.ok).toBe(true)
    expect(edited.event.sourceDigest).not.toBe(linked.event.sourceDigest)

    // A module that links out of the project is refused. The digest fails for
    // the same reason, and the worker survives it: one event, exit 0.
    mkdirSync(join(root, "elsewhere/sub"), { recursive: true })
    writeFileSync(join(root, "elsewhere/sub/secret.ts"), customer("email"))
    rmSync(join(dir, "src/customer.ts"))
    symlinkSync(join(root, "elsewhere/sub/secret.ts"), join(dir, "src/secret.ts"))
    const escaped = compileOnce(dir)
    expect(escaped).toMatchObject({ status: 0, event: { event: "compiled", gen: 1, ok: false, sourceDigest: "" } })
    expect(escaped.event.diagnostics[0]).toMatchObject({ code: "cli.project.invalid_manifest", message: expect.stringContaining("src/secret.ts") })
    expect(JSON.stringify(escaped.event)).not.toContain("shop.Customer")
    rmSync(join(dir, "src/secret.ts"))

    // A dangling module link is a project diagnostic too, not an internal error.
    symlinkSync(join(dir, "shared/gone.ts"), join(dir, "src/gone.ts"))
    const dangling = compileOnce(dir)
    expect(dangling).toMatchObject({ status: 0, event: { ok: false } })
    expect(dangling.event.diagnostics[0]).toMatchObject({ code: "cli.project.invalid_manifest", message: expect.stringContaining("src/gone.ts") })
    rmSync(join(dir, "src/gone.ts"))

    // A source directory reached through a symlinked ancestor is refused the same way.
    symlinkSync(join(root, "elsewhere"), join(dir, "linked"))
    writeFileSync(join(dir, "norn.json"), JSON.stringify({ ...manifest, sources: ["src", "linked/sub"] }))
    const through = compileOnce(dir)
    expect(through).toMatchObject({ status: 0, event: { ok: false, sourceDigest: "" } })
    expect(through.event.diagnostics[0]).toMatchObject({ code: "cli.project.invalid_manifest", message: expect.stringContaining("linked/sub") })
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}, 240_000)
