import { expect, test } from "bun:test"
import { cellHostOf, hostOrigin, httpHost } from "../http-host.ts"

const ORIGIN = "https://alpha-cells.norn.cloud"

const recording = () => {
  const seen: Request[] = []
  const fetcher = (async (input: RequestInfo | URL, init?: RequestInit) => {
    seen.push(new Request(input, init))
    return new Response("ok")
  }) as typeof fetch
  return { seen, fetcher }
}

test("a request for the host goes to the same path and query on its origin, with its headers and body", async () => {
  const { seen, fetcher } = recording()
  const host = httpHost("https://alpha-cells.norn.cloud", fetcher)
  await host.fetch(new Request("https://norn-host/v1/definitions/a?org=o&env=dev", {
    method: "POST", headers: { authorization: "Bearer t", "content-type": "application/json" }, body: "{\"x\":1}",
  }))
  const [sent] = seen
  expect(sent?.url).toBe("https://alpha-cells.norn.cloud/v1/definitions/a?org=o&env=dev")
  expect(sent?.method).toBe("POST")
  expect(sent?.headers.get("authorization")).toBe("Bearer t")
  expect(sent?.redirect).toBe("manual")
  expect(await sent?.text()).toBe("{\"x\":1}")
})

test("a live socket's upgrade headers pass through", async () => {
  const { seen, fetcher } = recording()
  await httpHost("https://alpha-cells.norn.cloud", fetcher).fetch(new Request("https://norn-host/v1/subscribe?org=o", { headers: { upgrade: "websocket", "sec-websocket-key": "k" } }))
  expect(seen[0]?.url).toBe("https://alpha-cells.norn.cloud/v1/subscribe?org=o")
  expect(seen[0]?.headers.get("upgrade")).toBe("websocket")
})

test("only a bare https origin is a host origin", () => {
  expect(hostOrigin("https://alpha-cells.norn.cloud")).toBe("https://alpha-cells.norn.cloud")
  for (const bad of ["http://alpha-cells.norn.cloud", "https://alpha-cells.norn.cloud/", "https://alpha-cells.norn.cloud/v1", "https://u:p@alpha-cells.norn.cloud", "https://alpha-cells.norn.cloud?x=1", "alpha-cells.norn.cloud"]) {
    expect(() => hostOrigin(bad)).toThrow()
  }
})

test("a stage has one cell host: the binding, the origin, or none", async () => {
  const binding = { fetch: async () => new Response("binding") } as unknown as Fetcher
  expect(cellHostOf({ NORN_HOST: binding })).toBe(binding)
  expect(cellHostOf({})).toBeUndefined()
  const { seen, fetcher } = recording()
  await cellHostOf({ NORN_HOST_URL: "https://alpha-cells.norn.cloud" }, fetcher)?.fetch(new Request("https://norn-host/v1/me"))
  expect(seen[0]?.url).toBe("https://alpha-cells.norn.cloud/v1/me")
  expect(() => cellHostOf({ NORN_HOST: binding, NORN_HOST_URL: "https://alpha-cells.norn.cloud" })).toThrow()
})

/** A host that never answers until the call is aborted. */
const hanging = () => {
  const seen: Request[] = []
  const fetcher = ((input: RequestInfo | URL) => {
    const request = input as Request
    seen.push(request)
    return new Promise<Response>((_, reject) => {
      request.signal.addEventListener("abort", () => reject(request.signal.reason ?? new Error("aborted")), { once: true })
    })
  }) as typeof fetch
  return { seen, fetcher }
}

const refusalOf = async (response: Response) => ({ status: response.status, body: await response.json() as { code: string; retryable: boolean; category: string; explanation: string } })

test("an unreachable host is a structured 502; a read may be retried, a write is an unknown outcome", async () => {
  const failing = (async () => { throw new TypeError("connection refused") }) as unknown as typeof fetch
  const host = httpHost(ORIGIN, failing)
  const read = await refusalOf(await host.fetch(new Request("https://norn-host/v1/me")))
  expect(read.status).toBe(502)
  expect(read.body).toMatchObject({ code: "host.unreachable", category: "internal", retryable: true })
  const write = await refusalOf(await host.fetch(new Request("https://norn-host/v1/cell/definitions:apply", { method: "POST", body: "{}" })))
  expect(write.status).toBe(502)
  expect(write.body).toMatchObject({ code: "host.unreachable", retryable: false })
  expect(write.body.explanation).toContain("not retried")
})

test("a host that does not answer in time is a 504, and a timed-out write is not retried", async () => {
  const { seen, fetcher } = hanging()
  const host = httpHost(ORIGIN, fetcher, { responseMs: 20, handshakeMs: 5_000 })
  const read = await refusalOf(await host.fetch(new Request("https://norn-host/v1/me")))
  expect(read.status).toBe(504)
  expect(read.body).toMatchObject({ code: "host.timeout", retryable: true })
  const write = await refusalOf(await host.fetch(new Request("https://norn-host/v1/cell:provision", { method: "POST", body: "{}" })))
  expect(write.status).toBe(504)
  expect(write.body).toMatchObject({ code: "host.timeout", retryable: false })
  expect(write.body.explanation).toContain("may or may not have been applied")
  expect(seen.length).toBe(2)
  expect(seen.every((request) => request.signal.aborted)).toBe(true)
})

test("a socket upgrade has the handshake deadline", async () => {
  const { fetcher } = hanging()
  const host = httpHost(ORIGIN, fetcher, { responseMs: 60_000, handshakeMs: 20 })
  const started = Date.now()
  const answered = await host.fetch(new Request("https://norn-host/v1/subscribe?orgId=o", { headers: { upgrade: "websocket" } }))
  expect(answered.status).toBe(504)
  expect(Date.now() - started).toBeLessThan(5_000)
})

test("the caller's cancellation reaches the host and is not turned into a refusal", async () => {
  const { seen, fetcher } = hanging()
  const host = httpHost(ORIGIN, fetcher, { responseMs: 60_000, handshakeMs: 60_000 })
  const caller = new AbortController()
  const pending = host.fetch(new Request("https://norn-host/v1/me", { signal: caller.signal }))
  caller.abort(new Error("the reader left"))
  await expect(pending).rejects.toThrow()
  expect(seen[0]?.signal.aborted).toBe(true)
})

test("the deadline covers the response head only; a slow body still streams, and the reader can still cancel it", async () => {
  const sent: Request[] = []
  const slowBody = ((input: RequestInfo | URL) => {
    sent.push(input as Request)
    const body = new ReadableStream<Uint8Array>({
      async start(controller) {
        controller.enqueue(new TextEncoder().encode("a"))
        await new Promise((resolve) => setTimeout(resolve, 60))
        controller.enqueue(new TextEncoder().encode("b"))
        controller.close()
      },
    })
    return Promise.resolve(new Response(body))
  }) as typeof fetch
  const host = httpHost(ORIGIN, slowBody, { responseMs: 20, handshakeMs: 20 })
  const answered = await host.fetch(new Request("https://norn-host/v1/export"))
  expect(await answered.text()).toBe("ab")
  expect(sent[0]?.signal.aborted).toBe(false)
  const caller = new AbortController()
  await host.fetch(new Request("https://norn-host/v1/export", { signal: caller.signal }))
  caller.abort()
  expect(sent[1]?.signal.aborted).toBe(true)
})
