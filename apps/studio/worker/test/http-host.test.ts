import { expect, test } from "bun:test"
import { cellHostOf, hostOrigin, httpHost } from "../http-host.ts"

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
