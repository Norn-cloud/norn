import { expect, test } from "bun:test"
import { createNornClient, fillRoute, NornRefusalError, NornUnavailableError, type FetchLike, type NornDocument, type Schemas } from "../src/index.ts"

const scope = { orgId: "org_1", environmentId: "dev" }

/** A cell stand-in that records each request and answers with `status` and `body` text. */
function cell(status: number, body: string, headers: Record<string, string> = {}) {
  const asked: { url: string; init: RequestInit | undefined }[] = []
  const fetch: FetchLike = async (url, init) => {
    asked.push({ url, init })
    return new Response(body, { status, headers: { "content-type": "application/json", ...headers } })
  }
  return { asked, fetch }
}

// A 28-digit decimal: a double keeps 15 to 17 significant digits.
const EXACT = "1234567890123456.789012345678"
const record = `{"objectType":"demo.Item","id":"i/1","scope":"org_1/dev","version":3,"values":{"amount":${EXACT},"rate":100.000,"name":"one"},"withheld":false,"withheldProperties":[]}`

test("a typed read names the route's parameters, binds the scope, and keeps every number's exact text", async () => {
  const { asked, fetch } = cell(200, record, { "cache-control": "private, max-age=60", etag: '"v3"' })
  const client = createNornClient({ baseUrl: "https://cell.example/", scope, fetch })
  const read = await client.get("/v1/objects/{type}/{id}", { path: { type: "demo.Item", id: "i/1" } })
  const typed: NornDocument<Schemas["NornQueriedRecord"]> = read
  expect(asked[0]?.url).toBe("https://cell.example/v1/objects/demo.Item/i%2F1?orgId=org_1&environmentId=dev")
  expect(asked[0]?.init?.method).toBe("GET")
  expect(typed.document.version).toBe(3)
  expect(typed.numbers.get("/values/amount")).toBe(EXACT)
  expect(String(typed.document.values?.["amount"])).not.toBe(EXACT)
  expect(typed.numbers.get("/values/rate")).toBe("100.000")
  expect(typed.text).toBe(record)
  expect(typed.cacheControl).toBe("private, max-age=60")
  expect(typed.etag).toBe('"v3"')
  // @ts-expect-error A record read needs its identity.
  void (() => client.get("/v1/objects/{type}/{id}", { path: { type: "demo.Item" } }))
  // @ts-expect-error The scope is the client's, never a call's.
  void (() => client.get("/v1/runs/{id}", { path: { id: "r" }, query: { orgId: "other" } }))
  // @ts-expect-error Only served routes are callable.
  void (() => client.get("/v1/nowhere"))
})

test("query members keep their order, lists repeat, absent members are left out, the scope comes last", async () => {
  const { asked, fetch } = cell(200, '{"items":[],"cursor":null,"total":0}')
  const client = createNornClient({ baseUrl: "/api/host", scope: async () => scope, fetch, credentials: "same-origin" })
  await client.get("/v1/decisions", { query: { state: "pending", limit: 0 } })
  expect(asked[0]?.url).toBe("/api/host/v1/decisions?state=pending&limit=0&orgId=org_1&environmentId=dev")
  expect(asked[0]?.init?.credentials).toBe("same-origin")
  expect(new Headers(asked[0]?.init?.headers).has("authorization")).toBe(false)
  expect(await client.route("/v1/runs/{id}", { path: { id: "run 1" } })).toBe("/v1/runs/run%201?orgId=org_1&environmentId=dev")
})

test("a host token is read before each call and sent as a bearer credential", async () => {
  const { asked, fetch } = cell(200, '{"runId":"r"}')
  let minted = 0
  const client = createNornClient({ baseUrl: "https://cell.example", scope, fetch, token: async () => `t${++minted}` })
  await client.get("/v1/runs/{id}", { path: { id: "r" } })
  await client.get("/v1/runs/{id}", { path: { id: "r" } })
  expect(asked.map(({ init }) => new Headers(init?.headers).get("authorization"))).toEqual(["Bearer t1", "Bearer t2"])
})

test("a call sends its typed body as JSON, decimals as their exact text", async () => {
  const { asked, fetch } = cell(200, '{"outcome":"committed"}')
  const client = createNornClient({ baseUrl: "https://cell.example", scope, fetch })
  await client.post("/v1/actions/{actionId}", { path: { actionId: "demo.item.add" }, body: { input: { amount: EXACT }, idempotencyKey: "k1" } })
  expect(asked[0]?.init?.method).toBe("POST")
  expect(new Headers(asked[0]?.init?.headers).get("content-type")).toBe("application/json")
  expect(asked[0]?.init?.body).toBe(`{"input":{"amount":"${EXACT}"},"idempotencyKey":"k1"}`)
})

test("a refusal is a typed error with the server's code, category and checker diagnostics", async () => {
  const refusal = {
    format: "norn/refusal@2", retryable: false, reason: null, code: "question.invalid", category: "invalid",
    message: "the question does not check", explanation: "fix the measure",
    diagnostics: [{ code: "Q001", severity: "error", message: "unknown measure", span: { pointer: "/question/measure" } }],
  }
  const { fetch } = cell(422, JSON.stringify(refusal))
  const client = createNornClient({ baseUrl: "https://cell.example", scope, fetch })
  const error = await client.post("/v1/questions:evaluate", { body: {} as never }).catch((caught: unknown) => caught)
  if (!(error instanceof NornRefusalError)) throw new Error("expected a refusal")
  expect(error.status).toBe(422)
  expect(error.code).toBe("question.invalid")
  expect(error.category).toBe("invalid")
  expect(error.retryable).toBe(false)
  expect(error.message).toBe("the question does not check")
  expect(error.diagnostics).toEqual([{ code: "Q001", severity: "error", message: "unknown measure", span: { pointer: "/question/measure" } }])
  expect(error.refusal.explanation).toBe("fix the measure")
})

test("an answer that is no Norn refusal, or no answer at all, is unavailable, never a refusal", async () => {
  const proxy = createNornClient({ baseUrl: "https://cell.example", scope, fetch: cell(502, "<html>bad gateway</html>").fetch })
  const page = await proxy.get("/v1/runs/{id}", { path: { id: "r" } }).catch((caught: unknown) => caught)
  expect(page).toBeInstanceOf(NornUnavailableError)
  expect((page as NornUnavailableError).status).toBe(502)
  expect((page as NornUnavailableError).body).toBe("<html>bad gateway</html>")
  // A code without the refusal's category is not a `norn/refusal@2` document.
  const worker = createNornClient({ baseUrl: "https://cell.example", scope, fetch: cell(403, '{"code":"PLAN_LIMIT","message":"upgrade"}').fetch })
  expect(await worker.get("/v1/runs/{id}", { path: { id: "r" } }).catch((caught: unknown) => caught)).toBeInstanceOf(NornUnavailableError)
  const down = new TypeError("network down")
  const offline = createNornClient({ baseUrl: "https://cell.example", scope, fetch: async () => { throw down } })
  const failed = await offline.get("/v1/runs/{id}", { path: { id: "r" } }).catch((caught: unknown) => caught)
  expect(failed).toBeInstanceOf(NornUnavailableError)
  expect((failed as NornUnavailableError).status).toBe(0)
  expect((failed as NornUnavailableError).cause).toBe(down)
})

test("route templates fill in one pass and refuse a missing or unclosed parameter", () => {
  expect(fillRoute("/v1/objects/{type}:facets", { type: "a b" })).toBe("/v1/objects/a%20b:facets")
  expect(fillRoute("/v1/cell:provision", undefined)).toBe("/v1/cell:provision")
  expect(() => fillRoute("/v1/runs/{id}", {})).toThrow("needs the path parameter id")
  expect(() => fillRoute("/v1/runs/{id", { id: "r" })).toThrow("unclosed")
  const hostile = "{".repeat(50_000)
  const started = performance.now()
  expect(() => fillRoute(hostile, {})).toThrow()
  expect(performance.now() - started).toBeLessThan(1_000)
})

test("a success the route declares without content has no document, never an error", async () => {
  const { fetch } = cell(202, "")
  const client = createNornClient({ baseUrl: "https://cell.example", scope, fetch })
  const accepted = await client.post("/v1/mcp", { body: { jsonrpc: "2.0", method: "notifications/initialized" } as never })
  expect(accepted.status).toBe(202)
  expect(accepted.document).toBeUndefined()
  expect(accepted.numbers.size).toBe(0)
})

test("a refusal-like body that is no valid norn/refusal@2 document is unavailable, never a typed refusal", async () => {
  const valid = { format: "norn/refusal@2", retryable: false, reason: null, code: "authz.denied", category: "permission", message: "denied", explanation: "policy" }
  const malformed: Record<string, unknown>[] = [
    { ...valid, format: undefined },
    { ...valid, format: "norn/refusal@1" },
    { ...valid, retryable: "no" },
    { ...valid, retryable: undefined },
    { ...valid, explanation: 7 },
    { ...valid, category: "teapot" },
    { ...valid, reason: "because" },
    { ...valid, diagnostics: "none" },
    { ...valid, diagnostics: [{ code: "Q001", severity: "error", message: "m" }] },
    { ...valid, diagnostics: [{ code: "Q001", severity: "error", message: "m", span: { pointer: 3 } }] },
    { ...valid, incidentId: 42 },
  ]
  for (const body of malformed) {
    const client = createNornClient({ baseUrl: "https://cell.example", scope, fetch: cell(403, JSON.stringify(body)).fetch })
    const error = await client.get("/v1/runs/{id}", { path: { id: "r" } }).catch((caught: unknown) => caught)
    expect(error, JSON.stringify(body)).toBeInstanceOf(NornUnavailableError)
    expect((error as NornUnavailableError).status).toBe(403)
  }
  const client = createNornClient({ baseUrl: "https://cell.example", scope, fetch: cell(403, JSON.stringify({ ...valid, diagnostics: [{ code: "Q001", severity: "error", message: "m", span: null, law: null }], incidentId: "inc_1" })).fetch })
  const refused = await client.get("/v1/runs/{id}", { path: { id: "r" } }).catch((caught: unknown) => caught)
  expect(refused).toBeInstanceOf(NornRefusalError)
  expect((refused as NornRefusalError).diagnostics).toHaveLength(1)
})

test("a file route is read as bytes by its content type, never through the JSON decoder", async () => {
  const parquet = new Uint8Array([0x50, 0x41, 0x52, 0x31, 0x00, 0xff, 0x50, 0x41, 0x52, 0x31])
  const asked: (RequestInit | undefined)[] = []
  const client = createNornClient({
    baseUrl: "https://cell.example",
    scope,
    fetch: async (_url, init) => { asked.push(init); return new Response(parquet, { status: 200, headers: { "content-type": "application/vnd.apache.parquet", etag: '"e1"' } }) },
  })
  const file = await client.download("/v1/objects/{type}:export", { path: { type: "demo.Item" }, query: { columns: "amount" } })
  expect(file.contentType).toBe("application/vnd.apache.parquet")
  expect([...new Uint8Array(file.bytes)]).toEqual([...parquet])
  expect(file.etag).toBe('"e1"')
  expect(new Headers(asked[0]?.headers).get("accept")).toBe("*/*")
  // @ts-expect-error An export answers with a file, not a document: `get` does not take it.
  void (() => client.get("/v1/objects/{type}:export", { path: { type: "demo.Item" }, query: { columns: "amount" } }))
  // @ts-expect-error A document route is read with `get`.
  void (() => client.download("/v1/runs/{id}", { path: { id: "r" } }))
  // A document read that is answered with a file is unavailable, not decoded.
  const misrouted = await (client.get as unknown as (route: string, options: unknown) => Promise<unknown>)("/v1/runs/{id}", { path: { id: "r" } }).catch((caught: unknown) => caught)
  expect(misrouted).toBeInstanceOf(NornUnavailableError)
  expect((misrouted as NornUnavailableError).message).toContain("application/vnd.apache.parquet")
  // A file read answered with a JSON document is not the file.
  const json = createNornClient({ baseUrl: "https://cell.example", scope, fetch: cell(200, '{"rows":[]}').fetch })
  expect(await json.download("/v1/objects/{type}:export", { path: { type: "demo.Item" }, query: { columns: "amount" } }).catch((caught: unknown) => caught)).toBeInstanceOf(NornUnavailableError)
  // A refusal to an export is still the typed refusal.
  const denied = { format: "norn/refusal@2", retryable: false, reason: null, code: "authz.denied", category: "permission", message: "denied", explanation: "policy" }
  const refusing = createNornClient({ baseUrl: "https://cell.example", scope, fetch: cell(403, JSON.stringify(denied)).fetch })
  expect(await refusing.download("/v1/objects/{type}:export", { path: { type: "demo.Item" }, query: { columns: "amount" } }).catch((caught: unknown) => caught)).toBeInstanceOf(NornRefusalError)
})
