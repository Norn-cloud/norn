/**
 * Seeds the Studio organization `org_demo` ("Norn demo") and its demo members
 * into a stage's auth database. The cell's demo data is scoped to `org_demo`,
 * and the host only accepts a token whose `norn.org` is the cell's org (#985),
 * so demo users need a real membership there.
 *
 * Idempotent: re-running changes nothing but the members' roles.
 * Preview only. Prod gets the organization when it is promoted.
 *
 *   bun scripts/seed-demo-org.ts --stage preview --member viewer@demo.norn.cloud=viewer --member analyst@demo.norn.cloud=admin
 *
 * Needs CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_EMAIL and CLOUDFLARE_API_KEY. The members must already have signed up.
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
        sql: `insert into "member" ("id", "organizationId", "userId", "role", "createdAt") select ?, ?, "id", ?, ? from "user" where "email" = ? and not exists (select 1 from "member" where "organizationId" = ? and "userId" = "user"."id")`,
        params: [`m_${DEMO_ORG.id}_${crypto.randomUUID()}`, DEMO_ORG.id, role, at, email, DEMO_ORG.id],
      },
      { sql: `update "member" set "role" = ? where "organizationId" = ? and "userId" = (select "id" from "user" where "email" = ?)`, params: [role, DEMO_ORG.id, email] },
    ]),
  ]
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
  const database = (found.result as { uuid: string; name: string }[]).find((db) => db.name === name)
  if (database === undefined) throw new Error(`no D1 database ${name}`)
  for (const { sql, params } of seedStatements(members, new Date())) {
    const result = await api(`/${database.uuid}/query`, { sql, params })
    if (!result.success) throw new Error(`seed failed: ${JSON.stringify(result.errors)}`)
  }
  const check = await api(`/${database.uuid}/query`, { sql: `select "user"."email" as email, "member"."role" as role from "member" join "user" on "user"."id" = "member"."userId" where "organizationId" = ?`, params: [DEMO_ORG.id] })
  console.log(`${DEMO_ORG.id} members:`, JSON.stringify((check.result as { results: unknown }[])[0]?.results))
}

if (import.meta.main) await main(process.argv.slice(2))
