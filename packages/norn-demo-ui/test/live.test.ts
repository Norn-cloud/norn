import { expect, test } from "bun:test"
import { LiveClient, type LiveSnapshot, type SocketLike } from "../src/live/client.ts"

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
}

function harness() {
  const sockets: FakeSocket[] = []
  const timers: (() => void)[] = []
  const states: string[] = []
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
  })
  const flush = () => new Promise((resolve) => setTimeout(resolve, 0))
  return { client, sockets, timers, states, flush }
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
  expect(b).toEqual([{ status: 200, digest: "d1", body: { items: [] } }])
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
