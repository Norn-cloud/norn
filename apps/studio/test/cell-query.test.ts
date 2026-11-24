import { expect, test } from "bun:test"
import { MutationObserver, onlineManager, QueryClient, QueryObserver } from "@tanstack/react-query"
import { parseLiveFrame } from "../../../packages/norn-sdk/src/live.ts"
import { LiveClient, type LiveRefusal, type LiveSnapshot, type SocketLike } from "../../../packages/norn-demo-ui/src/live/client.ts"
import { actionReads, attachLive, cellMutationOptions, cellPath, cellQueryKey, cellQueryOptions, cellReadState, invalidateCellRoutes, submitCall, type CellDocument, type CellEntry, type CellRefused, type CellSubmission, type LiveSource } from "../src/lib/cell-query.ts"
import { CellUnavailable, EXPECTED_ENV_HEADER, EXPECTED_ORG_HEADER, setDisplayedScope, StudioScopeError, WORKSPACE_CHANGED_MESSAGE, type StudioScope } from "../src/lib/definitions.ts"
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

/** The version of the document an entry holds; a refused entry has none. */
const versionOf = (entry: CellEntry<{ readonly version?: number | null }> | undefined): number | null | undefined =>
  entry?.kind === "document" ? entry.document.version : undefined

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
  const stop = attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
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
  const stop = attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
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
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: mine.options.queryKey, path })
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
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 404, "gone", '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"object.not_found","category":"visibility","message":"not found","explanation":"x"}')
  // A pushed refusal is the read's data: a tagged entry, never the error state alone.
  const entry = client.getQueryData<CellEntry<unknown>>(options.queryKey)
  expect(entry?.kind).toBe("refused")
  const shown = cellReadState(client.getQueryState<CellEntry<unknown>, Error>(options.queryKey) ?? { status: "pending", data: undefined, error: null })
  expect(shown.status).toBe("error")
  expect(shown.data).toBeUndefined()
  expect((shown.error as CellRefusal).code).toBe("object.not_found")
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
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 200, "d2", record(2, "12"))
  while (cell.asked.length === 0) await Bun.sleep(1)
  release(new Response(record(1, "10.50")))
  const settled = await reading
  expect(versionOf(settled)).toBe(2)
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(2)
})

test("a refusal pushed while a read is in flight wins over the older read", async () => {
  const client = new QueryClient()
  let release: (response: Response) => void = () => {}
  const cell = httpCell(() => new Promise<Response>((resolve) => { release = resolve }))
  const options = cellQueryOptions(client, ROUTE, params, { scope, fetch: cell.fetch })
  const reading = client.fetchQuery(options)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 403, "denied", '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}')
  while (cell.asked.length === 0) await Bun.sleep(1)
  release(new Response(record(1, "10.50")))
  const settled = await reading
  expect(settled.kind).toBe("refused")
  expect((settled as CellRefused).refusal).toBeInstanceOf(CellRefusal)
  expect(client.getQueryData<CellEntry<unknown>>(options.queryKey)?.kind).toBe("refused")
})

const DENIED = '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}'

test("a refusal pushed during a refetch survives the last observer unmounting: the old document never comes back as a success", async () => {
  const client = new QueryClient()
  client.mount()
  try {
    // The cache holds v1; a refetch starts and is still pending.
    const { options } = await readOnce(client)
    let aborted = false
    const slow = { asked: [] as string[] }
    // A refetch the cell never answers; it ends only when it is aborted.
    const refetching = cellQueryOptions(client, ROUTE, params, { scope, fetch: (url, init) => new Promise<Response>((_, reject) => {
      slow.asked.push(url)
      init?.signal?.addEventListener("abort", () => { aborted = true; reject(new DOMException("Aborted", "AbortError")) })
    }) })
    const observer = new QueryObserver(client, { ...refetching, staleTime: 0, refetchOnMount: "always" })
    const unsubscribe = observer.subscribe(() => {})
    while (slow.asked.length === 0) await Bun.sleep(1)
    expect(client.getQueryState(options.queryKey)?.fetchStatus).toBe("fetching")
    const socket = liveCell()
    const path = cellPath(ROUTE, scope, params)
    const stop = attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
    // Every state the cache passes through from the refusal on.
    const seen: (CellEntry<unknown> | undefined)[] = []
    const unwatch = client.getQueryCache().subscribe((event) => { if (event.query.queryHash === observer.getCurrentQuery().queryHash) seen.push(event.query.state.data as CellEntry<unknown> | undefined) })
    socket.push(path, 403, "denied", DENIED)
    // A workspace switch unmounts the page: its live subscription and the last observer go.
    stop()
    unsubscribe()
    await Bun.sleep(5)
    expect(aborted).toBe(true)
    const state = client.getQueryState<CellEntry<unknown>, Error>(options.queryKey)
    expect(state?.fetchStatus).toBe("idle")
    expect(state?.data?.kind).toBe("refused")
    expect(seen.length).toBeGreaterThan(0)
    expect(seen.every((entry) => entry?.kind === "refused")).toBe(true)
    // Coming back to the record shows the refusal until the cell answers again.
    const back = new QueryObserver(client, { ...refetching, enabled: false })
    expect(cellReadState(back.getCurrentResult()).status).toBe("error")
    expect(cellReadState(back.getCurrentResult()).error).toBeInstanceOf(CellRefusal)
    unwatch()
  } finally {
    client.unmount()
  }
})

test("an unchanged snapshot after a failed read clears the failure: the cell confirmed the document", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client)
  // A refetch fails; TanStack keeps v1 beside the error.
  await client.fetchQuery({ ...cellQueryOptions(client, ROUTE, params, { scope, fetch: async () => { throw new TypeError("network down") } }), staleTime: 0 }).catch(() => undefined)
  expect(client.getQueryState(options.queryKey)?.status).toBe("error")
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(1)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 200, "d1", record(1, "10.50"))
  const state = client.getQueryState(options.queryKey)
  expect(state?.status).toBe("success")
  expect(state?.error).toBeNull()
  expect((state?.data as CellDocument<{ version: number }>).document.version).toBe(1)
})

test("a pushed refusal is classified as an HTTP answer is: a malformed one is unavailable, never read for its code", async () => {
  const client = new QueryClient()
  const malformed = [
    // A Worker's or proxy's error body with just a code.
    '{"code":"authz.denied"}',
    // A refusal whose reason misses the contract's members.
    '{"format":"norn/refusal@2","retryable":false,"reason":{},"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}',
    '{"format":"norn/refusal@2","retryable":false,"reason":{"law":"L1","subject":"s","values":{},"pin":"v1"},"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}',
  ]
  for (const body of malformed) {
    // Over HTTP…
    const read = await readOnce(client, scope, () => new Response(body, { status: 403 }))
    const answered = client.getQueryState(read.options.queryKey)?.error
    expect(answered, body).toBeInstanceOf(CellUnavailable)
    expect((answered as CellUnavailable).code).toBeUndefined()
    client.clear()
    // …and pushed.
    const { options } = await readOnce(client)
    const socket = liveCell()
    const path = cellPath(ROUTE, scope, params)
    attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
    socket.push(path, 403, "bad", body)
    const pushed = client.getQueryData<CellEntry<unknown>>(options.queryKey) as CellRefused
    expect(pushed.kind, body).toBe("refused")
    expect(pushed.refusal, body).toBeInstanceOf(CellUnavailable)
    expect((pushed.refusal as CellUnavailable).code).toBeUndefined()
    expect(pushed.refusal).not.toBeInstanceOf(CellRefusal)
    client.clear()
  }
  // A valid reason is a refusal on both transports.
  const reasoned = '{"format":"norn/refusal@2","retryable":false,"reason":{"law":"L1","subject":"demo.Item/i1","values":{"amount":"1"},"pin":{"kind":"builtin","id":"norn.unique","version":"1"}},"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}'
  const { options } = await readOnce(client)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 403, "ok", reasoned)
  expect(((client.getQueryData<CellEntry<unknown>>(options.queryKey) as CellRefused).refusal as CellRefusal).code).toBe("authz.denied")
})

test("a push wins over a read that fails after it", async () => {
  const client = new QueryClient()
  let fail: (error: Error) => void = () => {}
  const cell = httpCell(() => new Promise<Response>((_, reject) => { fail = reject }))
  const options = cellQueryOptions(client, ROUTE, params, { scope, fetch: cell.fetch })
  const reading = client.fetchQuery(options)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  socket.push(path, 200, "d2", record(2, "12"))
  while (cell.asked.length === 0) await Bun.sleep(1)
  fail(new TypeError("network down"))
  expect(versionOf(await reading)).toBe(2)
  expect(client.getQueryState(options.queryKey)?.status).toBe("success")
})

test("a live refusal leaves the read standing and is reported to the page", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client)
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  const refusals: LiveRefusal[] = []
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path, onLiveRefusal: (refusal) => { if (refusal !== null) refusals.push(refusal) } })
  socket.subscribed.get(path)?.onRefused?.({ code: "live.too_many_subscriptions", message: "at most 8 subscriptions per socket" })
  expect(refusals.map(({ code }) => code)).toEqual(["live.too_many_subscriptions"])
  expect(client.getQueryState(options.queryKey)?.status).toBe("success")
})

test("a malformed refusal is never trusted for its code: the read is unavailable", async () => {
  const client = new QueryClient()
  // Refusal-shaped, but no `norn/refusal@2` document: no format, and a mistyped diagnostics member.
  for (const body of [
    '{"retryable":false,"reason":null,"code":"authz.denied","category":"permission","message":"denied","explanation":"x"}',
    '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"authz.denied","category":"permission","message":"denied","explanation":"x","diagnostics":"none"}',
  ]) {
    const { options } = await readOnce(client, scope, () => new Response(body, { status: 403 }))
    const error = client.getQueryState(options.queryKey)?.error
    expect(error).toBeInstanceOf(CellUnavailable)
    expect((error as CellUnavailable).code).toBeUndefined()
    client.clear()
  }
})

test("a read in flight when an unchanged push lands is still fenced: an older answer never overwrites it", async () => {
  const client = new QueryClient()
  // The cache holds v2; a read that started before the cell pushed v2 again answers v1.
  const { options } = await readOnce(client, scope, () => new Response(record(2, "12")))
  let release: (response: Response) => void = () => {}
  const slow = httpCell(() => new Promise<Response>((resolve) => { release = resolve }))
  const again = cellQueryOptions(client, ROUTE, params, { scope, fetch: slow.fetch })
  const reading = client.fetchQuery({ ...again, staleTime: 0 })
  const socket = liveCell()
  const path = cellPath(ROUTE, scope, params)
  attachLive({ client, live: socket.live, route: ROUTE, queryKey: options.queryKey, path })
  const held = client.getQueryData(options.queryKey)
  socket.push(path, 200, "d2", record(2, "12"))
  expect(client.getQueryData(options.queryKey) === held).toBe(true)
  while (slow.asked.length === 0) await Bun.sleep(1)
  release(new Response(record(1, "10.50")))
  expect(versionOf(await reading)).toBe(2)
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(2)
})

/** A socket the real `LiveClient` drives: the test opens it, sends the cell's frames and drops it. */
class CellSocket implements SocketLike {
  readyState = 0
  readonly sent: { type: string; id: string; path?: string }[] = []
  readonly #listeners = new Map<string, ((event: { code?: number; data?: unknown }) => void)[]>()
  send(data: string) { this.sent.push(JSON.parse(data) as { type: string; id: string; path?: string }) }
  close() { this.readyState = 3 }
  addEventListener(type: string, listener: (event: { code?: number; data?: unknown }) => void) {
    this.#listeners.set(type, [...this.#listeners.get(type) ?? [], listener])
  }
  emit(type: "open" | "message" | "close", event: { code?: number; data?: unknown } = {}) {
    if (type === "open") this.readyState = 1
    if (type === "close") this.readyState = 3
    for (const listener of this.#listeners.get(type) ?? []) listener(event)
  }
  snapshot(id: string, digest: string, body: string) { this.emit("message", { data: `{"type":"snapshot","id":"${id}","status":200,"digest":"${digest}","body":${body}}` }) }
}

function realLive() {
  const sockets: CellSocket[] = []
  const live = new LiveClient({
    url: () => "wss://studio/api/host/v1/subscribe",
    connect: () => { const socket = new CellSocket(); sockets.push(socket); return socket },
    schedule: (callback) => queueMicrotask(callback),
    delay: () => 0,
  })
  const opened = async () => {
    while (sockets.at(-1)?.readyState !== 0) await Bun.sleep(1)
    const socket = sockets.at(-1) as CellSocket
    socket.emit("open")
    return socket
  }
  return { live, opened }
}

test("a replay of what the socket last held never overwrites a newer read", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client)
  const path = cellPath(ROUTE, scope, params)
  const { live, opened } = realLive()
  const stopFirst = attachLive({ client, live, route: ROUTE, queryKey: options.queryKey, path })
  const socket = await opened()
  const id = socket.sent.find(({ type }) => type === "subscribe")?.id ?? ""
  socket.snapshot(id, "d1", record(1, "10.50"))
  // A fresh read advances the shared entry to v2 while the socket still holds v1.
  await client.fetchQuery({ ...cellQueryOptions(client, ROUTE, params, { scope, fetch: httpCell(() => new Response(record(2, EXACT))).fetch }), staleTime: 0 })
  expect(live.snapshot(path)?.digest).toBe("d1")
  // A second observer mounts: the live client replays v1 to it at once.
  const stopSecond = attachLive({ client, live, route: ROUTE, queryKey: options.queryKey, path })
  const held = client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)
  expect(held?.document.version).toBe(2)
  expect(held?.numbers.get("/values/amount")).toBe(EXACT)
  // A new frame from the cell is still written.
  socket.snapshot(id, "d3", record(3, "13"))
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(3)
  stopSecond()
  stopFirst()
})

test("a live refusal clears once the cell answers the subscription again after a reconnect", async () => {
  const client = new QueryClient()
  const { options } = await readOnce(client)
  const path = cellPath(ROUTE, scope, params)
  const { live, opened } = realLive()
  const seen: (string | null)[] = []
  const stop = attachLive({ client, live, route: ROUTE, queryKey: options.queryKey, path, onLiveRefusal: (refusal) => seen.push(refusal?.code ?? null) })
  const first = await opened()
  const id = first.sent.find(({ type }) => type === "subscribe")?.id ?? ""
  first.emit("message", { data: `{"type":"refused","id":"${id}","code":"live.too_many_subscriptions","message":"at most 8 subscriptions per socket"}` })
  expect(seen).toEqual(["live.too_many_subscriptions"])
  first.emit("close", { code: 1006 })
  const second = await opened()
  expect(second.sent).toContainEqual({ type: "subscribe", id, path })
  second.snapshot(id, "d2", record(2, "12"))
  expect(seen).toEqual(["live.too_many_subscriptions", null])
  expect(client.getQueryData<CellDocument<{ version: number }>>(options.queryKey)?.document.version).toBe(2)
  stop()
})

const ACTION = "/v1/actions/{actionId}" as const
const call = { path: { actionId: "demo.item.add" }, body: { input: { amount: "1" }, idempotencyKey: "k1" } }
const inbox = () => [{ route: "/v1/inbox" }]

/** A cell that counts writes, and a mutation observer as `useCellMutation` runs it, with the page's scope in `page.scope`. */
function writer(client: QueryClient, page: { scope: StudioScope | null }, answer: () => Response = () => new Response('{"outcome":"committed"}')) {
  const posts: string[] = []
  const headers: Headers[] = []
  const bodies: string[] = []
  const fetch = async (url: string, init?: RequestInit) => { posts.push(url); headers.push(new Headers(init?.headers)); bodies.push(String(init?.body)); return answer() }
  const options = () => cellMutationOptions(client, () => page.scope, { fetch })
  const observer = new MutationObserver(client, options())
  // A render updates a pending mutation's options, as `useMutation` does.
  return { posts, headers, bodies, observer, rerender: () => observer.setOptions(options()) }
}

test("a write queued in one workspace is refused, with nothing sent, once the page moved to another", async () => {
  const client = new QueryClient()
  const page: { scope: StudioScope | null } = { scope }
  const { posts, observer, rerender } = writer(client, page)
  client.mount()
  onlineManager.setOnline(false)
  try {
    const sent = observer.mutate(submitCall(ACTION, call, page.scope, inbox)).catch((error: unknown) => error)
    await Bun.sleep(1)
    expect(observer.getCurrentResult().isPaused).toBe(true)
    // The page moves to another workspace and renders again before the write can start.
    page.scope = other
    rerender()
    client.setQueryData(cellQueryKey("/v1/inbox", other, {}), { document: {} })
    onlineManager.setOnline(true)
    const error = await sent
    expect(error).toBeInstanceOf(StudioScopeError)
    expect((error as Error).message).toBe(WORKSPACE_CHANGED_MESSAGE)
    expect(posts).toEqual([])
    expect(client.getQueryState(cellQueryKey("/v1/inbox", other, {}))?.isInvalidated).toBe(false)
  } finally {
    onlineManager.setOnline(true)
    client.unmount()
  }
})

test("a queued call runs the route it was submitted to: a plan queued offline is never sent as an apply", async () => {
  const client = new QueryClient()
  const page: { scope: StudioScope | null } = { scope }
  const { posts, bodies, observer, rerender } = writer(client, page, () => new Response('{"revision":3,"plans":[]}'))
  client.setQueryData(cellQueryKey("/v1/cell/definitions", scope, {}), { document: {} })
  client.setQueryData(cellQueryKey("/v1/inbox", scope, {}), { document: {} })
  const request = { body: { artifacts: [], expectedVersion: 3 } }
  client.mount()
  onlineManager.setOnline(false)
  try {
    const submitted = submitCall("/v1/cell/definitions:plan", request, page.scope, () => [{ route: "/v1/cell/definitions" }])
    const sent = observer.mutate(submitted)
    await Bun.sleep(1)
    expect(observer.getCurrentResult().isPaused).toBe(true)
    // The form moves on to apply, in the same workspace, and renders again; the caller's object changes too.
    request.body.expectedVersion = 4
    const apply = submitCall("/v1/cell/definitions:apply", request, page.scope, inbox)
    rerender()
    expect(Object.isFrozen(submitted.call.body)).toBe(true)
    onlineManager.setOnline(true)
    await sent
    expect(posts).toEqual(["/api/host/v1/cell/definitions:plan?orgId=org_1&environmentId=dev"])
    expect(JSON.parse(bodies[0] ?? "")).toEqual({ artifacts: [], expectedVersion: 3 })
    // The submission's own invalidation ran, not the later one's.
    expect(client.getQueryState(cellQueryKey("/v1/cell/definitions", scope, {}))?.isInvalidated).toBe(true)
    expect(client.getQueryState(cellQueryKey("/v1/inbox", scope, {}))?.isInvalidated).toBe(false)
    expect(apply.route).toBe("/v1/cell/definitions:apply")
    expect(submitted.idempotencyKey).toBeNull()
    expect(submitCall(ACTION, call, scope).idempotencyKey).toBe("k1")
  } finally {
    onlineManager.setOnline(true)
    client.unmount()
  }
})

test("a write runs in the scope it was submitted in, and invalidates that scope's reads", async () => {
  const client = new QueryClient()
  const page: { scope: StudioScope | null } = { scope }
  const { posts, headers, observer } = writer(client, page)
  client.setQueryData(cellQueryKey("/v1/inbox", scope, {}), { document: {} })
  client.setQueryData(cellQueryKey("/v1/inbox", other, {}), { document: {} })
  // One variables object submitted twice is two calls, each bound to its own scope.
  const first = submitCall(ACTION, call, scope, inbox)
  const second = submitCall(ACTION, call, other, inbox)
  expect(first).not.toBe(second)
  await observer.mutate(first)
  expect(posts).toEqual(["/api/host/v1/actions/demo.item.add?orgId=org_1&environmentId=dev"])
  // The write names the scope it expects, so Studio's Worker refuses it if the session moved (#1643).
  expect(headers[0]?.get(EXPECTED_ORG_HEADER)).toBe("org_1")
  expect(headers[0]?.get(EXPECTED_ENV_HEADER)).toBe("dev")
  expect(client.getQueryState(cellQueryKey("/v1/inbox", scope, {}))?.isInvalidated).toBe(true)
  expect(client.getQueryState(cellQueryKey("/v1/inbox", other, {}))?.isInvalidated).toBe(false)
  expect(await observer.mutate(second).catch((error: unknown) => (error as Error).message)).toBe(WORKSPACE_CHANGED_MESSAGE)
  // Variables `submitCall` did not make are refused, even shaped like a submission.
  const forged: CellSubmission<typeof ACTION> = { ...first }
  expect(await observer.mutate(forged).catch((error: unknown) => error)).toBeInstanceOf(StudioScopeError)
  // A call submitted with no scope is refused.
  expect(await observer.mutate(submitCall(ACTION, call, null)).catch((error: unknown) => error)).toBeInstanceOf(StudioScopeError)
  expect(posts).toHaveLength(1)
})

test("a write is refused before any request once the tab shows another workspace, and the Worker's stale-scope refusal is the same error", async () => {
  const client = new QueryClient()
  const page: { scope: StudioScope | null } = { scope }
  try {
    // Another tab switched the session; this tab's displayed workspace follows (#1643), the form's scope has not.
    setDisplayedScope(other)
    const stale = writer(client, page)
    const refused = await stale.observer.mutate(submitCall(ACTION, call, scope)).catch((error: unknown) => error)
    expect(refused).toBeInstanceOf(StudioScopeError)
    expect((refused as Error).message).toBe(WORKSPACE_CHANGED_MESSAGE)
    expect(stale.posts).toEqual([])
    // The tab still shows this workspace, but the session moved: the Worker refuses with 409.
    setDisplayedScope(scope)
    const moved = writer(client, page, () => new Response('{"code":"workspace.changed","message":"Your active workspace changed.","error":"Your active workspace changed."}', { status: 409 }))
    const answered = await moved.observer.mutate(submitCall(ACTION, call, scope)).catch((error: unknown) => error)
    expect(answered).toBeInstanceOf(StudioScopeError)
    expect((answered as Error).message).toBe(WORKSPACE_CHANGED_MESSAGE)
    expect(moved.posts).toHaveLength(1)
  } finally {
    setDisplayedScope(undefined)
  }
})
