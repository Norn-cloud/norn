/** Real-D1 rollback regression; creates and deletes an isolated database. */
import { readFile } from "node:fs/promises"
import { DEMO_ORG, executeSeedBatch, parseMembers, seedDemoOrg } from "./seed-demo-org.ts"

const { CLOUDFLARE_ACCOUNT_ID: account, CLOUDFLARE_EMAIL: email, CLOUDFLARE_API_KEY: key } = process.env
if (!account || !email || !key) throw new Error("Cloudflare credentials are required")
const base = `https://api.cloudflare.com/client/v4/accounts/${account}/d1/database`
const headers = { "x-auth-email": email, "x-auth-key": key, "content-type": "application/json" }
async function request(path: string, body?: unknown, method = "POST") {
  const response = await fetch(base + path, { method, headers, ...body === undefined ? {} : { body: JSON.stringify(body) } })
  return await response.json() as { success: boolean; result: unknown; errors: unknown }
}
const created = await request("", { name: `studio-seed-regression-${crypto.randomUUID()}` })
if (!created.success) throw new Error(`D1 test database creation failed: ${JSON.stringify(created.errors)}`)
const databaseId = (created.result as { uuid: string }).uuid
try {
  const query = (sql: string, params: string[] = []) => request(`/${databaseId}/query`, { sql, params })
  const schema = await readFile(new URL("../worker/migrations/0001_better_auth.sql", import.meta.url), "utf8")
  if (!(await query(schema)).success) throw new Error("D1 schema setup failed")
  for (const [id, address] of [["a", "analyst@example.com"], ["v", "viewer@example.com"]] as const) {
    if (!(await query(`insert into "user" ("id", "name", "email", "emailVerified", "createdAt", "updatedAt") values (?, ?, ?, 1, ?, ?)`, [id, id, address, "2026-01-01", "2026-01-01"])).success) throw new Error("D1 test user setup failed")
  }
  if (!(await query(`create trigger fail_second before insert on "member" when new."userId" = 'v' begin select raise(abort, 'injected failure'); end`)).success) throw new Error("D1 trigger setup failed")
  const members = parseMembers(["analyst@example.com=member", "viewer@example.com=viewer"])
  let failed = false
  try { await seedDemoOrg(members, new Date(), (batch) => executeSeedBatch(request, databaseId, batch)) } catch { failed = true }
  if (!failed) throw new Error("Expected the second membership to fail")
  const check = await query(`select (select count(*) from "member" where "organizationId" = ?) as members, (select count(*) from "organization" where "id" = ?) as organizations`, [DEMO_ORG.id, DEMO_ORG.id])
  const rows = (check.result as { results: { members: number; organizations: number }[] }[])?.[0]?.results
  if (!check.success || rows?.[0]?.members !== 0 || rows[0].organizations !== 0) throw new Error("Real D1 left partial provisioning writes")
  console.log("Real D1: later failure rolled back both the first grant and organization")
} finally {
  const deleted = await request(`/${databaseId}`, undefined, "DELETE")
  if (!deleted.success) throw new Error(`Delete the disposable D1 test database: ${databaseId}`)
}
