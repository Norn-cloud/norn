import type { OrgRole } from "../worker/auth.ts"

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

/** Narrow structural view of the Worker D1 binding used by provisioning. */
export interface SeedDatabase {
  prepare(sql: string): { bind(...params: string[]): SeedStatement }
  batch(statements: SeedStatement[]): Promise<unknown>
}
export interface SeedStatement { all(): Promise<{ results: unknown[] }> }

/** Execute provisioning through the Worker D1 binding's atomic batch. */
export async function seedDemoOrg(db: SeedDatabase, members: readonly { readonly email: string; readonly role: OrgRole }[], now: Date): Promise<void> {
  await db.batch(seedStatements(members, now).map(({ sql, params }) => db.prepare(sql).bind(...params)))
}

/** `email=role` pairs, each role one the organization plugin knows. */
export function parseMembers(args: readonly string[]): { email: string; role: OrgRole }[] {
  return args.map((arg) => {
    const parts = arg.split("=")
    if (parts.length !== 2) throw new Error("member must be email=role")
    const [email = "", role = ""] = parts
    if (!/^[^@\s]+@[^@\s]+$/.test(email)) throw new Error(`not an email: ${email}`)
    if (!["admin", "member", "viewer"].includes(role)) throw new Error(`role must be admin, member or viewer, got ${role}`)
    return { email, role: role as OrgRole }
  })
}
