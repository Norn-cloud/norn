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
const ISSUES = { type: "object", properties: { repo: { type: "string" }, state: { enum: ["open", "closed"] }, limit: { type: "integer" } }, required: ["repo"] }
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
function stage(gatewayClient = false) {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  db.exec(migration("0002_connector_binding.sql"))
  const env: Env = {
    ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher,
    AUTH_DB: db as unknown as D1Database,
    BETTER_AUTH_SECRET: "t".repeat(40),
    BETTER_AUTH_URL: ORIGIN,
  }
  const auth = createAuth({ database: db, secret: env.BETTER_AUTH_SECRET, baseURL: ORIGIN, ...gatewayClient ? { gateway: { url: GATEWAY, clientId: "studio", clientSecret: "s".repeat(32) } } : {} })
  const calls: Request[] = []
  let answer: (request: Request) => Response = () => Response.json({})
  const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init)
    calls.push(request)
    const { pathname } = new URL(request.url)
    if (pathname === "/api/context") return Response.json({ organization: "gw-org", slug: "acme", role: "member", gitOrigins: [GATEWAY] })
    if (pathname === "/api/organizations/gw-org/apps/github/tools/list_issues") return Response.json({ name: "list_issues", inputSchema: ISSUES })
    return answer(request)
  }) as typeof fetch
  const deps: ConnectorDeps = { db: d1(db), grant: async () => TOKEN, fetch: fetcher }
  const send = async (path: string, init: RequestInit & { cookie?: string } = {}, overrides: Partial<ConnectorDeps> = {}, gateway: string | null = GATEWAY) => {
    const headers = new Headers(init.headers)
    if (init.cookie !== undefined) headers.set("cookie", init.cookie)
    if (init.method !== undefined && init.method !== "GET") headers.set("origin", ORIGIN)
    const response = await connectors(new Request(`${ORIGIN}/api/connectors${path}`, { ...init, headers }), { GATEWAY_URL: gateway ?? undefined, BETTER_AUTH_URL: ORIGIN }, auth, path, { ...deps, ...overrides })
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

const body = (method: string, value: unknown) => ({ method, headers: { "content-type": "application/json" }, body: JSON.stringify(value) })
const gatewayCalls = (calls: Request[], from: number) => calls.slice(from).filter((r) => new URL(r.url).pathname !== "/api/context").map((r) => `${r.method} ${new URL(r.url).pathname}`)

test("every route needs a signed-in person and names its refusal", async () => {
  const { send } = stage()
  expect(await send("/inventory")).toEqual({ status: 401, body: { code: "connectors.sign_in", error: "Sign in first." } })
})

test("gateway sign-in: status, connect, reconnect and disconnect", async () => {
  const { send, signUp } = stage(true)
  const owner = await signUp("ada@example.com")
  expect((await send("/status", { cookie: owner }, {}, null)).body).toEqual({ gateway: null, linked: false })
  expect((await send("/inventory", { cookie: owner }, {}, null)).body.code).toBe("connectors.not_configured")
  // The real grant lookup: nothing is linked yet, and a gateway call says how to fix it.
  expect((await send("/status", { cookie: owner }, { grant: undefined })).body).toEqual({ gateway: GATEWAY, linked: false })
  expect((await send("/inventory", { cookie: owner }, { grant: undefined })).body).toMatchObject({ code: "connectors.not_linked", action: { kind: "sign_in_gateway" } })
  for (let attempt = 0; attempt < 2; attempt++) {
    // Connecting and reconnecting are the same authorization-code link with PKCE, back to Connections.
    const url = new URL(String((await send("/link", { method: "POST", cookie: owner })).body.url))
    expect(`${url.origin}${url.pathname}`).toBe(`${GATEWAY}/api/auth/oauth2/authorize`)
    expect([url.searchParams.get("client_id"), url.searchParams.get("code_challenge_method")]).toEqual(["studio", "S256"])
    expect(url.searchParams.get("scope")?.split(" ")).toContain("executor")
    expect(url.searchParams.get("redirect_uri")).toBe(`${ORIGIN}/api/auth/callback/executor`)
  }
  expect((await send("/unlink", { method: "POST", cookie: owner }, { grant: undefined })).body).toEqual({ linked: false })
})

test("app accounts: view, connect, reconnect, rename and disconnect, all in the gateway", async () => {
  const { send, signUp, calls, answer } = stage()
  const owner = await signUp("ada@example.com")
  answer((request) => {
    const path = `${request.method} ${new URL(request.url).pathname}`
    if (path === "GET /api/organizations/gw-org/inventory") return Response.json({ apps: [{ id: "github", name: "GitHub" }], accounts: [{ id: "acc1", provider: "github", label: "Work", accessToken: "never" }], links: [{ app: "github", account: "acc1" }] })
    if (path === "GET /api/organizations/gw-org/apps/github/accounts/user") return Response.json([{ requirement: "github", account: "acc1", state: "connected" }])
    if (path.endsWith("/connections")) return Response.json({ url: `${GATEWAY}/connect/c1`, redirectUri: `${GATEWAY}/cb` })
    if (path === "GET /api/organizations/gw-org/accounts/acc1") return Response.json({ account: { id: "acc1", label: "Personal" }, provider: { name: "GitHub" }, apps: [{ id: "github" }], health: { status: "ok" } })
    return Response.json({})
  })
  const inventory = (await send("/inventory", { cookie: owner })).body
  expect(inventory).toEqual({ apps: [{ id: "github", name: "GitHub" }], accounts: [{ id: "acc1", provider: "github", label: "Work" }], links: [{ app: "github", account: "acc1" }] })
  expect((await send("/apps/github/accounts", { cookie: owner })).body).toEqual({ requirements: [{ requirement: "github", account: "acc1", state: "connected" }] })
  expect((await send("/accounts/acc1", { cookie: owner })).body).toEqual({ id: "acc1", provider: "GitHub", label: "Personal", apps: ["github"], health: "ok" })
  const from = calls.length
  expect((await send("/apps/github/accounts/github/connect", { ...body("POST", {}), cookie: owner })).body).toEqual({ url: `${GATEWAY}/connect/c1` })
  expect((await calls.at(-1)?.json()) as unknown).toEqual({ destination: { kind: "personal" } })
  await send("/apps/github/accounts/github/connect", { ...body("POST", { account: "acc1" }), cookie: owner })
  expect((await calls.at(-1)?.json()) as unknown).toEqual({ destination: { kind: "personal" }, account: "acc1" })
  expect((await send("/accounts/acc1", { ...body("PATCH", { label: "  Personal " }), cookie: owner })).body.label).toBe("Personal")
  expect((await send("/accounts/acc1", { ...body("PATCH", { label: "" }), cookie: owner })).body).toMatchObject({ code: "connectors.invalid_request", field: "label" })
  expect((await send("/accounts/acc1", { method: "DELETE", cookie: owner })).body).toEqual({ disconnected: "acc1" })
  expect(gatewayCalls(calls, from)).toEqual([
    "POST /api/organizations/gw-org/apps/github/accounts/user/github/connections",
    "POST /api/organizations/gw-org/apps/github/accounts/user/github/connections",
    "PATCH /api/organizations/gw-org/accounts/acc1",
    "GET /api/organizations/gw-org/accounts/acc1",
    "DELETE /api/organizations/gw-org/accounts/acc1",
  ])
})

test("tool bindings: an admin adds, edits and removes them; the mapping is checked against the tool's schema", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("owner@example.com")
  const added = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues", label: "Norn issues", inputMapping: { repo: { fixed: "norn" } } }), cookie: owner })
  const binding = added.body.binding as { id: string }
  expect(added.body.binding).toMatchObject({ app: "github", tool: "list_issues", label: "Norn issues", inputMapping: { repo: { fixed: "norn" } }, runRole: "member" })
  const edited = await send(`/bindings/${binding.id}`, { ...body("PATCH", { label: "Open Norn issues", inputMapping: { repo: { fixed: "norn" }, state: { fixed: "open" } }, runRole: "admin" }), cookie: owner })
  expect(edited.body.binding).toMatchObject({ id: binding.id, label: "Open Norn issues", runRole: "admin", inputMapping: { state: { fixed: "open" } } })
  expect((await send("/bindings", { cookie: owner })).body).toMatchObject({ manage: true, bindings: [{ id: binding.id, label: "Open Norn issues" }] })
  for (const [mapping, field] of [[{ nope: { fixed: 1 } }, "inputMapping.nope"], [{ limit: { fixed: "ten" } }, "inputMapping.limit"], [{ state: { fixed: "merged" } }, "inputMapping.state"]] as const) {
    expect((await send(`/bindings/${binding.id}`, { ...body("PATCH", { inputMapping: mapping }), cookie: owner })).body).toMatchObject({ code: "connectors.binding_invalid", field })
  }
  const member = await signUp("member@example.com", "member")
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: member })).body).toMatchObject({ code: "connectors.forbidden", action: { kind: "ask_admin" } })
  expect((await send(`/bindings/${binding.id}`, { method: "DELETE", cookie: member })).status).toBe(403)
  expect((await send(`/bindings/${binding.id}`, { method: "DELETE", cookie: owner })).body).toEqual({ removed: binding.id })
  expect((await send(`/bindings/${binding.id}`, { method: "DELETE", cookie: owner })).body.code).toBe("connectors.binding_not_found")
})

test("tools list with their schemas and the bindings that let them run", async () => {
  const { send, signUp, answer } = stage()
  const owner = await signUp("ada@example.com")
  const { body: added } = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner })
  answer(() => Response.json({ items: [{ name: "list_issues", description: "List issues", inputSchema: ISSUES }, { name: "delete_repo", inputSchema: { type: "object" } }] }))
  expect((await send("/apps/github/tools", { cookie: owner })).body.tools).toEqual([
    { app: "github", name: "list_issues", description: "List issues", inputSchema: ISSUES, outputSchema: null, bindings: [(added.binding as { id: string }).id] },
    { app: "github", name: "delete_repo", description: null, inputSchema: { type: "object" }, outputSchema: null, bindings: [] },
  ])
})

test("a test run goes through its binding: fixed fields win, a person cannot override them, roles are checked", async () => {
  const { send, signUp, calls, answer } = stage()
  const owner = await signUp("ada@example.com")
  const { body: added } = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues", inputMapping: { repo: { fixed: "norn" } } }), cookie: owner })
  const id = (added.binding as { id: string }).id
  answer(() => Response.json({ issues: [] }))
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: { state: "open" } }), cookie: owner })).body).toEqual({ binding: id, result: { issues: [] } })
  expect((await calls.at(-1)?.json()) as unknown).toEqual({ tool: "list_issues", input: { state: "open", repo: "norn" } })
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: { repo: "other" } }), cookie: owner })).body).toMatchObject({ code: "connectors.binding_invalid", field: "input.repo" })
  expect((await send("/bindings/missing/run", { ...body("POST", { input: {} }), cookie: owner })).body.code).toBe("connectors.binding_not_found")
  const viewer = await signUp("viewer@example.com", "viewer")
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: {} }), cookie: viewer })).body.code).toBe("connectors.forbidden")
})

test("expired sign-ins and missing scopes come back typed, with the fix", async () => {
  const { send, signUp, answer } = stage()
  const owner = await signUp("ada@example.com")
  const { body: added } = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner })
  const run = () => send(`/bindings/${(added.binding as { id: string }).id}/run`, { ...body("POST", { input: { repo: "n" } }), cookie: owner })
  const cases: [Response, Record<string, unknown>][] = [
    [Response.json({ _tag: "Unauthorized", reason: "expired" }, { status: 401 }), { code: "connectors.reauth", action: { kind: "sign_in_gateway" } }],
    [Response.json({ _tag: "Forbidden", reason: "insufficient_scope" }, { status: 403 }), { code: "connectors.missing_scope", action: { kind: "sign_in_gateway" } }],
    [Response.json({ _tag: "AppProviderFailed", accountId: "acc1", upstream: { code: "insufficient_scope" } }, { status: 403 }), { code: "connectors.account_missing_scope", action: { kind: "reconnect_account", app: "github", account: "acc1" } }],
    [Response.json({ _tag: "OAuthReconnectRequired", account: "acc1" }, { status: 409 }), { code: "connectors.account_reconnect", action: { kind: "reconnect_account", app: "github", account: "acc1" } }],
    [Response.json({ _tag: "AccountRequired", requirement: "github" }, { status: 409 }), { code: "connectors.account_required", action: { kind: "connect_account", app: "github", requirement: "github" } }],
    [Response.json({ _tag: "ToolApprovalRequired", message: "approve" }, { status: 409 }), { code: "connectors.approval_required" }],
    [Response.json({ _tag: "InputInvalid", message: "repo is required" }, { status: 400 }), { code: "connectors.input_invalid", error: "repo is required" }],
  ]
  for (const [response, expected] of cases) {
    answer(() => response.clone())
    expect((await run()).body).toMatchObject(expected)
  }
})

test("the Worker routes /api/connectors/ through the server-side actor", async () => {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  const env: Env = { ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, GATEWAY_URL: GATEWAY }
  const response = await worker.fetch(new Request(`${ORIGIN}/api/connectors/status`) as Parameters<typeof worker.fetch>[0], env)
  expect([response.status, ((await response.json()) as { code: string }).code]).toEqual([401, "connectors.sign_in"])
})
