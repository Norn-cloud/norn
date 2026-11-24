import { expect, test } from "bun:test"
import { NotServed, ScopeSupersededError, currentMode, liveTransport, readMode, refreshMode } from "../src/lib/live-transport.ts"

const ME = { user: { id: "user:dev", name: "user:dev", email: "" }, organization: { id: "local", name: "shop", role: "local developer" }, environmentId: "dev" }
const DEFINITIONS = '{"admitted":true,"version":3,"artifacts":[{"artifactId":"shop.Product.entity","version":"0.1.0","payload":{"scale":2.50}}]}'

/** A fetch answering /api/me and each host path from `routes` (path without scope → [status, body]). */
function server(routes: Record<string, readonly [number, string]>, me: readonly [number, unknown] = [200, ME]) {
  const calls: string[] = []
  const fetcher = async (input: string) => {
    if (input === "/api/me") return Response.json(me[1], { status: me[0] })
    calls.push(input)
    const path = input.replace(/^\/api\/host/, "").replace(/[?&]orgId=local&environmentId=dev$/, "")
    const [status, body] = routes[path] ?? [404, '{"code":"route.not_found"}']
    return new Response(body, { status, headers: { "content-type": body.startsWith("<") ? "text/html" : "application/json" } })
  }
  return { calls, fetcher }
}

test("a cell that answers its definitions is live, with its scope, artifacts and exact numbers", async () => {
  const { fetcher, calls } = server({ "/v1/definitions": [200, DEFINITIONS] })
  const mode = await readMode(fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  expect(mode.scope).toEqual({ orgId: "local", environmentId: "dev" })
  expect(mode.catalog.origin).toBe("live")
  expect(mode.catalog.document.map((artifact) => artifact.artifactId)).toEqual(["shop.Product.entity"])
  expect(mode.catalog.numbers.get("/0/payload/scale")).toBe("2.50")
  expect(calls).toEqual(["/api/host/v1/definitions?orgId=local&environmentId=dev"])
})

test("currentMode keys cached catalogs by org + environment and rejects superseded in-flight reads (S1)", async () => {
  refreshMode()
  const scopeA = { orgId: "org_demo", environmentId: "env-a" }
  const scopeB = { orgId: "org_demo", environmentId: "env-b" }
  const calls: string[] = []
  let releaseA!: () => void
  const gateA = new Promise<void>((resolve) => { releaseA = resolve })

  const fetcher = async (input: string) => {
    calls.push(input)
    if (input.includes("environmentId=env-a")) {
      await gateA
      return new Response('{"admitted":true,"version":1,"artifacts":[{"artifactId":"learning.Episode.entity","kind":"entity","lifecycle":"active","payload":{"objectType":{"name":"learning.Episode"}}}]}', { status: 200 })
    }
    return new Response('{"admitted":true,"version":2,"artifacts":[]}', { status: 200 })
  }

  // Start env-a catalog read, then switch scope to env-b before env-a finishes.
  const inflightA = currentMode(fetcher, scopeA)
  const modeB = await currentMode(fetcher, scopeB)
  if (modeB.kind !== "live") throw new Error("expected live env-b")
  expect(modeB.scope).toEqual(scopeB)
  expect(modeB.catalog.document).toEqual([])

  // Late resolution of env-a must be superseded and never replace env-b's cached mode.
  releaseA()
  await expect(inflightA).rejects.toBeInstanceOf(ScopeSupersededError)
  const cachedB = await currentMode(fetcher, scopeB)
  expect(cachedB).toBe(modeB)
  expect(calls).toEqual([
    "/api/host/v1/definitions?orgId=org_demo&environmentId=env-a",
    "/api/host/v1/definitions?orgId=org_demo&environmentId=env-b",
  ])
  refreshMode()
})

test.each([
  { selectedEnvironment: "env-b", discoveryStatus: 200 },
  { selectedEnvironment: "env-b", discoveryStatus: 404 },
  { selectedEnvironment: "dev", discoveryStatus: 200 },
])("unkeyed discovery with status $discoveryStatus respects the selected $selectedEnvironment scope", async ({ selectedEnvironment, discoveryStatus }) => {
  refreshMode()
  const scopeA = { orgId: "local", environmentId: "dev" }
  const selected = { orgId: "local", environmentId: selectedEnvironment }
  let startDiscovery!: () => void
  let releaseDiscovery!: () => void
  const started = new Promise<void>((resolve) => { startDiscovery = resolve })
  const release = new Promise<void>((resolve) => { releaseDiscovery = resolve })
  const discover = async (input: string) => {
    if (input === "/api/me") return Response.json(ME)
    expect(input).toBe("/api/host/v1/definitions?orgId=local&environmentId=dev")
    startDiscovery()
    await release
    return new Response(DEFINITIONS, { status: discoveryStatus })
  }
  const pending = currentMode(discover)
  await started
  try {
    const keyedFetch = async () => new Response(DEFINITIONS)
    const keyed = await currentMode(keyedFetch, selected)
    releaseDiscovery()
    if (selectedEnvironment === scopeA.environmentId) {
      await expect(pending).resolves.toMatchObject({ kind: "live", scope: scopeA })
    } else {
      await expect(pending).rejects.toBeInstanceOf(ScopeSupersededError)
    }
    expect(await currentMode(keyedFetch)).toBe(keyed)
  } finally {
    releaseDiscovery()
    refreshMode()
  }
})

test("the catalog's per-type counts are the cell's, kept only when well formed; none when it sends none", async () => {
  const counted = '{"admitted":true,"version":3,"artifacts":[],"objectCounts":{"shop.Product":{"count":12,"approximate":false},"shop.Order":{"count":500,"approximate":true},"shop.Bad":{"count":-1,"approximate":false},"shop.Odd":{"count":3}}}'
  const mode = await readMode(server({ "/v1/definitions": [200, counted] }).fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  expect(mode.counts).toEqual({ "shop.Product": { count: 12, approximate: false }, "shop.Order": { count: 500, approximate: true } })
  expect(await liveTransport(mode.catalog, server({}).fetcher, mode.counts).objectCounts()).toEqual(mode.counts)
  const plain = await readMode(server({ "/v1/definitions": [200, DEFINITIONS] }).fetcher)
  expect(plain.kind === "live" && plain.counts).toEqual({})
  expect(plain.kind === "live" && plain.publishedQuestions.size).toBe(0)

  const withQuestions = '{"admitted":true,"version":3,"artifacts":[],"publishedQuestions":["learning.misfit_by_law","",42,"learning.triage_over_time"]}'
  const qMode = await readMode(server({ "/v1/definitions": [200, withQuestions] }).fetcher)
  if (qMode.kind !== "live") throw new Error("expected live")
  expect([...qMode.publishedQuestions]).toEqual(["learning.misfit_by_law", "learning.triage_over_time"])
})

test("no session, no cell, no host or a page that is not a document reads the recording; a refusal is the cell's error", async () => {
  expect(await readMode(server({}, [401, { error: "Sign in first." }]).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({}, [200, { ...ME, environmentId: null }]).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({}, [200, "<!doctype html>"]).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({ "/v1/definitions": [503, '{"error":"No Norn host is connected to this deployment."}'] }).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({ "/v1/definitions": [404, "<!doctype html>"] }).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({ "/v1/definitions": [200, "<!doctype html>"] }).fetcher)).toEqual({ kind: "recorded" })
  // An outage is not "nothing connected": a session server that fails (5xx) or cannot be reached is raised, and retried.
  await expect(readMode(server({}, [503, { error: "upstream unavailable" }]).fetcher)).rejects.toThrow("did not answer (503)")
  await expect(readMode(async () => { throw new TypeError("Failed to fetch") })).rejects.toThrow("did not answer (network)")
  await expect(readMode(server({ "/v1/definitions": [503, '{"error":"norn dev at http://127.0.0.1:4174 did not answer; is it still running?"}'] }).fetcher)).rejects.toThrow("did not answer")
  await expect(readMode(server({ "/v1/definitions": [403, '{"code":"authz.denied","message":"the read request is not authorized"}'] }).fetcher)).rejects.toThrow("the read request is not authorized")
})

test("an HTML catalog outage rejects the mode read and the next attempt can connect", async () => {
  refreshMode()
  const { fetcher } = server({ "/v1/definitions": [502, "<!doctype html>Bad Gateway"] })
  const recovered = server({ "/v1/definitions": [200, DEFINITIONS] })
  try {
    await expect(currentMode(fetcher)).rejects.toThrow("did not answer (502)")
    await expect(currentMode(recovered.fetcher)).resolves.toMatchObject({ kind: "live" })
  } finally {
    refreshMode()
  }
})

const live = async (routes: Record<string, readonly [number, string]>) => {
  const { fetcher, calls } = server({ "/v1/definitions": [200, DEFINITIONS], ...routes })
  const mode = await readMode(fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  return { transport: liveTransport(mode.catalog, fetcher), calls }
}

const OBJECT = '{"objectType":"shop.Product","id":"SKU-1","scope":"local/dev","version":1,"values":{"price":19.990,"sku":"SKU-1"},"withheld":false,"withheldProperties":[]}'

test("an object reads with its links (a `next` cursor is `hasMore`) and its lineage; a 404 lineage is no source", async () => {
  const { transport } = await live({
    "/v1/objects/shop.Product/SKU-1": [200, OBJECT],
    "/v1/objects/shop.Product/SKU-1/links": [200, '{"objectType":"shop.Product","id":"SKU-1","items":[{"sourceType":"shop.OrderLine","link":"product","source":"L-1","targetType":"shop.Product","target":"SKU-1","version":1}],"next":"L-1"}'],
    "/v1/objects/shop.Product/SKU-1/lineage": [404, '{"code":"object.not_found"}'],
  })
  const reads = await transport.object("shop.Product", "SKU-1")
  expect(reads?.object.origin).toBe("live")
  expect(reads?.object.numbers.get("/values/price")).toBe("19.990")
  expect(reads?.links.document).toEqual({ objectType: "shop.Product", id: "SKU-1", items: [{ sourceType: "shop.OrderLine", link: "product", source: "L-1", targetType: "shop.Product", target: "SKU-1", version: 1 }], hasMore: true })
  expect(reads?.source).toBeNull()
})

test("an object the cell does not have is null, never a recording; reads the cell does not serve say so", async () => {
  const { transport } = await live({})
  expect(await transport.object("canon.Item", "demo/item/gold")).toBeNull()
  expect(await transport.objectPage("canon.Item")).toBeNull()
  await expect(transport.questionBoard()).rejects.toBeInstanceOf(NotServed)
  await expect(transport.guarantees()).rejects.toThrow("not served by this cell")
})

test("a refused read is the cell's error, not an empty page", async () => {
  const { transport } = await live({ "/v1/objects/shop.Product": [403, '{"code":"authz.denied","message":"the read request is not authorized"}'] })
  await expect(transport.objectPage("shop.Product")).rejects.toThrow("the read request is not authorized")
})
