import { Database } from "bun:sqlite"
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createAuth, gatewaySubject } from "../auth.ts"
import { connectors, type ConnectorDeps } from "../connectors.ts"
import { canonicalJson, connectorVersion, manifestDigest, readDraft, runInput, toolManifest } from "../bindings.ts"
import { Gateway } from "../gateway.ts"
import { cellScope, scopeQuery } from "../scope.ts"
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
  db.exec(migration("0002_workspace_provisioning.sql"))
  db.exec(migration("0002_connector_binding.sql"))
  db.exec(migration("0003_connector_binding_manifest.sql"))
  db.exec(migration("0004_connector_binding_definition.sql"))
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
    if (pathname === "/api/organizations/gw-org/apps/github/tools/list_issues") return Response.json({ name: "list_issues", inputSchema: ISSUES, version: "dpl_1" })
    return answer(request)
  }) as typeof fetch
  const deps: ConnectorDeps = { db: d1(db), grant: async () => TOKEN, fetch: fetcher }
  const send = async (path: string, init: RequestInit & { cookie?: string } = {}, overrides: Partial<ConnectorDeps> = {}, gateway: string | null = GATEWAY) => {
    const headers = new Headers(init.headers)
    if (init.cookie !== undefined) headers.set("cookie", init.cookie)
    if (init.method !== undefined && init.method !== "GET") headers.set("origin", ORIGIN)
    const response = await connectors(new Request(`${ORIGIN}/api/connectors${path}`, { ...init, headers }), { TOOL_GATEWAY_URL: gateway ?? undefined, BETTER_AUTH_URL: ORIGIN }, auth, path, { ...deps, ...overrides })
    const text = await response.text()
    expect(text).not.toContain(TOKEN)
    return { status: response.status, body: JSON.parse(text) as Record<string, unknown>, cookies: response.headers.getSetCookie() }
  }
  /** A new person; with `colleagueOf`, a member of that person's organization instead of their own. */
  const signUp = async (email: string, role = "owner", colleagueOf?: string, environment = true) => {
    const init = (body: unknown) => ({ method: "POST", headers: { "content-type": "application/json", origin: ORIGIN, "cf-connecting-ip": `198.51.100.${++address % 250}` }, body: JSON.stringify(body) })
    const call = (path: string, body: unknown) => worker.fetch(new Request(`${ORIGIN}${path}`, init(body)) as Parameters<typeof worker.fetch>[0], env)
    await call("/api/auth/sign-up/email", { email, password: "correct horse battery", name: "Ada" })
    const member = "(SELECT id FROM user WHERE email = ?)"
    if (colleagueOf !== undefined) db.query(`UPDATE member SET organizationId = (SELECT organizationId FROM member WHERE userId = ${member}) WHERE userId = ${member}`).run(colleagueOf, email)
    const response = await call("/api/auth/sign-in/email", { email, password: "correct horse battery" })
    db.query(`UPDATE member SET role = ? WHERE userId = (SELECT id FROM user WHERE email = ?)`).run(role, email)
    // The organization's cell environment, added to its server-provisioned metadata (reserved keys are written only server-side).
    if (environment) db.query(`UPDATE organization SET metadata = json_set(coalesce(metadata, '{}'), '$.environmentId', ?) WHERE id = (SELECT organizationId FROM member WHERE userId = (SELECT id FROM user WHERE email = ?))`).run("analytics", email)
    return response.headers.getSetCookie().map((line) => line.split(";")[0]).join("; ")
  }
  return { send, signUp, calls, answer: (next: (request: Request) => Response) => { answer = next } }
}

const body = (method: string, value: unknown) => ({ method, headers: { "content-type": "application/json" }, body: JSON.stringify(value) })
const gatewayCalls = (calls: Request[], from: number) => calls.slice(from).filter((r) => new URL(r.url).pathname !== "/api/context").map((r) => `${r.method} ${new URL(r.url).pathname}`)

test("every route needs a signed-in person and names its refusal", async () => {
  const { send } = stage()
  expect(await send("/inventory")).toMatchObject({ status: 401, body: { code: "connectors.sign_in", error: "Sign in first." } })
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
    const linked = await send("/link", { method: "POST", cookie: owner })
    // The OAuth state cookie reaches the browser, so the callback can match it.
    expect(linked.cookies.some((cookie) => /state/i.test(cookie.split("=")[0] ?? ""))).toBe(true)
    const url = new URL(String(linked.body.url))
    expect(`${url.origin}${url.pathname}`).toBe(`${GATEWAY}/api/auth/oauth2/authorize`)
    expect([url.searchParams.get("client_id"), url.searchParams.get("code_challenge_method")]).toEqual(["studio", "S256"])
    expect(url.searchParams.get("scope")?.split(" ").sort()).toEqual(["executor", "offline_access"])
    expect(url.searchParams.get("redirect_uri")).toBe(`${ORIGIN}/api/auth/callback/executor`)
    // RFC 8707: the gateway refuses an authorization without its product API as the resource.
    expect(url.searchParams.get("resource")).toBe(`${GATEWAY}/api`)
  }
  expect((await send("/unlink", { method: "POST", cookie: owner }, { grant: undefined })).body).toEqual({ linked: false })
})

test("app accounts: view, connect, reconnect, rename and disconnect, all in the gateway", async () => {
  const { send, signUp, calls, answer } = stage()
  const owner = await signUp("ada@example.com")
  answer((request) => {
    const path = `${request.method} ${new URL(request.url).pathname}`
    if (path === "GET /api/organizations/gw-org/inventory") return Response.json({ apps: [{ id: "github", name: "GitHub" }], accounts: [{ id: "acc1", provider: "github", label: "Work", accessToken: "never" }], links: [{ app: "github", account: "acc1" }] })
    if (path === "GET /api/organizations/gw-org/apps/github") return Response.json({ id: "github", requirements: { accounts: { github: { scope: "user" }, slack: { scope: "user" }, team: { scope: "app" } } } })
    if (path === "GET /api/organizations/gw-org/apps/github/accounts/user") return Response.json([{ requirement: "github", account: "acc1", state: "connected" }])
    if (path.endsWith("/connections")) return Response.json({ url: `${GATEWAY}/connect/c1`, redirectUri: `${GATEWAY}/cb` })
    if (path === "GET /api/organizations/gw-org/accounts/acc1") return Response.json({ account: { id: "acc1", label: "Personal" }, provider: { name: "GitHub" }, apps: [{ id: "github" }], health: { status: "ok" } })
    return Response.json({})
  })
  const inventory = (await send("/inventory", { cookie: owner })).body
  expect(inventory).toEqual({ apps: [{ id: "github", name: "GitHub" }], accounts: [{ id: "acc1", provider: "github", label: "Work" }], links: [{ app: "github", account: "acc1" }] })
  // Declared user requirements appear even when nothing fills them; the app-scoped one does not.
  expect((await send("/apps/github/accounts", { cookie: owner })).body).toEqual({ requirements: [{ requirement: "github", account: "acc1", state: "connected" }, { requirement: "slack", account: null, state: null }] })
  expect((await send("/accounts/acc1", { cookie: owner })).body).toEqual({ id: "acc1", provider: "GitHub", label: "Personal", apps: ["github"], health: "ok" })
  const from = calls.length
  expect((await send("/apps/github/accounts/github/connect", { ...body("POST", {}), cookie: owner })).body).toEqual({ url: `${GATEWAY}/connect/c1` })
  expect((await calls.at(-1)?.json()) as unknown).toEqual({ destination: { kind: "personal" } })
  await send("/apps/github/accounts/github/connect", { ...body("POST", { account: "acc1" }), cookie: owner })
  expect((await calls.at(-1)?.json()) as unknown).toEqual({ destination: { kind: "personal" }, account: "acc1" })
  expect((await send("/accounts/acc1", { ...body("PATCH", { label: "  Personal " }), cookie: owner })).body.label).toBe("Personal")
  expect((await send("/accounts/acc1", { ...body("PATCH", { label: "" }), cookie: owner })).body).toMatchObject({ code: "connectors.invalid_request", field: "label" })
  expect((await send("/apps/github/accounts/github/acc1", { method: "DELETE", cookie: owner })).body).toEqual({ disconnected: "acc1", deleted: true })
  expect(gatewayCalls(calls, from)).toEqual([
    "POST /api/organizations/gw-org/apps/github/accounts/user/github/connections",
    "POST /api/organizations/gw-org/apps/github/accounts/user/github/connections",
    "PATCH /api/organizations/gw-org/accounts/acc1",
    "GET /api/organizations/gw-org/accounts/acc1",
    "DELETE /api/organizations/gw-org/apps/github/accounts/user/github/acc1",
    "DELETE /api/organizations/gw-org/accounts/acc1",
  ])
})

test("disconnecting keeps an account another app still uses: unlinked here, not deleted", async () => {
  const { send, signUp, answer } = stage()
  const owner = await signUp("ada@example.com")
  answer((request) => request.method === "DELETE" && new URL(request.url).pathname === "/api/organizations/gw-org/accounts/acc1"
    ? Response.json({ _tag: "AccountLinkedToApp", message: "linked to another app" }, { status: 409 })
    : Response.json({}))
  expect((await send("/apps/github/accounts/github/acc1", { method: "DELETE", cookie: owner })).body).toEqual({ disconnected: "acc1", deleted: false })
})

test("tool bindings: an admin adds, edits and removes them; the mapping is checked against the tool's schema", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("owner@example.com")
  const added = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues", label: "Norn issues", inputMapping: { repo: { fixed: "norn" } } }), cookie: owner })
  const binding = added.body.binding as { id: string }
  expect(added.body.binding).toMatchObject({ app: "github", tool: "list_issues", label: "Norn issues", inputMapping: { repo: { fixed: "norn" } }, runRole: "member" })
  // Pinned by manifest digest, with the connector it came from.
  const digest = await manifestDigest(toolManifest({ app: "github", name: "list_issues", inputSchema: ISSUES, outputSchema: null }))
  expect(added.body.binding).toMatchObject({ manifestDigest: digest, connector: { gateway: "executor", origin: GATEWAY, app: "github", tool: "list_issues", version: "dpl_1" } })
  expect(digest).toMatch(/^sha256:[0-9a-f]{64}$/)
  const edited = await send(`/bindings/${binding.id}`, { ...body("PATCH", { label: "Open Norn issues", inputMapping: { repo: { fixed: "norn" }, state: { fixed: "open" } }, runRole: "admin" }), cookie: owner })
  expect(edited.body.binding).toMatchObject({ id: binding.id, label: "Open Norn issues", runRole: "admin", inputMapping: { state: { fixed: "open" } } })
  expect((await send("/bindings", { cookie: owner })).body).toMatchObject({ manage: true, bindings: [{ id: binding.id, label: "Open Norn issues", manifestDigest: digest, connector: { app: "github", tool: "list_issues" } }] })
  for (const [mapping, field] of [[{ nope: { fixed: 1 } }, "inputMapping.nope"], [{ limit: { fixed: "ten" } }, "inputMapping.limit"], [{ state: { fixed: "merged" } }, "inputMapping.state"]] as const) {
    expect((await send(`/bindings/${binding.id}`, { ...body("PATCH", { inputMapping: mapping }), cookie: owner })).body).toMatchObject({ code: "connectors.binding_invalid", field })
  }
  const member = await signUp("member@example.com", "member", "owner@example.com")
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: member })).body).toMatchObject({ code: "connectors.forbidden", action: { kind: "ask_admin" } })
  expect((await send(`/bindings/${binding.id}`, { method: "DELETE", cookie: member })).status).toBe(403)
  expect((await send(`/bindings/${binding.id}`, { method: "DELETE", cookie: owner })).body).toEqual({ removed: binding.id, retired: null })
  expect((await send(`/bindings/${binding.id}`, { method: "DELETE", cookie: owner })).body.code).toBe("connectors.binding_not_found")
})

test("tools list with their schemas and the bindings that let them run", async () => {
  const { send, signUp, answer } = stage()
  const owner = await signUp("ada@example.com")
  const { body: added } = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner })
  answer(() => Response.json({ items: [{ name: "list_issues", description: "List issues", inputSchema: ISSUES }, { name: "delete_repo", inputSchema: { type: "object" } }] }))
  expect((await send("/apps/github/tools", { cookie: owner })).body.tools).toEqual([
    { app: "github", name: "list_issues", description: "List issues", inputSchema: ISSUES, outputSchema: null, version: null, bindings: [(added.binding as { id: string }).id] },
    { app: "github", name: "delete_repo", description: null, inputSchema: { type: "object" }, outputSchema: null, version: null, bindings: [] },
  ])
})

test("a test run goes through its binding: fixed fields win, no override, other organizations see nothing, roles are checked", async () => {
  const { send, signUp, calls, answer } = stage()
  const owner = await signUp("ada@example.com")
  const { body: added } = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues", inputMapping: { repo: { fixed: "norn" } } }), cookie: owner })
  const id = (added.binding as { id: string }).id
  answer(() => Response.json({ issues: [] }))
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: { state: "open" } }), cookie: owner })).body).toEqual({ binding: id, result: { issues: [] } })
  expect((await calls.at(-1)?.json()) as unknown).toEqual({ tool: "list_issues", input: { state: "open", repo: "norn" } })
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: { repo: "other" } }), cookie: owner })).body).toMatchObject({ code: "connectors.binding_invalid", field: "input.repo" })
  expect((await send("/bindings/missing/run", { ...body("POST", { input: {} }), cookie: owner })).body.code).toBe("connectors.binding_not_found")
  const stranger = await signUp("stranger@example.com")
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: {} }), cookie: stranger })).body.code).toBe("connectors.binding_not_found")
  await send(`/bindings/${id}`, { ...body("PATCH", { runRole: "admin" }), cookie: owner })
  const member = await signUp("member@example.com", "member", "ada@example.com")
  expect((await send(`/bindings/${id}/run`, { ...body("POST", { input: {} }), cookie: member })).body).toMatchObject({ code: "connectors.forbidden", action: { kind: "ask_admin" } })
  const viewer = await signUp("viewer@example.com", "viewer", "ada@example.com")
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
    // The gateway's own shape (Executor 2.0.0-beta.37): the requirement is its `slot`.
    [Response.json({ _tag: "AccountRequired", app: "github", slot: "echoKey", scope: "user" }, { status: 409 }), { code: "connectors.account_required", action: { kind: "connect_account", app: "github", requirement: "echoKey" } }],
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
  const env: Env = { ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, TOOL_GATEWAY_URL: GATEWAY }
  const response = await worker.fetch(new Request(`${ORIGIN}/api/connectors/status`) as Parameters<typeof worker.fetch>[0], env)
  expect([response.status, ((await response.json()) as { code: string }).code]).toEqual([401, "connectors.sign_in"])
})

test("the gateway subject comes from introspection with Studio's client credentials", async () => {
  const seen: Request[] = []
  const answer = (body: unknown) => (async (input: RequestInfo | URL, init?: RequestInit) => { seen.push(new Request(input, init)); return Response.json(body) }) as typeof fetch
  const client = { url: GATEWAY, clientId: "studio", clientSecret: "s".repeat(32) }
  expect(await gatewaySubject(client, "tok", answer({ active: true, sub: "user_1" }))).toEqual({ id: "user_1", name: "Tool gateway", email: "user_1@gateway.invalid", emailVerified: false })
  expect(seen[0]?.url).toBe(`${GATEWAY}/api/auth/oauth2/introspect`)
  expect(seen[0]?.headers.get("authorization")).toBe(`Basic ${btoa(`studio:${"s".repeat(32)}`)}`)
  expect(await seen[0]?.text()).toBe("token=tok&token_type_hint=access_token")
  expect(await gatewaySubject(client, "tok", answer({ active: false }))).toBeNull()
  expect(await gatewaySubject(client, undefined, answer({}))).toBeNull()
})

test("the manifest digest matches the shared RFC 8785 golden vectors", async () => {
  const file = JSON.parse(readFileSync(join(import.meta.dir, "../../../../tools/norn-ts/src/builder/fixtures/binding-manifest-digest.vectors.json"), "utf8")) as { vectors: { name: string; manifestJson: string; canonical: string; manifestDigest: string; version: string }[] }
  expect(file.vectors.length).toBeGreaterThanOrEqual(5)
  for (const vector of file.vectors) {
    const manifest = JSON.parse(vector.manifestJson) as Parameters<typeof manifestDigest>[0]
    expect(canonicalJson(manifest)).toBe(vector.canonical)
    expect(await manifestDigest(manifest)).toBe(vector.manifestDigest)
    expect(connectorVersion(null, vector.manifestDigest)).toBe(vector.version)
  }
})

test("the manifest digest is over canonical JSON: key order does not change it, the contract does", async () => {
  expect(canonicalJson({ b: [1, { d: true, c: null }], a: "x" })).toBe('{"a":"x","b":[1,{"c":null,"d":true}]}')
  const a = await manifestDigest(toolManifest({ app: "github", name: "t", inputSchema: { type: "object", properties: { x: { type: "string" } } }, outputSchema: null }))
  const reordered = await manifestDigest(toolManifest({ app: "github", name: "t", inputSchema: { properties: { x: { type: "string" } }, type: "object" }, outputSchema: null }))
  const changed = await manifestDigest(toolManifest({ app: "github", name: "t", inputSchema: { type: "object", properties: { x: { type: "integer" } } }, outputSchema: null }))
  expect(a).toBe(reordered)
  expect(a).not.toBe(changed)
})

test("the gateway calls fetch without a receiver, as workerd requires", async () => {
  // workerd's native fetch throws "Illegal invocation" when called as a method of another object.
  const strict = function (this: unknown, _input: RequestInfo | URL) {
    if (this !== undefined && this !== globalThis) throw new TypeError("Illegal invocation")
    return Promise.resolve(Response.json({ organization: "gw-org" }))
  } as unknown as typeof fetch
  expect(await new Gateway(GATEWAY, TOKEN, strict).organization()).toBe("gw-org")
})

test("an unreachable gateway, a malformed path and a third-party sign-in are typed refusals", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("ada@example.com")
  const down = (async () => { throw new TypeError("fetch failed") }) as unknown as typeof fetch
  expect((await send("/inventory", { cookie: owner }, { fetch: down })).body).toMatchObject({ code: "connectors.gateway", error: expect.stringContaining("could not be reached") })
  expect(await send("/bindings/%E0%A4%A", { method: "DELETE", cookie: owner })).toMatchObject({ status: 400, body: { code: "connectors.invalid_request" } })
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  const env: Env = { ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, TOOL_GATEWAY_URL: GATEWAY, TOOL_GATEWAY_CLIENT_ID: "studio", TOOL_GATEWAY_CLIENT_SECRET: "s".repeat(32) }
  for (const path of ["/api/auth/sign-in/social", "/api/auth/sign-in/oauth2"]) {
    const response = await worker.fetch(new Request(`${ORIGIN}${path}`, { method: "POST", headers: { "content-type": "application/json", origin: ORIGIN }, body: JSON.stringify({ provider: "executor", providerId: "executor" }) }) as Parameters<typeof worker.fetch>[0], env)
    expect(response.status).toBe(403)
  }
})

test("a fixed value for a property without a declared type is left to the gateway", () => {
  const schema = { type: "object", properties: { filter: {}, choice: { anyOf: [{ type: "string" }, { type: "integer" }] }, n: { type: "integer" } } }
  expect(readDraft({ app: "github", tool: "t", inputMapping: { filter: { fixed: { any: "thing" } }, choice: { fixed: 3 } } }, schema)).toMatchObject({ inputMapping: { filter: { fixed: { any: "thing" } } } })
  expect(readDraft({ app: "github", tool: "t", inputMapping: { n: { fixed: "x" } } }, schema)).toMatchObject({ field: "inputMapping.n" })
  // Prototype names are not declared properties.
  expect(readDraft({ app: "github", tool: "t", inputMapping: { constructor: { fixed: "x" } } }, schema)).toMatchObject({ field: "inputMapping.constructor" })
  // ...and an input field with such a name is not "fixed by the binding".
  expect(runInput({ inputMapping: { n: { fixed: 1 } } } as never, { toString: "x" })).toMatchObject({ ok: true })
})

/** A cell host that answers DEFS's definition writes from `answer`, recording each request. */
function cellHost(answer: (path: string, body: Record<string, unknown>) => Response) {
  const seen: { path: string; query: string; body: Record<string, unknown>; bearer: boolean }[] = []
  const host = { fetch: async (request: Request) => {
    const body = (await request.json()) as Record<string, unknown>
    const url = new URL(request.url)
    const path = url.pathname
    seen.push({ path, query: url.search, body, bearer: (request.headers.get("authorization") ?? "").startsWith("Bearer ") })
    return answer(path, body)
  } } as unknown as Fetcher
  return { host, seen }
}

test("a binding is installed in the cell as a definition, which the record and its edits follow", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("ada@example.com")
  let version = 0
  const { host, seen } = cellHost((path, body) => path === "/v1/definitions:install" ? Response.json({ installed: true, id: body.id, version: ++version, receipt: { receiptHash: "r", commitSeq: version, recordedAtMs: 1 } }) : Response.json({ installed: true, id: "x", version: ++version, receipt: {} }))
  const added = (await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues", label: "Issues" }), cookie: owner }, { host })).body.binding as Record<string, unknown> & { id: string; manifestDigest: string }
  expect(seen[0]).toMatchObject({ path: "/v1/definitions:install", bearer: true, body: { kind: "binding", id: `studio.binding.${added.id}`, payload: {
    connector: { gateway: "executor", origin: GATEWAY, app: "github", tool: "list_issues", version: "dpl_1" }, inputSchema: ISSUES, outputSchema: null, manifestDigest: added.manifestDigest,
    label: "Issues", inputMapping: {}, runRole: "member",
  } } })
  expect(seen[0]?.body).not.toHaveProperty("expectedVersion")
  // DEFS's scope: the actor's own organization, resolved on the server, and the environment.
  const orgId = new URLSearchParams(seen[0]?.query).get("orgId")
  expect(orgId).toBeString()
  expect(new URLSearchParams(seen[0]?.query).get("environmentId")).toBe("analytics")
  expect(added).toMatchObject({ definition: { id: `studio.binding.${added.id}`, version: 1 }, connectorRef: "executor:github/list_issues@dpl_1" })
  // An edit installs the next version of the same definition.
  const edited = (await send(`/bindings/${added.id}`, { ...body("PATCH", { label: "Open issues" }), cookie: owner }, { host })).body.binding
  expect(seen[1]?.body).toMatchObject({ id: `studio.binding.${added.id}`, expectedVersion: 1 })
  expect(edited).toMatchObject({ definition: { version: 2 } })
  // Removing retires the definition at its current version first.
  expect((await send(`/bindings/${added.id}`, { method: "DELETE", cookie: owner }, { host })).body).toEqual({ removed: added.id, retired: `studio.binding.${added.id}` })
  expect(seen[2]).toMatchObject({ path: `/v1/definitions/studio.binding.${added.id}:retire`, body: { expectedVersion: 2 } })
  // Every definition write carries the same scope.
  const scope = seen[0]?.query ?? ""
  expect(seen.map((r) => r.query)).toEqual([scope, scope, scope])
})

test("a refused or conflicting definition saves nothing; a stage without the route keeps the local record", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("ada@example.com")
  const refusing = cellHost(() => Response.json({ code: "definition.refused", diagnostics: [{ code: "binding.digest_mismatch", message: "manifestDigest does not match the payload", span: { pointer: "/payload/manifestDigest" } }] }, { status: 422 }))
  const refused = await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: refusing.host })
  expect(refused).toMatchObject({ status: 422, body: { code: "connectors.binding_refused", findings: [{ code: "binding.digest_mismatch", pointer: "/payload/manifestDigest" }] } })
  expect((await send("/bindings", { cookie: owner })).body.bindings).toEqual([])
  const conflicting = cellHost(() => Response.json({ code: "definition.version_conflict" }, { status: 409 }))
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: conflicting.host })).body.code).toBe("connectors.binding_conflict")
  // A cell whose binding kind does not take label, inputMapping and runRole yet: kept locally, like a 404.
  const older = cellHost(() => Response.json({ code: "definition.refused", diagnostics: ["label", "inputMapping", "runRole"].map((f) => ({ code: "definition.unknown_field", message: `unknown field \`${f}\``, span: { pointer: `/payload/${f}` } })) }, { status: 422 }))
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: older.host })).body.binding).toMatchObject({ definition: null })
  // An unknown-field refusal that also names something else is still a refusal.
  const mixed = cellHost(() => Response.json({ code: "definition.refused", diagnostics: [{ code: "definition.unknown_field", message: "unknown field `label`", span: { pointer: "/payload/label" } }, { code: "binding.role_undeclared", message: "runRole names no declared role", span: { pointer: "/payload/runRole" } }] }, { status: 422 }))
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: mixed.host })).body.code).toBe("connectors.binding_refused")
  // The cell's authority says no: a typed denial with the admin fix, never a fallback and never "unavailable".
  const denying = cellHost(() => Response.json({ format: "norn/refusal@2", code: "authz.denied", message: "the read request is not authorized" }, { status: 403 }))
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: denying.host })).body).toMatchObject({ code: "connectors.binding_denied", action: { kind: "ask_admin" } })
  // The cell does not serve the binding kind yet (422 kind_unknown): kept locally, like a 404.
  const unknownKind = cellHost(() => Response.json({ code: "definition.kind_unknown", message: "no such kind: binding" }, { status: 422 }))
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: unknownKind.host })).body.binding).toMatchObject({ definition: null })
  const noRoute = cellHost(() => Response.json({ code: "route.not_found" }, { status: 404 }))
  const local = (await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: noRoute.host })).body.binding
  expect(local).toMatchObject({ definition: null, connectorRef: "executor:github/list_issues@dpl_1" })
  const down = { fetch: async () => { throw new TypeError("connect refused") } } as unknown as Fetcher
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host: down })).body.code).toBe("connectors.cell_unavailable")
})

test("a tool with no gateway version gets sha256- and the first 12 hex of its digest", () => {
  const digest = `sha256:${"ab12".repeat(16)}`
  expect(connectorVersion(null, digest)).toBe("sha256-ab12ab12ab12")
  expect(connectorVersion("dpl_7", digest)).toBe("dpl_7")
})

test("the whole link round trip: state cookie, basic client auth, resource, introspection, then a linked grant", async () => {
  const db = new Database(":memory:")
  for (const file of ["0001_better_auth.sql", "0002_workspace_provisioning.sql", "0002_connector_binding.sql", "0003_connector_binding_manifest.sql", "0004_connector_binding_definition.sql"]) db.exec(migration(file))
  const env: Env = { ASSETS: { fetch: async () => new Response("studio") } as unknown as Fetcher, AUTH_DB: db as unknown as D1Database, BETTER_AUTH_SECRET: "t".repeat(40), BETTER_AUTH_URL: ORIGIN, TOOL_GATEWAY_URL: GATEWAY, TOOL_GATEWAY_CLIENT_ID: "studio", TOOL_GATEWAY_CLIENT_SECRET: "s".repeat(32) }
  const jar = new Map<string, string>()
  const keep = (response: Response) => { for (const line of response.headers.getSetCookie()) { const [pair] = line.split(";"); const [name, ...rest] = (pair ?? "").split("="); if (name) jar.set(name, rest.join("=")) } return response }
  const cookie = () => [...jar].map(([k, v]) => `${k}=${v}`).join("; ")
  const call = async (path: string, init: RequestInit = {}) => {
    const headers = new Headers(init.headers); headers.set("cookie", cookie()); headers.set("origin", ORIGIN); headers.set("cf-connecting-ip", "198.51.100.77")
    return keep(await worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers, redirect: "manual" }) as Parameters<typeof worker.fetch>[0], env))
  }
  const json = (value: unknown) => ({ method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(value) })
  await call("/api/auth/sign-up/email", json({ email: "ada@example.com", password: "correct horse battery", name: "Ada" }))
  await call("/api/auth/sign-in/email", json({ email: "ada@example.com", password: "correct horse battery" }))
  // The gateway's token and introspection endpoints, as Executor answers them.
  const real = globalThis.fetch
  const seen: { url: string; auth: string | null; body: string }[] = []
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init)
    if (!request.url.startsWith(GATEWAY)) return real(input, init)
    const body = await request.text()
    seen.push({ url: request.url, auth: request.headers.get("authorization"), body })
    if (request.url.endsWith("/oauth2/token")) {
      if (body.includes("client_secret=")) return Response.json({ error: "invalid_client", error_description: "client registered for client_secret_basic cannot use client_secret_post" }, { status: 400 })
      return Response.json({ access_token: TOKEN, refresh_token: "r", token_type: "Bearer", expires_in: 3600, scope: "executor offline_access" })
    }
    if (request.url.endsWith("/oauth2/introspect")) return Response.json({ active: true, sub: "gw_user_1" })
    return new Response("not found", { status: 404 })
  }) as typeof fetch
  try {
    const link = await call("/api/connectors/link", { method: "POST" })
    const state = new URL(((await link.json()) as { url: string }).url).searchParams.get("state") ?? ""
    const back = await call(`/api/auth/callback/executor?code=abc&state=${encodeURIComponent(state)}`)
    expect(back.status).toBe(302)
    expect(back.headers.get("location") ?? "").not.toContain("error")
    const token = seen.find((r) => r.url.endsWith("/oauth2/token"))
    expect(token?.auth).toBe(`Basic ${btoa(`studio:${"s".repeat(32)}`)}`)
    expect(token?.body).toContain(`resource=${encodeURIComponent(`${GATEWAY}/api`)}`)
    expect(token?.body).not.toContain("client_secret=")
    expect(seen.some((r) => r.url.endsWith("/oauth2/introspect"))).toBe(true)
    // The grant is now linked to this person, and never leaves the server.
    const status = await call("/api/connectors/status")
    const text = await status.text()
    expect(JSON.parse(text)).toEqual({ gateway: GATEWAY, linked: true })
    expect(text).not.toContain(TOKEN)
  } finally {
    globalThis.fetch = real
  }
})

test("every definitions call carries the session's orgId and environmentId; no environment, no call", async () => {
  const { send, signUp } = stage()
  const owner = await signUp("ada@example.com")
  const { host, seen } = cellHost((path, body) => Response.json({ installed: true, id: path.endsWith(":retire") ? "x" : body.id, version: seen.length, receipt: {} }))
  const added = (await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: owner }, { host })).body.binding as { id: string }
  await send(`/bindings/${added.id}`, { ...body("PATCH", { label: "Edited" }), cookie: owner }, { host })
  await send(`/bindings/${added.id}`, { method: "DELETE", cookie: owner }, { host })
  expect(seen.map((r) => r.path)).toEqual(["/v1/definitions:install", "/v1/definitions:install", `/v1/definitions/studio.binding.${added.id}:retire`])
  for (const call of seen) {
    const query = new URLSearchParams(call.query)
    expect(query.get("orgId")).toMatch(/.+/)
    expect(query.get("environmentId")).toBe("analytics")
  }
  // An organization that names no environment: refused before the cell is called, never defaulted.
  const lone = await signUp("lone@example.com", "owner", undefined, false)
  const other = cellHost(() => Response.json({}))
  expect((await send("/bindings", { ...body("POST", { app: "github", tool: "list_issues" }), cookie: lone }, { host: other.host })).body).toMatchObject({ code: "connectors.no_environment", action: { kind: "ask_admin" } })
  expect(other.seen).toEqual([])
})

test("the cell scope is the organization's declared environment, or null", async () => {
  const db = new Database(":memory:")
  db.exec(migration("0001_better_auth.sql"))
  const auth = createAuth({ database: db, secret: "t".repeat(40), baseURL: ORIGIN })
  const add = (id: string, metadata: string | null) => db.query("INSERT INTO organization (id, name, slug, createdAt, metadata) VALUES (?, ?, ?, ?, ?)").run(id, id, id, Date.now(), metadata)
  add("org_demo", '{"environmentId":"analytics"}')
  add("org_none", null)
  add("org_bad", '{"environmentId":"../x"}')
  add("org_text", "not json")
  expect(await cellScope(auth, "org_demo")).toEqual({ orgId: "org_demo", environmentId: "analytics" })
  for (const id of ["org_none", "org_bad", "org_text", "org_missing"]) expect(await cellScope(auth, id)).toEqual({ orgId: id, environmentId: null })
  expect(scopeQuery({ orgId: "org_demo", environmentId: "analytics" })).toBe("orgId=org_demo&environmentId=analytics")
})
