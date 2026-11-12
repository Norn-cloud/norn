import { afterAll, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { LocalCellError, localStudio, readSession, type DevSession } from "../scripts/local-cell.ts"

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
