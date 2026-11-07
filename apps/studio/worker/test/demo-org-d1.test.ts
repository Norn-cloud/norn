import { Miniflare } from "miniflare"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { DEMO_ORG, parseMembers, seedDemoOrg } from "../../scripts/seed-demo-org-core.ts"

test("D1 binding rolls back an injected second grant failure and then provisions both members", async () => {
  const mf = new Miniflare({ modules: true, script: "export default { fetch() { return new Response('ok') } }", compatibilityDate: "2026-10-20", d1Databases: { DB: "00000000-0000-0000-0000-000000000001" } })
  try {
    const db = await mf.getD1Database("DB") as unknown as D1Database
    await db.exec(readFileSync(join(import.meta.dir, "../migrations/0001_better_auth.sql"), "utf8"))
    for (const [id, email] of [["a", "analyst@example.com"], ["v", "viewer@example.com"]] as const) {
      await db.prepare(`insert into "user" ("id", "name", "email", "emailVerified", "createdAt", "updatedAt") values (?, ?, ?, 1, ?, ?)`).bind(id, id, email, "2026-01-01", "2026-01-01").run()
    }
    await db.exec(`create trigger fail_second before insert on "member" when new."userId" = 'v' begin select raise(abort, 'INJECTED_SECOND_GRANT_FAILURE'); end;`)
    const members = parseMembers(["analyst@example.com=member", "viewer@example.com=viewer"])
    await expect(seedDemoOrg(db as unknown as import("../../scripts/seed-demo-org-core.ts").SeedDatabase, members, new Date())).rejects.toThrow("INJECTED_SECOND_GRANT_FAILURE")
    expect((await db.prepare(`select count(*) as n from "member" where "organizationId" = ?`).bind(DEMO_ORG.id).first<{ n: number }>())?.n).toBe(0)
    expect((await db.prepare(`select count(*) as n from "organization" where "id" = ?`).bind(DEMO_ORG.id).first<{ n: number }>())?.n).toBe(0)
    await db.exec(`drop trigger fail_second;`)
    await seedDemoOrg(db as unknown as import("../../scripts/seed-demo-org-core.ts").SeedDatabase, members, new Date())
    await seedDemoOrg(db as unknown as import("../../scripts/seed-demo-org-core.ts").SeedDatabase, members, new Date())
    const rows = await db.prepare(`select "user"."email" as email, "member"."role" as role from "member" join "user" on "user"."id" = "member"."userId" where "organizationId" = ? order by email`).bind(DEMO_ORG.id).all()
    expect(rows.results).toEqual([{ email: "analyst@example.com", role: "member" }, { email: "viewer@example.com", role: "viewer" }])
  } finally { await mf.dispose() }
}, 60_000)
