import { expect, test } from "bun:test"
import { NotServed, liveTransport, readMode } from "../src/lib/live-transport.ts"

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

test("a cell that answers its definitions is live, with the artifacts and their exact numbers", async () => {
  const { fetcher, calls } = server({ "/v1/definitions": [200, DEFINITIONS] })
  const mode = await readMode(fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  expect(mode.catalog.origin).toBe("live")
  expect(mode.catalog.document.map((artifact) => artifact.artifactId)).toEqual(["shop.Product.entity"])
  expect(mode.catalog.numbers.get("/0/payload/scale")).toBe("2.50")
  expect(calls).toEqual(["/api/host/v1/definitions?orgId=local&environmentId=dev"])
})

test("no session, no cell, no host or a page that is not a document reads the recording; a refusal is the cell's error", async () => {
  expect(await readMode(server({}, [401, { error: "Sign in first." }]).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({}, [200, { ...ME, environmentId: null }]).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({}, [200, "<!doctype html>"]).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({ "/v1/definitions": [503, '{"error":"No Norn host is connected to this deployment."}'] }).fetcher)).toEqual({ kind: "recorded" })
  expect(await readMode(server({ "/v1/definitions": [200, "<!doctype html>"] }).fetcher)).toEqual({ kind: "recorded" })
  await expect(readMode(server({ "/v1/definitions": [403, '{"code":"authz.denied","message":"the read request is not authorized"}'] }).fetcher)).rejects.toThrow("the read request is not authorized")
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
