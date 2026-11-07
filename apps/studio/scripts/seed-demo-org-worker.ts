/** Operator-only Worker: Wrangler binds it to loopback and a remote D1 database. */
import { parseMembers, seedDemoOrg } from "./seed-demo-org-core.ts"

export default {
  async fetch(request: Request, env: { AUTH_DB: D1Database; SEED_TOKEN: string }): Promise<Response> {
    if (request.method !== "POST" || new URL(request.url).pathname !== "/seed") return new Response(null, { status: 404 })
    if (!env.SEED_TOKEN || request.headers.get("authorization") !== `Bearer ${env.SEED_TOKEN}`) return new Response(null, { status: 403 })
    try {
      const input = await request.json() as { members: { email: string; role: string }[] }
      const members = parseMembers(input.members.map(({ email, role }) => `${email}=${role}`))
      await seedDemoOrg(env.AUTH_DB, members, new Date())
      const result = await env.AUTH_DB.prepare(`select "user"."email" as email, "member"."role" as role from "member" join "user" on "user"."id" = "member"."userId" where "organizationId" = ?`).bind("org_demo").all()
      return Response.json(result.results)
    } catch (error) {
      return Response.json({ error: error instanceof Error ? error.message : "Provisioning failed" }, { status: 500 })
    }
  },
} satisfies ExportedHandler<{ AUTH_DB: D1Database; SEED_TOKEN: string }>
