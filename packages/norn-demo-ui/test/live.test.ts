import { expect, test } from "bun:test"
// @ts-expect-error: the host module is plain JavaScript without declarations.
import * as hostLive from "../../../apps/norn-cell-do/subscribe.mjs"
import { LiveClient, type LiveClientRefusal, type LiveRefusal, type LiveSnapshot, type SocketLike } from "../src/live/client.ts"

type Listener = (event: { code?: number; data?: unknown }) => void

class FakeSocket implements SocketLike {
  readyState = 0
  readonly sent: unknown[] = []
  readonly listeners = new Map<string, Listener[]>()
  closed: number | null = null
  constructor(readonly url: string) {}
  send(data: string) {
    this.sent.push(JSON.parse(data))
  }
  close(code = 1000) {
    this.closed = code
  }
  addEventListener(type: string, listener: Listener) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener])
  }
  emit(type: string, event: { code?: number; data?: unknown } = {}) {
    if (type === "open") this.readyState = 1
    for (const listener of this.listeners.get(type) ?? []) listener(event)
  }
  snapshot(id: string, digest: string, body: unknown) {
    this.emit("message", { data: JSON.stringify({ type: "snapshot", id, status: 200, digest, body }) })
  }
  refused(id: string | null, code: string, message: string) {
    this.emit("message", { data: JSON.stringify({ type: "refused", id, code, message }) })
  }
}

function harness() {
  const sockets: FakeSocket[] = []
  const timers: (() => void)[] = []
  const states: string[] = []
  const refusals: LiveClientRefusal[] = []
  const client = new LiveClient({
    url: () => "wss://studio/api/host/v1/subscribe",
    connect: (url) => {
      const socket = new FakeSocket(url)
      sockets.push(socket)
      return socket
    },
    schedule: (callback) => timers.push(callback),
    maxFailures: 3,
    onState: (state) => states.push(state),
    onRefused: (refusal) => refusals.push(refusal),
  })
  const flush = () => new Promise((resolve) => setTimeout(resolve, 0))
  return { client, sockets, timers, states, refusals, flush }
}

test("one wire subscription per path; every listener gets the cell's answers, a late one the last answer at once", async () => {
  const { client, sockets, flush } = harness()
  const a: LiveSnapshot[] = []
  const b: LiveSnapshot[] = []
  client.subscribe("/v1/decisions?state=pending", (s) => a.push(s))
  await flush()
  const socket = sockets[0]!
  socket.emit("open")
  expect(socket.sent).toEqual([{ type: "subscribe", id: "s1", path: "/v1/decisions?state=pending" }])
  socket.snapshot("s1", "d1", { items: [] })
  client.subscribe("/v1/decisions?state=pending", (s) => b.push(s))
  expect(socket.sent).toHaveLength(1)
  expect(b).toEqual([{ status: 200, digest: "d1", body: { items: [] }, numbers: new Map() }])
  socket.snapshot("s1", "d2", { items: [1] })
  expect(a.map((s) => s.digest)).toEqual(["d1", "d2"])
  expect(b.map((s) => s.digest)).toEqual(["d1", "d2"])
})

test("an expired token reconnects at once and subscribes again; other losses back off, then report unavailable", async () => {
  const { client, sockets, timers, states, flush } = harness()
  const unsubscribe = client.subscribe("/v1/runs/run_1", () => {})
  await flush()
  sockets[0]!.emit("open")
  sockets[0]!.emit("close", { code: 4401 })
  await flush()
  expect(sockets).toHaveLength(2)
  sockets[1]!.emit("open")
  expect(sockets[1]!.sent).toEqual([{ type: "subscribe", id: "s1", path: "/v1/runs/run_1" }])
  for (let attempt = 0; attempt < 3; attempt += 1) {
    sockets.at(-1)!.emit("close", { code: 1006 })
    timers.shift()?.()
    await flush()
  }
  expect(client.state).toBe("unavailable")
  expect(states).toContain("unavailable")
  unsubscribe()
  expect(client.state).toBe("idle")
})

test("the last listener leaving unsubscribes on the wire and closes the socket", async () => {
  const { client, sockets, flush } = harness()
  const first = client.subscribe("/v1/a", () => {})
  const second = client.subscribe("/v1/b", () => {})
  await flush()
  const socket = sockets[0]!
  socket.emit("open")
  first()
  expect(socket.sent.at(-1)).toEqual({ type: "unsubscribe", id: "s1" })
  second()
  expect(socket.closed).toBe(1000)
})

test("a resubscribed path is unsubscribed before its recovery snapshot arrives", async () => {
  const { client, sockets, flush } = harness()
  const stop = client.subscribe("/v1/a", () => {})
  const keep = client.subscribe("/v1/b", () => {})
  await flush()
  const socket = sockets[0]!
  socket.emit("open")
  socket.refused("s1", "live.too_many_subscriptions", "this socket's subscriptions are full")
  socket.emit("close", { code: 4401 })
  await flush()
  const reconnected = sockets[1]!
  reconnected.emit("open")
  expect(reconnected.sent).toContainEqual({ type: "subscribe", id: "s1", path: "/v1/a" })
  stop()
  expect(reconnected.sent.at(-1)).toEqual({ type: "unsubscribe", id: "s1" })
  expect(reconnected.closed).toBeNull()
  keep()
})

test("refused frames notify per-subscription and client-level listeners, late subscribers, and skip permanently unsubscribable paths on reconnect", async () => {
  const { client, sockets, refusals, flush } = harness()
  const subA: LiveRefusal[] = []
  const subLate: LiveRefusal[] = []
  const unsubA = client.subscribe("/v1/objects/bad", () => {}, { onRefused: (r) => subA.push(r) })
  const unsubGood = client.subscribe("/v1/objects/good", () => {})
  await flush()
  const socket = sockets[0]!
  socket.emit("open")
  socket.refused("s1", "live.not_subscribable", "a subscription names one of this cell's GET routes")
  expect(subA).toEqual([{ code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" }])
  expect(client.refusal("/v1/objects/bad")).toEqual(subA[0])
  expect(refusals).toEqual([{ id: "s1", path: "/v1/objects/bad", code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" }])

  // A late subscriber to an already-refused path learns of the refusal immediately.
  const unsubLate = client.subscribe("/v1/objects/bad", () => {}, { onRefused: (r) => subLate.push(r) })
  expect(subLate).toEqual(subA)

  // Protocol refusals with id: null still reach the client-level onRefused callback.
  socket.refused(null, "live.invalid_id", "a subscription id is 1 to 64 letters, digits or _.:-")
  expect(refusals.at(-1)).toEqual({ id: null, path: null, code: "live.invalid_id", message: "a subscription id is 1 to 64 letters, digits or _.:-" })

  // On reconnect (4401), permanently unsubscribable paths are not re-sent while valid paths re-subscribe.
  socket.emit("close", { code: 4401 })
  await flush()
  const reconnected = sockets[1]!
  reconnected.emit("open")
  expect(reconnected.sent).toEqual([{ type: "subscribe", id: "s2", path: "/v1/objects/good" }])

  // Unsubscribing a refused entry does not send a spurious wire unsubscribe.
  const sentBefore = reconnected.sent.length
  unsubA()
  unsubLate()
  expect(reconnected.sent).toHaveLength(sentBefore)
  unsubGood()
})

test("protocol-faithful: a 6-clause filtered facets path past 512 bytes is refused by subscribe.mjs and delivered by LiveClient", async () => {
  const filter = {
    all: Array.from({ length: 6 }, (_, i) => ({ property: `field_${i}`, op: "eq", value: `val_${i}` })),
  }
  const query = new URLSearchParams({
    filter: JSON.stringify(filter),
    properties: "status",
    orgId: "org_demo",
    environmentId: "analytics",
  })
  const longPath = `/v1/objects/commerce.PurchaseOrder:facets?${query}`
  expect(longPath.length).toBeGreaterThan(512)

  // Wire LiveClient directly to the real cell live protocol handler (`apps/norn-cell-do/subscribe.mjs`).
  let attachment: unknown = { scope: ["org_demo", "analytics"], authorization: "Bearer reader", subs: {}, role: "member" }
  const clientListeners = new Map<string, Listener[]>()
  const serverWs = {
    serializeAttachment: (v: unknown) => { attachment = structuredClone(v) },
    deserializeAttachment: () => structuredClone(attachment),
    send: (data: string) => {
      for (const listener of clientListeners.get("message") ?? []) listener({ data })
    },
    close: (code: number) => {
      for (const listener of clientListeners.get("close") ?? []) listener({ code })
    },
  }
  const bridgeSocket: SocketLike = {
    readyState: 0,
    send: (data: string) => {
      void hostLive.message(serverWs, data, () => ({ status: 200, body: "{}" }))
    },
    close: () => {},
    addEventListener: (type, listener) => {
      clientListeners.set(type, [...(clientListeners.get(type) ?? []), listener])
    },
  }

  const receivedSnapshots: LiveSnapshot[] = []
  const receivedRefusals: LiveRefusal[] = []
  const client = new LiveClient({
    url: () => "wss://studio/api/host/v1/subscribe?orgId=org_demo&environmentId=analytics",
    connect: () => bridgeSocket,
  })
  const unsub = client.subscribe(longPath, (s) => receivedSnapshots.push(s), { onRefused: (r) => receivedRefusals.push(r) })
  await new Promise((resolve) => setTimeout(resolve, 0))
  ;(bridgeSocket as { readyState: number }).readyState = 1
  for (const listener of clientListeners.get("open") ?? []) listener({})
  await new Promise((resolve) => setTimeout(resolve, 0))

  expect(receivedSnapshots).toEqual([])
  expect(receivedRefusals).toEqual([
    { code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" },
  ])
  expect(client.refusal(longPath)).toEqual(receivedRefusals[0])
  unsub()
})

test("LiveClient guards against frames larger than 4096 bytes so an oversize subscription cannot close the shared socket with 1009", async () => {
  const { client, sockets, flush } = harness()
  const refused: LiveRefusal[] = []
  const hugePath = `/v1/objects/commerce.PurchaseOrder:facets?filter=${"a".repeat(4200)}`
  const unsub = client.subscribe(hugePath, () => {}, { onRefused: (r) => refused.push(r) })
  await flush()
  sockets[0]!.emit("open")
  await flush()
  expect(sockets[0]!.sent).toEqual([])
  expect(refused).toHaveLength(1)
  expect(refused[0]?.code).toBe("live.not_subscribable")
  unsub()
})

test("a multibyte filter over the wire limit is refused without disrupting a healthy subscription", async () => {
  const { client, sockets, flush } = harness()
  const healthyPath = "/v1/objects/good"
  const healthy: LiveSnapshot[] = []
  const refused: LiveRefusal[] = []
  const badPath = `/v1/objects/commerce.PurchaseOrder:facets?filter=${"é".repeat(2050)}`
  const wire = JSON.stringify({ type: "subscribe", id: "s2", path: badPath })
  expect(wire.length).toBeLessThan(4096)
  expect(new TextEncoder().encode(wire).byteLength).toBeGreaterThan(4096)

  const stopHealthy = client.subscribe(healthyPath, (snapshot) => healthy.push(snapshot))
  await flush()
  const socket = sockets[0]!
  socket.emit("open")
  socket.snapshot("s1", "before", {})
  const stopBad = client.subscribe(badPath, () => {}, { onRefused: (refusal) => refused.push(refusal) })
  await flush()
  expect(socket.sent).toEqual([{ type: "subscribe", id: "s1", path: healthyPath }])
  expect(refused).toEqual([{ code: "live.not_subscribable", message: "subscription exceeds the live protocol frame limit" }])
  socket.snapshot("s1", "after", {})
  expect(healthy.map((snapshot) => snapshot.digest)).toEqual(["before", "after"])

  socket.emit("close", { code: 4401 })
  await flush()
  const reconnected = sockets[1]!
  reconnected.emit("open")
  expect(reconnected.sent).toEqual([{ type: "subscribe", id: "s1", path: healthyPath }])
  stopBad()
  stopHealthy()
})

test("same-tick subscribers to a locally refused path each receive the refusal once", async () => {
  const { client, refusals, flush } = harness()
  const path = `/v1/objects/bad?filter=${"a".repeat(4096)}`
  const first: LiveRefusal[] = []
  const second: LiveRefusal[] = []
  const stopFirst = client.subscribe(path, () => {}, { onRefused: (refusal) => first.push(refusal) })
  const stopSecond = client.subscribe(path, () => {}, { onRefused: (refusal) => second.push(refusal) })

  expect(first).toHaveLength(0)
  expect(second).toHaveLength(1)
  await flush()
  expect(first).toEqual(second)
  expect(second).toHaveLength(1)
  expect(refusals).toHaveLength(1)
  stopFirst()
  stopSecond()
})
