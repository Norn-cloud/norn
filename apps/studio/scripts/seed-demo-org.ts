/**
 * Seeds the Studio organization `org_demo` ("Norn demo") and its demo members
 * into a stage's auth database. The cell's demo data is scoped to `org_demo`,
 * and the host only accepts a token whose `norn.org` is the cell's org (#985),
 * so demo users need a real membership there.
 *
 * Idempotent: re-running changes nothing but the members' roles.
 * Preview only. Prod gets the organization when it is promoted.
 *
 *   bun scripts/seed-demo-org.ts --stage preview --member viewer@demo.norn.cloud=viewer --member analyst@demo.norn.cloud=member
 *
 * Uses a loopback-only Wrangler Worker and remote D1 binding. Needs CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_EMAIL and CLOUDFLARE_API_KEY. The members must already have signed up and verified their email addresses.
 */
import { DEMO_ORG, parseMembers } from "./seed-demo-org-core.ts"
export { DEMO_ORG, parseMembers, seedDemoOrg, seedStatements } from "./seed-demo-org-core.ts"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { createServer } from "node:net"
import { homedir } from "node:os"
import { join, resolve } from "node:path"

/** Exit codes for operator-visible provisioning failures. */
export const SEED_EXIT = { timeout: 124, refused: 125, wranglerExited: 126 } as const
export class SeedFailure extends Error {
  constructor(message: string, readonly exitCode: number) { super(message) }
}
const wranglerExit = (code: number) => new SeedFailure(`Wrangler exited (exit ${code})`, SEED_EXIT.wranglerExited)
const timedOut = (error: unknown) => error instanceof Error && (error.name === "TimeoutError" || error.name === "AbortError")

/** A deadline whose timer and any pending request can be cancelled. */
function deadline(ms: number, parent?: AbortSignal) {
  const controller = new AbortController()
  const abortParent = () => controller.abort(parent?.reason)
  if (parent?.aborted) abortParent()
  else parent?.addEventListener("abort", abortParent, { once: true })
  const timer = setTimeout(() => controller.abort(new DOMException("Deadline exceeded", "TimeoutError")), ms)
  return { signal: controller.signal, dispose: () => { clearTimeout(timer); parent?.removeEventListener("abort", abortParent); controller.abort() } }
}

/** Wait for the local Worker or report Wrangler's exit immediately. */
export async function waitForWorker(exited: Promise<number>, url: string, token: string, timeoutMs = 60_000, parent?: AbortSignal): Promise<void> {
  const failure = exited.then((code) => ({ kind: "exited" as const, code }))
  const end = Date.now() + timeoutMs
  while (Date.now() < end) {
    const limit = deadline(Math.max(1, Math.min(1_000, end - Date.now())), parent)
    let result: { kind: "ready"; response: Response } | { kind: "retry" } | { kind: "exited"; code: number }
    try {
      const attempt = fetch(url, { headers: { authorization: `Bearer ${token}` }, signal: limit.signal })
        .then((response) => ({ kind: "ready" as const, response }), () => ({ kind: "retry" as const }))
      result = await Promise.race([attempt, failure])
    } finally { limit.dispose() }
    if (parent?.aborted) throw parent.reason
    if (result.kind === "exited") throw wranglerExit(result.code)
    if (result.kind === "ready" && result.response.status === 204) return
    if (result.kind === "ready" && result.response.status !== 503) throw new SeedFailure(`Readiness check refused (${result.response.status})`, SEED_EXIT.refused)
    let timer: ReturnType<typeof setTimeout> | undefined
    let abortPause: (() => void) | undefined
    try {
      const cancelled = new Promise<never>((_resolve, reject) => {
        abortPause = () => reject(parent?.reason)
        if (parent?.aborted) abortPause()
        else parent?.addEventListener("abort", abortPause, { once: true })
      })
      const pause = await Promise.race([new Promise<null>((resolve) => { timer = setTimeout(() => resolve(null), Math.min(200, Math.max(1, end - Date.now()))) }), failure, cancelled])
      if (pause) throw wranglerExit(pause.code)
    } finally { if (timer !== undefined) clearTimeout(timer); if (abortPause) parent?.removeEventListener("abort", abortPause) }
  }
  throw new SeedFailure("Local provisioning Worker readiness timed out", SEED_EXIT.timeout)
}

/** Check readiness without writes, then submit the seed exactly once. */
export async function seedWhenReady(exited: Promise<number>, base: string, token: string, members: readonly { email: string; role: string }[], timeouts = { readiness: 60_000, seed: 30_000 }, parent?: AbortSignal): Promise<Response> {
  await waitForWorker(exited, `${base}/ready`, token, timeouts.readiness, parent)
  const failure = exited.then((code) => ({ kind: "exited" as const, code }))
  const limit = deadline(timeouts.seed, parent)
  try {
    const attempt = fetch(`${base}/seed`, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify({ members }), signal: limit.signal })
      .then(async (response) => ({ kind: "response" as const, status: response.status, text: await response.text() }))
      .catch((error: unknown) => ({ kind: "error" as const, error }))
    const result = await Promise.race([attempt, failure])
    if (parent?.aborted) throw parent.reason
    if (result.kind === "exited") throw wranglerExit(result.code)
    if (result.kind === "error") throw new SeedFailure(timedOut(result.error) ? "Seed request timed out" : "Seed request failed", timedOut(result.error) ? SEED_EXIT.timeout : SEED_EXIT.refused)
    if (result.status < 200 || result.status >= 300) throw new SeedFailure(`Seed request refused (${result.status}): ${result.text}`, SEED_EXIT.refused)
    return new Response(result.text, { status: result.status })
  } finally { limit.dispose() }
}

/** Look up D1 through the control plane under a cancellable deadline. */
export async function discoverD1(url: string, headers: Record<string, string>, timeoutMs = 15_000): Promise<{ success: boolean; result: unknown; errors: unknown }> {
  const limit = deadline(timeoutMs)
  try {
    const response = await fetch(url, { headers, signal: limit.signal })
    return await response.json() as { success: boolean; result: unknown; errors: unknown }
  } catch (error) {
    throw new SeedFailure(timedOut(error) ? "D1 discovery timed out" : "D1 discovery failed", timedOut(error) ? SEED_EXIT.timeout : SEED_EXIT.refused)
  } finally { limit.dispose() }
}

/** Reap Wrangler on success, error, SIGINT and SIGTERM before returning. */
export async function withWrangler<T>(child: { readonly exited: Promise<number>; readonly exitCode: number | null; readonly signalCode: string | null; kill(signal?: number): void }, action: (signal: AbortSignal) => Promise<T>): Promise<T> {
  const controller = new AbortController()
  let interrupt!: (error: SeedFailure) => void
  const stopped = new Promise<never>((_resolve, reject) => { interrupt = reject })
  const sigint = () => interrupt(new SeedFailure("Interrupted by SIGINT", 130))
  const sigterm = () => interrupt(new SeedFailure("Interrupted by SIGTERM", 143))
  process.on("SIGINT", sigint); process.on("SIGTERM", sigterm)
  try { return await Promise.race([action(controller.signal), stopped]) }
  finally {
    controller.abort()
    try {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill()
        let timer: ReturnType<typeof setTimeout> | undefined
        try { await Promise.race([child.exited, new Promise<void>((resolve) => { timer = setTimeout(resolve, 2_000) })]) }
        finally { if (timer !== undefined) clearTimeout(timer) }
        if (child.exitCode === null && child.signalCode === null) child.kill(9)
      }
      await child.exited
    } finally { process.removeListener("SIGINT", sigint); process.removeListener("SIGTERM", sigterm) }
  }
}

async function main(argv: readonly string[]) {
  const stage = argv[argv.indexOf("--stage") + 1]
  if (argv.indexOf("--stage") < 0 || stage !== "preview") throw new Error("--stage preview is required; prod gets org_demo only on promotion")
  const members = parseMembers(argv.flatMap((arg, index) => argv[index - 1] === "--member" ? [arg] : []))
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_EMAIL: email, CLOUDFLARE_API_KEY: key } = process.env
  if (!account || !email || !key) throw new Error("CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_EMAIL and CLOUDFLARE_API_KEY are required")
  const api = (path: string) => discoverD1(`https://api.cloudflare.com/client/v4/accounts/${account}/d1/database${path}`, { "x-auth-email": email, "x-auth-key": key, "content-type": "application/json" })
  const name = `norn-studio-auth-${stage}`
  const found = await api(`?name=${name}`)
  if (!found.success) throw new SeedFailure(`could not list D1 databases: ${JSON.stringify(found.errors)}`, SEED_EXIT.refused)
  const database = (found.result as { uuid: string; name: string }[]).find((db) => db.name === name)
  if (database === undefined) throw new SeedFailure(`no D1 database ${name}`, SEED_EXIT.refused)
  const root = join(homedir(), ".cache", "agent-scratch")
  await import("node:fs/promises").then(({ mkdir }) => mkdir(root, { recursive: true }))
  const dir = await mkdtemp(join(root, "studio-demo-seed-"))
  const listener = createServer()
  await new Promise<void>((resolveListen, reject) => listener.once("error", reject).listen(0, "127.0.0.1", resolveListen))
  const address = listener.address()
  if (!address || typeof address === "string") throw new Error("Could not reserve a loopback port")
  const port = address.port
  await new Promise<void>((done, reject) => listener.close((error) => error ? reject(error) : done()))
  const token = crypto.randomUUID() + crypto.randomUUID()
  const config = { name: "norn-demo-seed-operator", main: resolve(import.meta.dir, "seed-demo-org-worker.ts"), compatibility_date: "2026-10-20", compatibility_flags: ["nodejs_compat"], d1_databases: [{ binding: "AUTH_DB", database_name: name, database_id: database.uuid, remote: true }] }
  const configPath = join(dir, "wrangler.json")
  try {
    await writeFile(configPath, JSON.stringify(config), { mode: 0o600 })
    await writeFile(join(dir, ".dev.vars"), `SEED_TOKEN=${JSON.stringify(token)}\n`, { mode: 0o600 })
    const child = Bun.spawn([resolve(import.meta.dir, "../node_modules/.bin/wrangler"), "dev", "--config", configPath, "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", "0"], { stdout: "ignore", stderr: "ignore", env: process.env })
    await withWrangler(child, async (signal) => {
      const response = await seedWhenReady(child.exited, `http://127.0.0.1:${port}`, token, members, undefined, signal)
      let body: unknown
      try { body = await response.json() }
      catch (error) { throw new SeedFailure(timedOut(error) ? "Seed response timed out" : "Seed response was invalid", timedOut(error) ? SEED_EXIT.timeout : SEED_EXIT.refused) }
      console.log(`${DEMO_ORG.id} members:`, JSON.stringify(body))
    })
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
}

if (import.meta.main) {
  try { await main(process.argv.slice(2)) }
  catch (error) { console.error(error instanceof Error ? error.message : "Provisioning failed"); process.exitCode = error instanceof SeedFailure ? error.exitCode : 1 }
}
