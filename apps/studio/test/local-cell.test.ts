import { afterAll, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { LIVE_PROTOCOL, LiveHub, LocalCellError, localStudio, readSession, serveLocal, subscribable, type DevSession, type LiveSocket } from "../scripts/local-cell.ts"

const temp = mkdtempSync(join(tmpdir(), "studio-local-"))
afterAll(() => rmSync(temp, { recursive: true, force: true }))

/** A project directory with `norn dev`'s descriptor and token, as the dev host writes them. */
function project(descriptor: Record<string, unknown>, token = "dev-token-1"): string {
  const root = mkdtempSync(join(temp, "project-"))
  mkdirSync(join(root, ".norn"))
  writeFileSync(join(root, "norn.json"), JSON.stringify({ name: "shop" }))
  writeFileSync(join(root, ".norn/dev.json"), JSON.stringify(descriptor))
  writeFileSync(join(root, ".norn/dev.token"), `${token}\n`)
  return root
}

const DESCRIPTOR = { format: "norn-dev/1", url: "http://127.0.0.1:4174", tokenFile: ".norn/dev.token", scope: { orgId: "local", environmentId: "dev" }, actor: "user:dev" }

test("--as reviewer is the descriptor's reviewer, with its own token; a descriptor without one refuses", () => {
  const root = project({ ...DESCRIPTOR, reviewer: { actor: "user:dev+reviewer", tokenFile: ".norn/dev.reviewer.token" } })
  writeFileSync(join(root, ".norn/dev.reviewer.token"), "reviewer-token\n")
  expect(readSession(root, "reviewer")).toEqual({ url: "http://127.0.0.1:4174", token: "reviewer-token", actor: "user:dev+reviewer", as: "reviewer", orgId: "local", environmentId: "dev", project: "shop" })
  expect(() => readSession(project(DESCRIPTOR), "reviewer")).toThrow("names no reviewer")
  expect(() => readSession(project({ ...DESCRIPTOR, reviewer: { actor: "x", tokenFile: "../../t" } }), "reviewer")).toThrow("outside the project")
})

test("the session is the descriptor's: loopback url, project token, scope, actor and project name", () => {
  expect(readSession(project(DESCRIPTOR))).toEqual({ url: "http://127.0.0.1:4174", token: "dev-token-1", actor: "user:dev", orgId: "local", environmentId: "dev", project: "shop" })
  expect(() => readSession(join(temp, "nowhere"))).toThrow(LocalCellError)
  expect(() => readSession(project({ ...DESCRIPTOR, url: "http://10.0.0.5:4174" }))).toThrow("not a loopback address")
  expect(() => readSession(project({ ...DESCRIPTOR, url: "https://cell.example:443" }))).toThrow("not a loopback address")
  expect(() => readSession(project({ ...DESCRIPTOR, tokenFile: "../../token" }))).toThrow("outside the project")
  expect(() => readSession(project({ ...DESCRIPTOR, format: "norn-dev/2" }))).toThrow("not a norn-dev/1 descriptor")
})

const SESSION: DevSession = { url: "http://127.0.0.1:4174", token: "secret-dev-token", actor: "user:dev", orgId: "local", environmentId: "dev", project: "shop" }
const dist = mkdtempSync(join(temp, "dist-"))
writeFileSync(join(dist, "index.html"), "<!doctype html><div id=root></div>")
mkdirSync(join(dist, "assets"))
writeFileSync(join(dist, "assets/app.js"), "console.log(1)")
writeFileSync(join(temp, "outside.txt"), "not the app")

/** The local Studio on port 5180, with a dev host that records what reached it. */
function studio() {
  const seen: { url: string; method: string; authorization: string | null; body: string }[] = []
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    const headers = new Headers(init?.headers)
    seen.push({ url: String(input), method: init?.method ?? "GET", authorization: headers.get("authorization"), body: init?.body ? new TextDecoder().decode(init.body as ArrayBuffer) : "" })
    return Response.json({ code: "object.not_found" }, { status: 404 })
  }) as typeof fetch
  const call = (path: string, init: RequestInit & { headers?: Record<string, string> } = {}) =>
    localStudio(new Request(`http://127.0.0.1:5180${path}`, { ...init, headers: { host: "127.0.0.1:5180", ...init.headers } }), { dist, port: 5180, session: () => SESSION, fetcher })
  return { seen, call }
}

test("/api/me reports exactly the dev session's actor and scope, never its token", async () => {
  const { call } = studio()
  const response = await call("/api/me")
  const text = await response.text()
  expect(JSON.parse(text)).toEqual({ user: { id: "user:dev", name: "user:dev", email: "" }, organization: { id: "local", name: "shop", role: "local developer" }, environmentId: "dev" })
  expect(text).not.toContain("secret-dev-token")
})

test("/api/host/* reaches the dev host as the session, with the path, query, method and body unchanged, and its answer as sent", async () => {
  const { seen, call } = studio()
  const response = await call("/api/host/v1/actions/shop.product.add?orgId=local&environmentId=dev", { method: "POST", headers: { "content-type": "application/json", origin: "http://127.0.0.1:5180" }, body: '{"input":{"price":19.990}}' })
  expect(response.status).toBe(404)
  expect(await response.json()).toEqual({ code: "object.not_found" })
  expect(seen).toEqual([{ url: "http://127.0.0.1:4174/v1/actions/shop.product.add?orgId=local&environmentId=dev", method: "POST", authorization: "Bearer secret-dev-token", body: '{"input":{"price":19.990}}' }])
})

test("another site's request, or one for another host name, is refused before anything is forwarded", async () => {
  const { seen, call } = studio()
  expect((await call("/api/host/v1/definitions", { method: "POST", headers: { origin: "https://evil.example" } })).status).toBe(403)
  expect((await call("/api/host/v1/definitions", { headers: { "sec-fetch-site": "cross-site" } })).status).toBe(403)
  expect((await call("/api/me", { headers: { host: "rebound.example:5180" } })).status).toBe(403)
  expect((await call("/api/me", { headers: { host: "127.0.0.1:5181" } })).status).toBe(403)
  expect(seen).toEqual([])
  expect((await call("/api/me", { headers: { host: "localhost:5180", "sec-fetch-site": "same-origin" } })).status).toBe(200)
})

test("other /api routes are not served; the app's files are, and every other path is the app's page", async () => {
  const { seen, call } = studio()
  expect((await call("/api/agent")).status).toBe(404)
  expect(await (await call("/assets/app.js")).text()).toBe("console.log(1)")
  expect(await (await call("/sources/canon.Item/demo/item/gold")).text()).toContain("<div id=root>")
  // A path out of the app's directory is not a file of it.
  expect(await (await call("/%2e%2e/outside.txt")).text()).toContain("<div id=root>")
  expect((await call("/", { method: "POST" })).status).toBe(405)
  expect(seen).toEqual([])
})

test("a stopped norn dev is a 503 that says so", async () => {
  const response = await localStudio(new Request("http://127.0.0.1:5180/api/me", { headers: { host: "127.0.0.1:5180" } }),
    { dist, port: 5180, session: () => { throw new LocalCellError("No running norn dev in /p") } })
  expect(response.status).toBe(503)
  expect(await response.json()).toEqual({ error: "No running norn dev in /p" })
})

/** A socket that records what the hub sent it. */
function socket() {
  const sent: Record<string, unknown>[] = []
  const closed: number[] = []
  const ws: LiveSocket = { send: (data: string) => sent.push(JSON.parse(data)), close: (code?: number) => closed.push(code ?? 0) }
  return { ws, sent, closed }
}

test("live reads: a subscription gets the dev host's GET answer, then only a changed one", async () => {
  let answer = { status: 200, body: '{"total":1}' }
  const asked: string[] = []
  const hub = new LiveHub(async (path) => { asked.push(path); return answer })
  const { ws, sent } = socket()
  hub.open(ws)
  await hub.message(ws, JSON.stringify({ type: "subscribe", id: "objects", path: "/v1/objects/canon.Delivery?orgId=local&environmentId=dev" }))
  expect(asked).toEqual(["/v1/objects/canon.Delivery?orgId=local&environmentId=dev"])
  expect(sent).toEqual([expect.objectContaining({ type: "snapshot", id: "objects", status: 200, body: { total: 1 } })])
  await hub.publish()
  expect(sent).toHaveLength(1)
  answer = { status: 200, body: '{"total":2}' }
  await hub.publish()
  expect(sent).toHaveLength(2)
  expect(sent[1]).toMatchObject({ type: "snapshot", id: "objects", body: { total: 2 } })
  await hub.message(ws, JSON.stringify({ type: "unsubscribe", id: "objects" }))
  answer = { status: 200, body: '{"total":3}' }
  await hub.publish()
  expect(sent).toHaveLength(2)
})

test("live reads refuse what the hosted cell refuses, and close on an expired token", async () => {
  expect(subscribable("/v1/subscribe?x=1")).toBeNull()
  expect(subscribable("/api/me")).toBeNull()
  expect(subscribable("/v1/objects/a/../../x")).toBe("/v1/x")
  // Dot segments are resolved before the path is decided.
  expect(subscribable("/v1/x/../subscribe")).toBeNull()
  expect(subscribable("/v1/../../api/me")).toBeNull()
  expect(subscribable("/v1/%2e%2e/api/me")).toBeNull()
  expect(subscribable("/v1/%2E%2E/subscribe")).toBeNull()
  expect(subscribable("/v1/..\\api/me")).toBeNull()
  const hub = new LiveHub(async () => ({ status: 401, body: "{}" }))
  const { ws, sent, closed } = socket()
  hub.open(ws)
  await hub.message(ws, JSON.stringify({ type: "subscribe", id: "bad id!", path: "/v1/objects" }))
  await hub.message(ws, JSON.stringify({ type: "subscribe", id: "live", path: "/v1/subscribe" }))
  await hub.message(ws, JSON.stringify({ type: "poke", id: "x" }))
  expect(sent.map((message) => message["code"])).toEqual(["live.invalid_id", "live.not_subscribable", "live.unknown_message"])
  await hub.message(ws, JSON.stringify({ type: "subscribe", id: "a", path: "/v1/objects" }))
  expect(closed).toEqual([4401])
  expect(hub.size).toBe(0)
})

test("at most eight subscriptions per socket", async () => {
  const hub = new LiveHub(async () => ({ status: 200, body: "{}" }))
  const { ws, sent } = socket()
  hub.open(ws)
  for (let i = 0; i < 9; i++) await hub.message(ws, JSON.stringify({ type: "subscribe", id: `s${i}`, path: `/v1/objects/t${i}` }))
  expect(sent.filter((message) => message["type"] === "snapshot")).toHaveLength(8)
  expect(sent.at(-1)).toMatchObject({ type: "refused", id: "s8", code: "live.too_many_subscriptions" })
})

test("the local Studio's live socket: same-site only, answered as the session, and re-asked after a write it forwarded", async () => {
  let total = 1
  const seen: { url: string; authorization: string | null; method: string }[] = []
  const fetcher = (async (input: string | URL | Request, init?: RequestInit) => {
    seen.push({ url: String(input), authorization: new Headers(init?.headers).get("authorization"), method: init?.method ?? "GET" })
    if ((init?.method ?? "GET") === "POST") { total += 1; return Response.json({ outcome: "committed" }) }
    return Response.json({ total })
  }) as typeof fetch
  const local = serveLocal({ dist, port: 0, session: () => SESSION, fetcher, intervalMs: 60_000 })
  try {
    const port = local.server.port
    const base = `http://127.0.0.1:${port}`
    // Another site's upgrade is refused before anything is forwarded.
    const crossSite = await fetch(`${base}/api/host/v1/subscribe`, { headers: { upgrade: "websocket", connection: "upgrade", origin: "https://evil.example", "sec-websocket-key": "dGhlIHNhbXBsZSBub25jZQ==", "sec-websocket-version": "13" } })
    expect(crossSite.status).toBe(403)
    expect(seen).toEqual([])
    const ws = new WebSocket(`ws://127.0.0.1:${port}/api/host/v1/subscribe?orgId=local&environmentId=dev`, [LIVE_PROTOCOL])
    const messages: Record<string, unknown>[] = []
    const next = (count: number) => new Promise<void>((done, fail) => {
      const deadline = setTimeout(() => fail(new Error(`waited for ${count} messages: ${JSON.stringify(messages)}`)), 5_000)
      const check = () => { if (messages.length >= count) { clearTimeout(deadline); done() } else setTimeout(check, 10) }
      check()
    })
    ws.addEventListener("message", (event) => messages.push(JSON.parse(String(event.data))))
    await new Promise((done) => ws.addEventListener("open", done))
    expect(ws.protocol).toBe(LIVE_PROTOCOL)
    ws.send(JSON.stringify({ type: "subscribe", id: "count", path: "/v1/objects/canon.Delivery?orgId=local&environmentId=dev" }))
    await next(1)
    expect(messages[0]).toMatchObject({ type: "snapshot", id: "count", body: { total: 1 } })
    expect(seen[0]).toEqual({ url: "http://127.0.0.1:4174/v1/objects/canon.Delivery?orgId=local&environmentId=dev", authorization: "Bearer secret-dev-token", method: "GET" })
    const write = await fetch(`${base}/api/host/v1/actions/shop.product.add?orgId=local&environmentId=dev`, { method: "POST", headers: { "content-type": "application/json", origin: base }, body: "{}" })
    expect(write.status).toBe(200)
    await next(2)
    expect(messages[1]).toMatchObject({ type: "snapshot", id: "count", body: { total: 2 } })
    ws.close()
  } finally { local.stop() }
})
