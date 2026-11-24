import { expect, test } from "bun:test"
import { QueryClient } from "@tanstack/react-query"
import { parseLiveFrame } from "../../../packages/norn-sdk/src/live.ts"
import type { LiveRefusal, LiveSnapshot } from "../../../packages/norn-demo-ui/src/live/client.ts"
import { actionReads, attachLive, cellPath, cellQueryKey, cellQueryOptions, invalidateCellRoutes, type CellDocument, type LiveSource } from "../src/lib/cell-query.ts"
import { CellRefusal, NotServed } from "../src/lib/live-transport.ts"

const ROUTE = "/v1/objects/{type}/{id}"
const params = { path: { type: "demo.Item", id: "i1" } }
const scope = { orgId: "org_1", environmentId: "dev" }
const other = { orgId: "org_2", environmentId: "dev" }
// A 28-digit decimal: a double keeps 15 to 17 significant digits.
const EXACT = "1234567890123456.789012345678"
const record = (version: number, amount: string) =>
  `{"objectType":"demo.Item","id":"i1","scope":"org_1/dev","version":${version},"values":{"amount":${amount}},"withheld":false,"withheldProperties":[]}`

/** A stand-in cell over HTTP: answers every read with `answer()`, counting reads. */
function httpCell(answer: () => Response | Promise<Response>) {
  const asked: string[] = []
  return { asked, fetch: async (url: string) => { asked.push(url); return answer() } }
}

/** A stand-in live socket: subscriptions by path, pushes as the cell's frames. */
function liveCell() {
  const subscribed = new Map<string, { readonly listener: (snapshot: LiveSnapshot) => void; readonly onRefused?: (refusal: LiveRefusal) => void }>()
  const unsubscribed: string[] = []
  const live: LiveSource = {
    subscribe(path, listener, options) {
      subscribed.set(path, { listener, ...(options?.onRefused ? { onRefused: options.onRefused } : {}) })
      return () => { subscribed.delete(path); unsubscribed.push(path) }
    },
  }
  const push = (path: string, status: number, digest: string, body: string) => {
    const frame = parseLiveFrame(`{"type":"snapshot","id":"s1","status":${status},"digest":"${digest}","body":${body}}`)
    if (frame === null || !("numbers" in frame)) throw new Error("not a snapshot")
    subscribed.get(path)?.listener({ status: frame.message.status, digest: frame.message.digest, body: frame.message.body, numbers: frame.numbers })
  }
  return { live, subscribed, unsubscribed, push }
}

async function readOnce(client: QueryClient, at = scope, answer: () => Response | Promise<Response> = () => new Response(record(1, "10.50"))) {
  const cell = httpCell(answer)
  const options = cellQueryOptions(client, ROUTE, params, { scope: at, fetch: cell.fetch })
  await client.fetchQuery(options).catch(() => undefined)
  return { ...cell, options }
}

test("the key is [route, orgId, environmentId, params] and the read asks the scoped route through the host proxy", async () => {
  const client = new QueryClient()
  const { asked, options } = await readOnce(client)
  expect([...options.queryKey]).toEqual([ROUTE, "org_1", "dev", params])
  expect(asked).toEqual(["/api/host/v1/objects/demo.Item/i1?orgId=org_1&environmentId=dev"])
  const read = client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)
  expect(read?.document.version).toBe(1)
  expect(read?.numbers.get("/values/amount")).toBe("10.50")
})

test("a push writes the cell's document into the cache, exact decimals kept, without reading again", async () => {
  const client = new QueryClient()
  const { asked, options } = await readOnce(client)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  const stop = attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: options.queryKey, path })
  expect([...socket.subscribed.keys()]).toEqual(["/v1/objects/demo.Item/i1?orgId=org_1&environmentId=dev"])
  // The first answer after subscribing is the page's own read: the cache keeps it.
  const first = client.getQueryData(options.queryKey)
  socket.push(path, 200, "d1", record(1, "10.50"))
  expect(client.getQueryData(options.queryKey) === first).toBe(true)
  socket.push(path, 200, "d2", record(2, EXACT))
  const pushed = client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)
  expect(pushed?.document.version).toBe(2)
  expect(pushed?.numbers.get("/values/amount")).toBe(EXACT)
  expect(pushed?.digest).toBe("d2")
  expect(asked).toHaveLength(1)
  expect(client.getQueryState(options.queryKey)?.isInvalidated).toBe(false)
  stop()
})

test("unmounting unsubscribes the read's path", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  const stop = attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: options.queryKey, path })
  stop()
  expect(socket.unsubscribed).toEqual([path])
  expect(socket.subscribed.size).toBe(0)
})

test("two scopes never share a read: keys, pushes and invalidation stay in their scope", async () => {
  const client = new QueryClient()
  const mine = await readOnce(client, scope)
  const theirs = await readOnce(client, other)
  expect(mine.options.queryKey).not.toEqual(theirs.options.queryKey)
  expect(cellPath(ROUTE, other, params)).toBe("/v1/objects/demo.Item/i1?orgId=org_2&environmentId=dev")
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: mine.options.queryKey, path })
  socket.push(path, 200, "d2", record(2, "11"))
  expect(client.getQueryData<CellDocument<{ version: number }>>(mine.options.queryKey)?.document.version).toBe(2)
  expect(client.getQueryData<CellDocument<{ version: number }>>(theirs.options.queryKey)?.document.version).toBe(1)
  await invalidateCellRoutes(client, scope, actionReads("demo.Item"))
  expect(client.getQueryState(mine.options.queryKey)?.isInvalidated).toBe(true)
  expect(client.getQueryState(theirs.options.queryKey)?.isInvalidated).toBe(false)
  client.setQueryData(cellQueryKey("/v1/objects/{type}/{id}", scope, { path: { type: "demo.Other", id: "o1" } }), { document: {} })
  await invalidateCellRoutes(client, scope, actionReads("demo.Item"))
  expect(client.getQueryState(cellQueryKey("/v1/objects/{type}/{id}", scope, { path: { type: "demo.Other", id: "o1" } }))?.isInvalidated).toBe(false)
})

test("a refusal, read or pushed, is the existing refusal state", async () => {
  const client = new QueryClient()
  const denied = '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"authz.denied","category":"permission","message":"the action is not permitted","explanation":"policy"}'
  const refused = await readOnce(client, other, () => new Response(denied, { status: 403 }))
  const error = client.getQueryState(refused.options.queryKey)?.error
  expect(error).toBeInstanceOf(CellRefusal)
  expect((error as CellRefusal).code).toBe("authz.denied")
  expect((error as CellRefusal).category).toBe("permission")
  expect((error as CellRefusal).explanation).toBe("policy")
  const { options } = await readOnce(client)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 404, "gone", '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"object.not_found","category":"visibility","message":"not found","explanation":"x"}')
  const state = client.getQueryState(options.queryKey)
  expect(state?.status).toBe("error")
  expect(state?.data).toBeUndefined()
  expect((state?.error as CellRefusal).code).toBe("object.not_found")
})

test("a route the cell does not serve is not served, not a refusal", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client, scope, () => new Response("", { status: 404 }))
  expect(client.getQueryState(options.queryKey)?.error).toBeInstanceOf(NotServed)
})

test("a read already in flight when a push lands never overwrites the pushed document", async () => {
  const client = new QueryClient()
  let release: (response: Response) => void = () => {}
  const cell = httpCell(() => new Promise<Response>((resolve) => { release = resolve }))
  const options = cellQueryOptions(client, ROUTE, params, { scope, fetch: cell.fetch })
  const reading = client.fetchQuery(options)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 200, "d2", record(2, "12"))
  while (cell.asked.length === 0) await Bun.sleep(1)
  release(new Response(record(1, "10.50")))
  const settled = await reading
  expect(settled.document.version).toBe(2)
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(2)
})

test("a refusal pushed while a read is in flight wins over the older read", async () => {
  const client = new QueryClient()
  let release: (response: Response) => void = () => {}
  const cell = httpCell(() => new Promise<Response>((resolve) => { release = resolve }))
  const options = cellQueryOptions(client, ROUTE, params, { scope, fetch: cell.fetch })
  const reading = client.fetchQuery(options).catch((error: unknown) => error)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 403, "denied", '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}')
  while (cell.asked.length === 0) await Bun.sleep(1)
  release(new Response(record(1, "10.50")))
  expect(await reading).toBeInstanceOf(CellRefusal)
  expect(client.getQueryState(options.queryKey)?.data).toBeUndefined()
})

test("a live refusal leaves the read standing and is reported to the page", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  const refusals: LiveRefusal[] = []
  attachLive({ client, live: socket.live, scope, route: ROUTE, queryKey: options.queryKey, path, onRefused: (refusal) => refusals.push(refusal) })
  socket.subscribed.get(path)?.onRefused?.({ code: "live.too_many_subscriptions", message: "at most 8 subscriptions per socket" })
  expect(refusals.map(({ code }) => code)).toEqual(["live.too_many_subscriptions"])
  expect(client.getQueryState(options.queryKey)?.status).toBe("success")
})
