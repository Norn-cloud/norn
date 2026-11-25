import { spawn, spawnSync, type ChildProcess } from "node:child_process"
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs"
import { createServer } from "node:net"
import { tmpdir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

/**
 * A live workspace for browser rows: a registry pack template laid out by
 * `norn init --template <name>`, served by the real native dev host
 * (`NORN_DEV_HOST_BIN`, `norn dev`), seeded by `norn seed`, and opened in the
 * built Studio by `scripts/local-cell.ts`, one local Studio per principal.
 * Nothing is stubbed: every page reads, and every call is decided by, the
 * dev host.
 *
 * CI builds the host before the browser phase, so a missing host fails there;
 * a hand run without it skips the rows that need it.
 */
const HERE = dirname(fileURLToPath(import.meta.url))
const REPO = resolve(HERE, "../../..")
const CLI = join(REPO, "tools/norn-ts/src/cli.ts")
const STUDIO = resolve(HERE, "..")
const BUN = process.env.BUN_BIN ?? "bun"

export const devHost = process.env.NORN_DEV_HOST_BIN
if (process.env.CI && !devHost) throw new Error("the live workspace rows require NORN_DEV_HOST_BIN (core.sh browser builds it)")

async function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const server = createServer()
    server.once("error", fail)
    server.listen(0, "127.0.0.1", () => {
      const address = server.address()
      server.close(() => typeof address === "object" && address !== null ? done(address.port) : fail(new Error("no port")))
    })
  })
}

async function until<T>(what: string, read: () => T | undefined | Promise<T | undefined>, timeoutMs: number): Promise<T> {
  const deadline = Date.now() + timeoutMs
  for (;;) {
    const value = await read()
    if (value !== undefined) return value
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await new Promise((wake) => setTimeout(wake, 250))
  }
}

export interface Workspace {
  /** The project directory `norn init` laid out. */
  readonly dir: string
  /** The `ready` event of `norn dev` (its descriptor). */
  readonly ready: Record<string, any>
  /** The local Studio serving `actor` (the developer when omitted). */
  readonly studio: (actor?: string) => string
  /** The registry's own files: the definitions the rows build their assertions from. */
  readonly registry: (path: string) => any
  /** The pack artifacts the project carries, by artifact id. */
  readonly artifacts: ReadonlyMap<string, any>
  readonly stop: () => Promise<void>
}

/** Lay out, serve and seed `template`, with a local Studio for each of `as`. */
export async function packWorkspace(template: string, as: readonly string[]): Promise<Workspace> {
  const env = { ...process.env, NORN_DEV_HOST_BIN: devHost ?? "" }
  const root = mkdtempSync(join(tmpdir(), `studio-${template}-`))
  const dir = join(root, template)
  const children: ChildProcess[] = []
  const stop = async () => {
    await Promise.all(children.map((child) => new Promise<void>((done) => {
      if (child.exitCode !== null || child.signalCode !== null) return done()
      child.once("exit", () => done())
      child.kill("SIGTERM")
    })))
    rmSync(root, { recursive: true, force: true })
  }
  try {
    const init = spawnSync(BUN, [CLI, "init", dir, "--template", template, "--no-install", "--json"], { env, encoding: "utf8" })
    if (init.status !== 0) throw new Error(`norn init --template ${template}: ${init.stdout}${init.stderr}`)
    const dev = spawn(BUN, [CLI, "dev", "--json", "--port", "0"], { cwd: dir, env })
    children.push(dev)
    const events: Record<string, any>[] = []
    let buffer = ""
    let stderr = ""
    dev.stderr?.on("data", (chunk: Buffer) => { stderr = (stderr + chunk.toString()).slice(-4000) })
    dev.stdout?.on("data", (chunk: Buffer) => {
      buffer += chunk.toString()
      let newline: number
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline).trim()
        buffer = buffer.slice(newline + 1)
        if (line) events.push(JSON.parse(line))
      }
    })
    const ready = await until("norn dev to serve", () => {
      if (dev.exitCode !== null || events.some((event) => event.event === "fatal" || event.event === "rejected")) {
        throw new Error(`norn dev stopped: ${JSON.stringify(events)} ${stderr}`)
      }
      return events.find((event) => event.event === "ready")
    }, 240_000)
    const seeded = spawnSync(BUN, [CLI, "seed", "--json"], { cwd: dir, env, encoding: "utf8", timeout: 300_000 })
    if (seeded.status !== 0) throw new Error(`norn seed: ${seeded.stdout}${seeded.stderr}`)
    const studios = new Map<string, string>()
    for (const actor of ["developer", ...as]) {
      const port = await freePort()
      const server = spawn(BUN, [join(STUDIO, "scripts/local-cell.ts"), "--project", dir, "--port", String(port), "--as", actor], { cwd: STUDIO, env })
      children.push(server)
      const url = `http://127.0.0.1:${port}`
      await until(`local Studio as ${actor}`, async () => {
        if (server.exitCode !== null) throw new Error(`local Studio as ${actor} exited`)
        try { return (await fetch(`${url}/api/me`, { headers: { host: `127.0.0.1:${port}` } })).ok ? true : undefined } catch { return undefined }
      }, 30_000)
      studios.set(actor, url)
    }
    const artifacts = new Map<string, any>()
    for (const sourceDir of (JSON.parse(readFileSync(join(dir, "norn.json"), "utf8")).sources as string[])) {
      let files: string[] = []
      try { files = readdirSync(join(dir, sourceDir)) } catch { continue }
      for (const file of files.filter((name) => name.endsWith(".json"))) {
        const value = JSON.parse(readFileSync(join(dir, sourceDir, file), "utf8"))
        if (typeof value?.artifactId === "string") artifacts.set(value.artifactId, value)
      }
    }
    return {
      dir, ready, artifacts, stop,
      studio: (actor = "developer") => {
        const url = studios.get(actor)
        if (url === undefined) throw new Error(`no local Studio as ${actor}`)
        return url
      },
      registry: (path: string) => JSON.parse(readFileSync(join(REPO, "registry", path), "utf8")),
    }
  } catch (error) {
    await stop()
    throw error
  }
}

/** The English label a pack's labels artifacts give `target`, or undefined. */
export function packLabel(artifacts: ReadonlyMap<string, any>, matches: (target: Record<string, unknown>) => boolean, locale: "en" | "ar" = "en"): string | undefined {
  for (const artifact of artifacts.values()) {
    if (artifact.kind !== "labels") continue
    for (const entry of artifact.payload?.entries ?? []) if (matches(entry.target)) return entry.label?.[locale]
  }
  return undefined
}
