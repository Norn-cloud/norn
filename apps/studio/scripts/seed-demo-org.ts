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

/** Wait for the local Worker or report Wrangler's exit immediately. */
export async function waitForWorker(exited: Promise<number>, url: string, token: string, timeoutMs = 60_000): Promise<void> {
  const failure = exited.then((code) => ({ kind: "exited" as const, code }))
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const attempt = fetch(url, { headers: { authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(Math.max(1, Math.min(1_000, deadline - Date.now()))) })
      .then((response) => ({ kind: "ready" as const, response }), () => ({ kind: "retry" as const }))
    const result = await Promise.race([attempt, failure])
    if (result.kind === "exited") throw wranglerExit(result.code)
    if (result.kind === "ready" && result.response.status === 204) return
    if (result.kind === "ready" && result.response.status !== 503) throw new SeedFailure(`Readiness check refused (${result.response.status})`, SEED_EXIT.refused)
    const pause = await Promise.race([Bun.sleep(Math.min(200, Math.max(1, deadline - Date.now()))).then(() => null), failure])
    if (pause) throw wranglerExit(pause.code)
  }
  throw new SeedFailure("Local provisioning Worker readiness timed out", SEED_EXIT.timeout)
}

/** Check readiness without writes, then submit the seed exactly once. */
export async function seedWhenReady(exited: Promise<number>, base: string, token: string, members: readonly { email: string; role: string }[], timeouts = { readiness: 60_000, seed: 30_000 }): Promise<Response> {
  await waitForWorker(exited, `${base}/ready`, token, timeouts.readiness)
  const failure = exited.then((code) => ({ kind: "exited" as const, code }))
  const attempt = fetch(`${base}/seed`, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify({ members }), signal: AbortSignal.timeout(timeouts.seed) })
    .then((response) => ({ kind: "response" as const, response }), (error: unknown) => ({ kind: "error" as const, error }))
  const result = await Promise.race([attempt, failure])
  if (result.kind === "exited") throw wranglerExit(result.code)
  if (result.kind === "error") throw new SeedFailure(timedOut(result.error) ? "Seed request timed out" : "Seed request failed", timedOut(result.error) ? SEED_EXIT.timeout : SEED_EXIT.refused)
  if (!result.response.ok) {
    let detail: string
    try { detail = await result.response.text() }
    catch (error) { throw new SeedFailure(timedOut(error) ? "Seed response timed out" : "Seed response failed", timedOut(error) ? SEED_EXIT.timeout : SEED_EXIT.refused) }
    throw new SeedFailure(`Seed request refused (${result.response.status}): ${detail}`, SEED_EXIT.refused)
  }
  return result.response
}

/** Always reap the operator Worker, including on errors and early exits. */
export async function withWrangler<T>(child: { readonly exited: Promise<number>; readonly exitCode: number | null; readonly signalCode: string | null; kill(signal?: number): void }, action: () => Promise<T>): Promise<T> {
  try { return await action() }
  finally {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill()
      await Promise.race([child.exited, Bun.sleep(2_000)])
      if (child.exitCode === null && child.signalCode === null) child.kill(9)
    }
    await child.exited
  }
}

async function main(argv: readonly string[]) {
  const stage = argv[argv.indexOf("--stage") + 1]
  if (argv.indexOf("--stage") < 0 || stage !== "preview") throw new Error("--stage preview is required; prod gets org_demo only on promotion")
  const members = parseMembers(argv.flatMap((arg, index) => argv[index - 1] === "--member" ? [arg] : []))
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_EMAIL: email, CLOUDFLARE_API_KEY: key } = process.env
  if (!account || !email || !key) throw new Error("CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_EMAIL and CLOUDFLARE_API_KEY are required")
  const api = async (path: string) => {
    try {
      const response = await fetch(`https://api.cloudflare.com/client/v4/accounts/${account}/d1/database${path}`, {
        headers: { "x-auth-email": email, "x-auth-key": key, "content-type": "application/json" }, signal: AbortSignal.timeout(15_000),
      })
      return await response.json() as { success: boolean; result: unknown; errors: unknown }
    } catch (error) {
      throw new SeedFailure(timedOut(error) ? "D1 discovery timed out" : "D1 discovery failed", timedOut(error) ? SEED_EXIT.timeout : SEED_EXIT.refused)
    }
  }
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
    await withWrangler(child, async () => {
      const response = await seedWhenReady(child.exited, `http://127.0.0.1:${port}`, token, members)
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
