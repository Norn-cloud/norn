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
 * Needs CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_EMAIL and CLOUDFLARE_API_KEY. The members must already have signed up and verified their email addresses.
 */
import { ORG_ROLES, type OrgRole } from "../worker/auth.ts"

export const DEMO_ORG = { id: "org_demo", name: "Norn demo", slug: "norn-demo" } as const

export interface Statement {
  readonly sql: string
  readonly params: readonly string[]
}

/** The statements, in order: the organization once, then each member once, with its role kept current. */
export function seedStatements(members: readonly { readonly email: string; readonly role: OrgRole }[], now: Date): readonly Statement[] {
  const at = now.toISOString()
  return [
    { sql: `insert into "organization" ("id", "name", "slug", "createdAt") values (?, ?, ?, ?) on conflict ("id") do nothing`, params: [DEMO_ORG.id, DEMO_ORG.name, DEMO_ORG.slug, at] },
    ...members.flatMap(({ email, role }) => [
      {
        sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select ?, ?, "id", ?, ? from "user" where "email" = ? and "emailVerified" = 1 and not exists (select 1 from "member" where "organizationId" = ? and "userId" = "user"."id")`,
        params: [`m_${DEMO_ORG.id}_${crypto.randomUUID()}`, DEMO_ORG.id, role, at, email, DEMO_ORG.id],
      },
      { sql: `update "member" set "role" = ? where "organizationId" = ? and "userId" = (select "id" from "user" where "email" = ? and "emailVerified" = 1)`, params: [role, DEMO_ORG.id, email] },
      // A failed assertion violates NOT NULL and rolls the entire D1 batch back.
      { sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select null, null, null, null, null where not exists (select 1 from "member" join "user" on "user"."id" = "member"."userId" where "member"."organizationId" = ? and "member"."role" = ? and "user"."email" = ? and "user"."emailVerified" = 1)`, params: [DEMO_ORG.id, role, email] },
    ]),
  ]
}

/** Run the organization and all guarded memberships as one atomic D1 batch. */
export async function seedDemoOrg(
  members: readonly { readonly email: string; readonly role: OrgRole }[],
  now: Date,
  batch: (statements: readonly Statement[]) => Promise<void>,
): Promise<void> {
  try { await batch(seedStatements(members, now)) }
  catch (error) { throw new Error("Demo provisioning failed or its result is unknown; check the D1 error and ensure every account has a verified email.", { cause: error }) }
}

/** The same REST batch path used by the seed and its real-D1 regression. */
export async function executeSeedBatch(api: (path: string, body?: unknown) => Promise<{ success: boolean; result: unknown; errors: unknown }>, databaseId: string, batch: readonly Statement[]): Promise<void> {
  const result = await api(`/${databaseId}/query`, { batch })
  if (!result.success || !Array.isArray(result.result) || result.result.length !== batch.length || result.result.some((row: { success?: boolean }) => row.success !== true)) {
    throw new Error(`seed failed: ${JSON.stringify(result.errors)}`)
  }
}

/** `email=role` pairs, each role one the organization plugin knows. */
export function parseMembers(args: readonly string[]): { email: string; role: OrgRole }[] {
  return args.map((arg) => {
    const [email = "", role = ""] = arg.split("=")
    if (!/^[^@\s]+@[^@\s]+$/.test(email)) throw new Error(`not an email: ${email}`)
    if (!(role in ORG_ROLES) || role === "owner") throw new Error(`role must be admin, member or viewer, got ${role}`)
    return { email, role: role as OrgRole }
  })
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
  await seedDemoOrg(members, new Date(), (batch) => executeSeedBatch(api, database.uuid, batch))
  const check = await api(`/${database.uuid}/query`, { sql: `select "user"."email" as email, "member"."role" as role from "member" join "user" on "user"."id" = "member"."userId" where "organizationId" = ?`, params: [DEMO_ORG.id] })
  const stored = (check.result as { results?: unknown }[] | null)?.[0]?.results
  if (!check.success || stored === undefined) throw new Error(`could not read ${DEMO_ORG.id}'s members back: ${JSON.stringify(check.errors)}`)
  console.log(`${DEMO_ORG.id} members:`, JSON.stringify(stored))
}

if (import.meta.main) await main(process.argv.slice(2))
