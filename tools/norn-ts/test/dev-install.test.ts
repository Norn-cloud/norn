/**
 * HOST-INSTALL-1 on a local `norn dev` cell, over the real native host (`NORN_DEV_HOST_BIN`): a
 * project serving the three-systems set takes the batches fixture pack live. A broken
 * pack is rejected and the last good revision keeps serving; the fixture
 * pack installs as the next revision; a batch that loses mass is refused and a
 * correct one is accepted through the pack's own access policy.
 */
import { expect, test } from "bun:test"
import { spawn, spawnSync, type ChildProcessWithoutNullStreams } from "node:child_process"
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"

const native = process.env.NORN_DEV_HOST_BIN
if (process.env.CI && !native) throw new Error("dev install tests require NORN_DEV_HOST_BIN")
const CLI = join(TOOL_ROOT, "src/cli.ts")
const REPO = join(TOOL_ROOT, "../..")
const env = { ...process.env, NORN_DEV_HOST_BIN: native ?? "" }

type Event = Record<string, any>

class Dev {
  readonly events: Event[] = []
  readonly child: ChildProcessWithoutNullStreams
  constructor(dir: string) {
    this.child = spawn(process.execPath, [CLI, "dev", "--json", "--port", "0"], { cwd: dir, env })
    let buffer = ""
    this.child.stdout.on("data", (chunk: Buffer) => {
      buffer += chunk.toString()
      let newline: number
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline)
        buffer = buffer.slice(newline + 1)
        if (line.trim()) this.events.push(JSON.parse(line))
      }
    })
  }
  async next(kind: string, from: number, timeoutMs = 90_000): Promise<Event> {
    const deadline = Date.now() + timeoutMs
    for (;;) {
      const found = this.events.slice(from).find((event) => event.event === kind)
      if (found) return found
      if (this.child.exitCode !== null) throw new Error(`norn dev exited ${this.child.exitCode} waiting for ${kind}: ${JSON.stringify(this.events)}`)
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${kind}: ${JSON.stringify(this.events)}`)
      await new Promise((wake) => setTimeout(wake, 250))
    }
  }
  async stop(): Promise<void> {
    if (this.child.exitCode !== null) return
    const exited = new Promise((done) => this.child.on("exit", done))
    this.child.kill("SIGTERM")
    await exited
  }
}

test("a pack installs live on norn dev: broken pack refused, the pack installs, a lossy batch refused, a correct one accepted", async () => {
  if (!native) return
  const root = mkdtempSync(join(tmpdir(), "norn-dev-install-"))
  const dir = join(root, "trading")
  let dev: Dev | undefined
  try {
    expect(spawnSync(process.execPath, [CLI, "init", dir, "--no-install"], { env }).status).toBe(0)
    // The registry layout the batches pack imports from (its Holding link), and
    // the three-systems set as pre-compiled artifacts, one envelope per file.
    cpSync(join(REPO, "registry/core/src"), join(dir, "registry/core/src"), { recursive: true })
    mkdirSync(join(dir, "registry/packs/canon"), { recursive: true })
    for (const file of readdirSync(join(REPO, "registry/packs/canon")).filter((f) => f.endsWith(".ts"))) {
      cpSync(join(REPO, "registry/packs/canon", file), join(dir, "registry/packs/canon", file))
    }
    mkdirSync(join(dir, "fixtures/packs/batches"), { recursive: true })
    mkdirSync(join(dir, "base"))
    const set = JSON.parse(readFileSync(join(REPO, "fixtures/demo/explorer/three-systems.definitions.json"), "utf8"))
    for (const artifact of set) writeFileSync(join(dir, "base", `${artifact.artifactId}.json`), JSON.stringify(artifact))
    const manifest = JSON.parse(readFileSync(join(dir, "norn.json"), "utf8"))
    writeFileSync(join(dir, "norn.json"), JSON.stringify({ ...manifest, sources: ["base", "fixtures/packs/batches"] }))
    rmSync(join(dir, "src"), { recursive: true, force: true })

    dev = new Dev(dir)
    const ready = await dev.next("ready", 0)
    const serving = ready.active.version
    const token = readFileSync(join(dir, ".norn/dev.token"), "utf8").trim()
    const query = `orgId=${ready.scope.orgId}&environmentId=${ready.scope.environmentId}`
    const batch = async (id: string, output: string, loss: string) => {
      const response = await fetch(`${ready.url}/v1/actions/lab.batch.record?${query}`, {
        method: "POST", headers: { authorization: `Bearer ${token}` },
        body: JSON.stringify({ input: { id, material: "copper", input_g: "100.000", output_g: output, loss_g: loss, loss_reason: null }, idempotencyKey: id }),
      })
      return { status: response.status, body: await response.text() }
    }

    // A broken pack: its law reads a property the Batch does not have.
    const source = readFileSync(join(REPO, "fixtures/packs/batches/batches.ts"), "utf8")
    const broken = source.replace("self.output_g + self.loss_g", "self.output_g + self.loss_kg")
    expect(broken).not.toBe(source)
    const breaking = dev.events.length
    writeFileSync(join(dir, "fixtures/packs/batches/batches.ts"), broken)
    const rejected = await dev.next("rejected", breaking)
    expect(rejected.serving).toEqual({ version: serving })
    expect(rejected.diagnostics.length).toBeGreaterThan(0)

    // The fixture pack installs as the next revision.
    const fixing = dev.events.length
    writeFileSync(join(dir, "fixtures/packs/batches/batches.ts"), source)
    const revision = await dev.next("revision", fixing)
    expect(revision.version).toBe(serving + 1)

    const lossy = await batch("m1", "98.000", "0.000")
    expect(lossy.status, lossy.body).toBeGreaterThanOrEqual(400)
    const correct = await batch("m2", "98.000", "2.000")
    expect(correct.status, correct.body).toBe(200)
    expect(JSON.parse(correct.body).outcome).toBe("committed")
  } finally {
    await dev?.stop()
    rmSync(root, { recursive: true, force: true })
  }
}, 300_000)
