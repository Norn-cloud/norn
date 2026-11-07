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

/** Wait for the local Worker or report Wrangler's exit immediately. */
export async function waitForWorker(exited: Promise<number>, url: string, token: string, members: readonly { email: string; role: string }[]): Promise<Response> {
  const failure = exited.then((code) => ({ kind: "exited" as const, code }))
  const deadline = Date.now() + 60_000
  while (Date.now() < deadline) {
    const attempt = fetch(url, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify({ members }), signal: AbortSignal.timeout(1_000) })
      .then((response) => ({ kind: "ready" as const, response }), () => ({ kind: "retry" as const }))
    const result = await Promise.race([attempt, failure])
    if (result.kind === "exited") throw new Error(`Wrangler exited before the provisioning Worker was ready (exit ${result.code})`)
    if (result.kind === "ready") return result.response
    const pause = await Promise.race([Bun.sleep(200).then(() => null), failure])
    if (pause) throw new Error(`Wrangler exited before the provisioning Worker was ready (exit ${pause.code})`)
  }
  throw new Error("Local provisioning Worker did not start")
}

async function main(argv: readonly string[]) {
  const stage = argv[argv.indexOf("--stage") + 1]
  if (argv.indexOf("--stage") < 0 || stage !== "preview") throw new Error("--stage preview is required; prod gets org_demo only on promotion")
  const members = parseMembers(argv.flatMap((arg, index) => argv[index - 1] === "--member" ? [arg] : []))
  const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_EMAIL: email, CLOUDFLARE_API_KEY: key } = process.env
  if (!account || !email || !key) throw new Error("CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_EMAIL and CLOUDFLARE_API_KEY are required")
  const api = (path: string, body?: unknown) => fetch(`https://api.cloudflare.com/client/v4/accounts/${account}/d1/database${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { "x-auth-email": email, "x-auth-key": key, "content-type": "application/json" },
    ...body === undefined ? {} : { body: JSON.stringify(body) },
  }).then(async (response) => (await response.json()) as { success: boolean; result: unknown; errors: unknown })
  const name = `norn-studio-auth-${stage}`
  const found = await api(`?name=${name}`)
  if (!found.success) throw new Error(`could not list D1 databases: ${JSON.stringify(found.errors)}`)
  const database = (found.result as { uuid: string; name: string }[]).find((db) => db.name === name)
  if (database === undefined) throw new Error(`no D1 database ${name}`)
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
  let child: ReturnType<typeof Bun.spawn> | undefined
  try {
    await writeFile(configPath, JSON.stringify(config), { mode: 0o600 })
    await writeFile(join(dir, ".dev.vars"), `SEED_TOKEN=${JSON.stringify(token)}\n`, { mode: 0o600 })
    child = Bun.spawn([resolve(import.meta.dir, "../node_modules/.bin/wrangler"), "dev", "--config", configPath, "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", "0"], { stdout: "ignore", stderr: "ignore", env: process.env })
    const response = await waitForWorker(child.exited, `http://127.0.0.1:${port}/seed`, token, members)
    const body: unknown = await response.json()
    if (!response.ok) throw new Error(`Provisioning failed: ${JSON.stringify(body)}`)
    console.log(`${DEMO_ORG.id} members:`, JSON.stringify(body))
  } finally {
    if (child) { child.kill(); await child.exited }
    await rm(dir, { recursive: true, force: true })
  }
}

if (import.meta.main) await main(process.argv.slice(2))
