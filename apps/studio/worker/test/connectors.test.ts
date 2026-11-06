import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createAuth } from "../auth.ts"
import { connectors, type ConnectorDeps } from "../connectors.ts"
import worker, { type Env } from "../index.ts"

const ORIGIN = "https://studio-next.norn.cloud"
const GATEWAY = "https://gateway.example"
const TOKEN = "gateway-grant-never-in-a-response"
const migration = (name: string): string => readFileSync(join(import.meta.dir, "../migrations", name), "utf8")

/** The D1 calls the policy table makes, over bun:sqlite. */
function d1(db: Database): D1Database {
  type Statement = { sql: string; args: unknown[] }
  const statement = (sql: string, args: unknown[] = []) => ({
    sql, args,
    bind: (...next: unknown[]) => statement(sql, next),
    all: async () => ({ results: db.query(sql).all(...(args as never[])) }),
    run: async () => { db.query(sql).run(...(args as never[])); return { success: true } },
  })
  const batch = async (list: Statement[]) => { db.transaction(() => { for (const s of list) db.query(s.sql).run(...(s.args as never[])) })(); return [] }
  return { prepare: (sql: string) => statement(sql), batch } as unknown as D1Database
}

let address = 0
function stage() {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  db.exec(migration("0002_connector_policy.sql"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
  }
  const auth = createAuth({ database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN })
  const calls: Request[] = []
  let answer: (request: Request) => Response = () => Response.json({})
  const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init)
    calls.push(request)
    if (new URL(request.url).pathname === "/api/context") return Response.json({ organization: "gw-org", slug: "acme", role: "member", gitOrigins: [GATEWAY] })
    return answer(request)
  }) as typeof fetch
  const deps: ConnectorDeps = { db: d1(db), grant: async () => TOKEN, fetch: fetcher }
  const send = async (path: string, init: RequestInit & { cookie?: string } = {}, overrides: Partial<ConnectorDeps> = {}, gateway: string | undefined = GATEWAY) => {
    const headers = new Headers(init.headers)
    if (init.cookie !== undefined) headers.set("cookie", init.cookie)
    if (init.method !== undefined && init.method !== "GET") headers.set("origin", ORIGIN)
    const response = await connectors(new Request(`${ORIGIN}/api/connectors${path}`, { ...init, headers }), { GATEWAY_URL: gateway, BETTER_AUTH_URL: ORIGIN }, auth, path, { ...deps, ...overrides })
    const text = await response.text()
    expect(text).not.toContain(TOKEN)
    return { status: response.status, body: JSON.parse(text) as Record<string, unknown> }
  }
  const signUp = async (email: string, role = "owner") => {
    const init = (body: unknown) => ({ method: "POST", headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": `198.51.100.${++address % 250}` }, body: JSON.stringify(body) })
    const call = (path: string, body: unknown) => worker.fetch(new Request(`${ORIGIN}${path}`, init(body)) as Parameters<typeof worker.fetch>[0], env)
    await call("/api/auth/sign-up/email", { email, password: "correct horse battery", name: "Ada" })
    const response = await call("/api/auth/sign-in/email", { email, password: "correct horse battery" })
    db.query(`UPDATE member SET role = ? WHERE userId = (SELECT id FROM user WHERE email = ?)`).run(role, email)
    return response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
  }
  return { send, signUp, calls, answer: (next: (request: Request) => Response) => { answer = next } }
}

const policyBody = (entries: unknown) => ({ method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ entries }) })
const callBody = (input: unknown) => ({ method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ input }) })

test("every route needs a signed-in person and names its refusal", async () => {
  const { send } = stage()
  expect(await send("/inventory")).toEqual({ status: 401, body: { code: "connectors.sign_in", error: "Sign in first." } })
})

test("only an organization admin sets which tools may be bound; the default is none", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("owner@example.com")
  expect((await send("/policy", { cookie: owner })).body).toEqual({ entries: [], editable: true })
  expect((await send("/policy", { ...policyBody([{ app: "github", tool: "*" }]), cookie: owner })).body.entries).toEqual([{ app: "github", tool: "*" }])
  expect((await send("/policy", { ...policyBody([{ app: "../x", tool: "a" }]), cookie: owner })).body.code).toBe("connectors.invalid_request")
  const member = await signUp("member@example.com", "member")
  const refused = await send("/policy", { ...policyBody([]), cookie: member })
  expect([refused.status, refused.body.code]).toEqual([403, "connectors.forbidden"])
})

test("a stage without a gateway says so, and an unlinked person is asked to sign in to it", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("ada@example.com")
  expect((await send("/status", { cookie: owner }, {}, undefined)).body).toEqual({ gateway: null, linked: false })
  expect((await send("/inventory", { cookie: owner }, {}, undefined)).body.code).toBe("connectors.not_configured")
  expect((await send("/inventory", { cookie: owner }, { grant: async () => null })).body.code).toBe("connectors.not_linked")
})

test("tools list with their schemas and whether policy lets them be bound", async () => {
  const { send, signUp, calls, answer } = stage()
  const owner = await signUp("ada@example.com")
  await send("/policy", { ...policyBody([{ app: "github", tool: "list_issues" }]), cookie: owner })
  const inputSchema = { type: "object", properties: { repo: { type: "string" } }, required: ["repo"] }
  answer(() => Response.json({ items: [{ name: "list_issues", description: "List issues", inputSchema }, { name: "delete_repo", inputSchema: { type: "object" } }] }))
  const { body } = await send("/apps/github/tools", { cookie: owner })
  expect(body.tools).toEqual([
    { app: "github", name: "list_issues", description: "List issues", inputSchema, outputSchema: null, bindable: true },
    { app: "github", name: "delete_repo", description: null, inputSchema: { type: "object" }, outputSchema: null, bindable: false },
  ])
  const listed = calls.at(-1)
  expect(new URL(listed?.url ?? "").pathname).toBe("/api/organizations/gw-org/apps/github/tools")
  expect(listed?.headers.get("authorization")).toBe(`Bearer ${TOKEN}`)
})

test("a call runs only when policy allows the tool and the role may run tools", async () => {
  const { send, signUp, calls, answer } = stage()
  const owner = await signUp("ada@example.com")
  await send("/policy", { ...policyBody([{ app: "github", tool: "list_issues" }]), cookie: owner })
  answer(() => Response.json({ issues: [] }))
  const before = calls.length
  const blocked = await send("/apps/github/tools/delete_repo/call", { ...callBody({}), cookie: owner })
  expect([blocked.status, blocked.body.code]).toEqual([403, "connectors.tool_not_allowed"])
  expect(calls.slice(before).some((request) => request.url.endsWith("/tools/call"))).toBe(false)
  const ran = await send("/apps/github/tools/list_issues/call", { ...callBody({ repo: "norn" }), cookie: owner })
  expect(ran.body).toEqual({ app: "github", tool: "list_issues", result: { issues: [] } })
  expect(await calls.at(-1)?.json()).toEqual({ tool: "list_issues", input: { repo: "norn" } })
  const viewer = await signUp("viewer@example.com", "viewer")
  expect((await send("/apps/github/tools/list_issues/call", { ...callBody({}), cookie: viewer })).body.code).toBe("connectors.forbidden")
})

test("gateway refusals come back typed", async () => {
  const { send, signUp, answer } = stage()
  const owner = await signUp("ada@example.com")
  await send("/policy", { ...policyBody([{ app: "github", tool: "*" }]), cookie: owner })
  answer(() => Response.json({ _tag: "ToolApprovalRequired", message: "approve" }, { status: 409 }))
  expect((await send("/apps/github/tools/merge/call", { ...callBody({}), cookie: owner })).body.code).toBe("connectors.approval_required")
  answer(() => Response.json({ _tag: "InputInvalid", message: "repo is required" }, { status: 400 }))
  expect((await send("/apps/github/tools/merge/call", { ...callBody({}), cookie: owner })).body).toEqual({ code: "connectors.input_invalid", error: "repo is required" })
})

test("the Worker routes /api/connectors/ through the server-side actor", async () => {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  const env: Env = { ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, GATEWAY_URL: GATEWAY }
  const response = await worker.fetch(new Request(`${ORIGIN}/api/connectors/status`) as Parameters<typeof worker.fetch>[0], env)
  expect([response.status, ((await response.json()) as { code: string }).code]).toEqual([401, "connectors.sign_in"])
})
