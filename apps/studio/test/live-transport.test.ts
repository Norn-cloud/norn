import { afterEach, expect, mock, spyOn, test } from "bun:test"
import { QueryClient, QueryObserver, queryOptions } from "@tanstack/react-query"
import { studioScopeQuery } from "../src/lib/definitions.ts"
import { NotServed, ScopeSupersededError, currentMode, liveTransport, modeQuery, readMode, refreshMode, retryMode, syncStudioScopeChange } from "../src/lib/live-transport.ts"

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
  refreshMode()
})

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

test("Retry during a stalled /api/me scope discovery cancels the stalled request and starts a replacement", async () => {
  const client = new QueryClient()
  const query = studioScopeQuery()
  const observer = new QueryObserver(client, query)
  const events: string[] = []
  let markFirstStarted = () => {}
  const firstStarted = new Promise<void>((resolve) => { markFirstStarted = resolve })
  const fetchProbe = async (...[input, init]: Parameters<typeof globalThis.fetch>) => {
    if (input !== "/api/me") return new Response(DEFINITIONS)
    if (events.length > 0) {
      events.push("replacement")
      return Response.json(ME)
    }
    events.push("first")
    return new Promise<Response>((_, reject) => {
      init?.signal?.addEventListener("abort", () => {
        events.push("aborted")
        reject(init.signal?.reason)
      }, { once: true })
      markFirstStarted()
    })
  }
  const fetch = spyOn(globalThis, "fetch").mockImplementation(
    Object.assign(fetchProbe, { preconnect: globalThis.fetch.preconnect }),
  )
  refreshMode()
  let unsubscribe: (() => void) | undefined
  try {
    unsubscribe = observer.subscribe(() => {})
    await firstStarted
    expect(events).toEqual(["first"])

    await retryMode(client)

    expect(events).toEqual(["first", "aborted", "replacement"])
    expect<unknown>(client.getQueryData(query.queryKey)).toEqual({ orgId: ME.organization.id, environmentId: ME.environmentId })
  } finally {
    unsubscribe?.()
    client.clear()
    refreshMode()
    fetch.mockRestore()
  }
})

test("Retry after a scope outage does not refetch the stale outage mode query or clobber the recovered scope", async () => {
  const client = new QueryClient()
  const scopeObs = new QueryObserver(client, studioScopeQuery())
  const outageObs = new QueryObserver(client, modeQuery(undefined, true, client))
  let meCalls = 0
  let failMe = true
  const fetchProbe = async (...[input]: Parameters<typeof globalThis.fetch>) => {
    if (input === "/api/me") {
      meCalls += 1
      if (failMe) return new Response("unavailable", { status: 503 })
      return Response.json({
        user: { id: "user:b", name: "B", email: "" },
        organization: { id: "org_b", name: "Workspace B", role: "admin" },
        environmentId: "prod",
      })
    }
    return new Response("not found", { status: 404 })
  }
  const fetch = spyOn(globalThis, "fetch").mockImplementation(
    Object.assign(fetchProbe, { preconnect: globalThis.fetch.preconnect }),
  )
  refreshMode()
  let unsubScope: (() => void) | undefined
  let unsubOutage: (() => void) | undefined
  try {
    unsubScope = scopeObs.subscribe(() => {})
    await new Promise((resolve) => setTimeout(resolve, 10))
    expect(client.getQueryState(["studio-scope"])?.status).toBe("error")

    unsubOutage = outageObs.subscribe(() => {})
    meCalls = 0
    failMe = false

    await retryMode(client)

    expect(meCalls).toBe(1)
    expect<unknown>(client.getQueryData(["studio-scope"])).toEqual({ orgId: "org_b", environmentId: "prod" })
  } finally {
    unsubOutage?.()
    unsubScope?.()
    client.clear()
    refreshMode()
    fetch.mockRestore()
  }
})

test("Retry cancels the transport's shared probe before starting its replacement", async () => {
  const client = new QueryClient()
  const query = modeQuery({ orgId: ME.organization.id, environmentId: ME.environmentId })
  const observer = new QueryObserver(client, query)
  const probes: string[] = []
  let markFirstStarted = () => {}
  const firstStarted = new Promise<void>((resolve) => { markFirstStarted = resolve })
  const fetchProbe = async (...[input, init]: Parameters<typeof globalThis.fetch>) => {
    if (input === "/api/me") return Response.json(ME)
    if (probes.length > 0) {
      probes.push("replacement")
      return new Response(DEFINITIONS)
    }
    probes.push("first")
    return new Promise<Response>((_, reject) => {
      init?.signal?.addEventListener("abort", () => {
        probes.push("aborted")
        reject(init.signal?.reason)
      }, { once: true })
      markFirstStarted()
    })
  }
  const fetch = spyOn(globalThis, "fetch").mockImplementation(
    Object.assign(fetchProbe, { preconnect: globalThis.fetch.preconnect }),
  )

  refreshMode()
  let unsubscribe: (() => void) | undefined
  try {
    const transportResult = currentMode().catch((error: unknown) => error)
    await firstStarted
    unsubscribe = observer.subscribe(() => {})
    expect(observer.getCurrentResult().fetchStatus).toBe("fetching")
    expect(probes).toEqual(["first"])

    await retryMode(client)

    expect(probes).toEqual(["first", "aborted", "replacement"])
    expect(await transportResult).toBeInstanceOf(DOMException)
    const replacement = client.getQueryData(query.queryKey)
    if (replacement?.kind !== "live") throw new Error("expected a live replacement")
    expect(await currentMode()).toBe(replacement)
  } finally {
    unsubscribe?.()
    client.clear()
    refreshMode()
    fetch.mockRestore()
  }
})

test("Retry during the session body read leaves the catalog and mode caches live", async () => {
  const client = new QueryClient()
  const query = modeQuery(undefined)
  const observer = new QueryObserver(client, query)
  const recorded = mock(async () => ({ document: [], numbers: new Map<string, string>(), origin: "recorded" as const }))
  // Mirrors transport.catalog() without importing ../src/lib/transport.ts, which other test files mock.module().
  const catalogQuery = queryOptions({
    queryKey: ["catalog"],
    queryFn: async () => {
      const active = await currentMode()
      return active.kind === "live"
        ? liveTransport(active.catalog, (input, init) => fetch(input, init), active.counts, active.scope).catalog()
        : recorded()
    },
  })
  let markBodyReading = () => {}
  const bodyReading = new Promise<void>((resolve) => { markBodyReading = resolve })
  let finishCatalog = () => {}
  const catalogReady = new Promise<void>((resolve) => { finishCatalog = resolve })
  let firstSignal: AbortSignal | undefined
  const fetchProbe = async (...[input, init]: Parameters<typeof globalThis.fetch>) => {
    if (input !== "/api/me") {
      await catalogReady
      return new Response(DEFINITIONS)
    }
    if (firstSignal !== undefined) return Response.json(ME)
    const signal = init?.signal
    if (!signal) throw new Error("expected a cancellable probe")
    firstSignal = signal
    return new Response(new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('{"organization":'))
        signal.addEventListener("abort", () => controller.error(signal.reason), { once: true })
      },
      pull() { markBodyReading() },
    }, { highWaterMark: 0 }))
  }
  const fetch = spyOn(globalThis, "fetch").mockImplementation(
    Object.assign(fetchProbe, { preconnect: globalThis.fetch.preconnect }),
  )
  refreshMode()
  let unsubscribe: (() => void) | undefined
  try {
    const firstCatalog = client.fetchQuery(catalogQuery).catch((error: unknown) => error)
    await bodyReading
    unsubscribe = observer.subscribe(() => {})
    const retry = retryMode(client)

    expect(firstSignal?.reason).toBeInstanceOf(DOMException)
    expect(await firstCatalog).toBe(firstSignal?.reason)
    expect(client.getQueryData(catalogQuery.queryKey)).toBeUndefined()
    expect(client.getQueryData(query.queryKey)).toBeUndefined()

    const nextCatalog = client.fetchQuery(catalogQuery)
    finishCatalog()
    await retry
    const catalog = await nextCatalog
    expect(catalog.origin).toBe("live")
    expect(catalog.document.map((artifact) => artifact.artifactId)).toEqual(["shop.Product.entity"])
    expect(client.getQueryData(catalogQuery.queryKey)?.origin).toBe("live")
    expect(client.getQueryData(query.queryKey)).toMatchObject({ kind: "live", catalog })
    expect(recorded).not.toHaveBeenCalled()
  } finally {
    finishCatalog()
    unsubscribe?.()
    client.clear()
    refreshMode()
    fetch.mockRestore()
  }
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
  const discoveryOutcome = pending.catch((error: unknown) => error)
  try {
    const keyedFetch = async () => new Response(DEFINITIONS)
    const next = currentMode(keyedFetch, selected)
    if (selectedEnvironment !== scopeA.environmentId) await next
    releaseDiscovery()
    if (selectedEnvironment === scopeA.environmentId) {
      expect(next).toBe(pending)
      expect(await discoveryOutcome).toMatchObject({ kind: "live", scope: scopeA })
    } else {
      expect(await discoveryOutcome).toBeInstanceOf(ScopeSupersededError)
    }
    expect(await currentMode(keyedFetch)).toBe(await next)
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
  // An outage is not "nothing connected": a session server or host that fails (5xx, HTML 502) or cannot be reached is raised, and retried.
  await expect(readMode(server({}, [503, { error: "upstream unavailable" }]).fetcher)).rejects.toThrow("did not answer (503)")
  await expect(readMode(async () => { throw new TypeError("Failed to fetch") })).rejects.toThrow("did not answer (network)")
  await expect(readMode(async (input: string) => input === "/api/me" ? Response.json(ME) : Promise.reject(new TypeError("Failed to fetch")))).rejects.toThrow("did not answer (network)")
  await expect(readMode(server({ "/v1/definitions": [502, "<html><body>502 Bad Gateway</body></html>"] }).fetcher)).rejects.toThrow("did not answer (502)")
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

test("once the mode query settles on an outage, page reads reuse its error instead of probing /v1/definitions until Retry", async () => {
  const definitionReads: string[] = []
  let gatewayDown = true
  const fetchProbe = async (...[input]: Parameters<typeof globalThis.fetch>) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(ME)
    definitionReads.push(url)
    return gatewayDown
      ? new Response("<html><body>502 Bad Gateway</body></html>", { status: 502, headers: { "content-type": "text/html" } })
      : new Response(DEFINITIONS, { headers: { "content-type": "application/json" } })
  }
  const fetch = spyOn(globalThis, "fetch").mockImplementation(Object.assign(fetchProbe, { preconnect: globalThis.fetch.preconnect }))
  const client = new QueryClient()
  try {
    const query = modeQuery({ orgId: ME.organization.id, environmentId: ME.environmentId })
    await expect(client.fetchQuery({ ...query, retryDelay: 0 })).rejects.toThrow("did not answer (502)")
    // The initial read plus MODE_AUTO_RETRIES (3) bounded retries, then the mode query settles.
    expect(definitionReads).toHaveLength(4)

    // A page read through the transport (a catalog query's own retry) gets the settled error without a new probe.
    await expect(currentMode()).rejects.toThrow("did not answer (502)")
    expect(definitionReads).toHaveLength(4)

    // Retry forgets the settled outage: the next read probes the cell again and connects.
    gatewayDown = false
    refreshMode()
    await expect(currentMode()).resolves.toMatchObject({ kind: "live" })
    expect(definitionReads).toHaveLength(5)
  } finally {
    fetch.mockRestore()
    client.clear()
  }
})

test("workspace A's settled refusal never reaches a read in workspace B, even during B's retry delay", async () => {
  const scopeA = { orgId: "org_a", environmentId: "env_a" }
  const scopeB = { orgId: "org_b", environmentId: "env_b" }
  let me: { organization: { id: string }; environmentId: string } = { ...ME, organization: { ...ME.organization, id: scopeA.orgId }, environmentId: scopeA.environmentId }
  const definitionReads: string[] = []
  let bReads = 0
  let markBFirstRead = () => {}
  const bFirstRead = new Promise<void>((resolve) => { markBFirstRead = resolve })
  const fetchProbe = async (...[input]: Parameters<typeof globalThis.fetch>) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(me)
    definitionReads.push(url)
    if (url.includes(`orgId=${scopeA.orgId}`)) {
      return Response.json(
        { code: "cell.unopenable", retryable: false, incidentId: "inc-workspace-a", explanation: "workspace A cannot open" },
        { status: 503 },
      )
    }
    bReads += 1
    if (bReads === 1) {
      markBFirstRead()
      return new Response("<html><body>502 Bad Gateway</body></html>", { status: 502, headers: { "content-type": "text/html" } })
    }
    return new Response(DEFINITIONS, { headers: { "content-type": "application/json" } })
  }
  const fetch = spyOn(globalThis, "fetch").mockImplementation(Object.assign(fetchProbe, { preconnect: globalThis.fetch.preconnect }))
  const client = new QueryClient()
  try {
    // 1. A settles on a non-retryable refusal carrying its incident ID.
    const settledA = await client.fetchQuery(modeQuery(scopeA)).then(() => undefined, (error: unknown) => error)
    expect(settledA).toMatchObject({ incidentId: "inc-workspace-a" })
    // Same scope: the transport reuses A's settled error without a new probe.
    await expect(currentMode()).rejects.toMatchObject({ incidentId: "inc-workspace-a" })
    expect(definitionReads).toHaveLength(1)

    // 2–3. B's mode query starts and its first definitions read is a transient 502, so B waits to retry.
    me = { ...me, organization: { ...me.organization, id: scopeB.orgId }, environmentId: scopeB.environmentId }
    const queryB = { ...modeQuery(scopeB), retryDelay: 60_000 }
    const modeB = client.fetchQuery(queryB).catch((error: unknown) => error)
    await bFirstRead
    // Wait until B's first attempt has failed and its probe is gone: B is now inside its retry delay.
    for (let tick = 0; (client.getQueryState(queryB.queryKey)?.fetchFailureCount ?? 0) < 1; tick += 1) {
      if (tick > 200) throw new Error("B's first mode read never failed")
      await new Promise((resolve) => setTimeout(resolve, 5))
    }
    expect(client.getQueryState(queryB.queryKey)?.fetchStatus).toBe("fetching")

    // 4. During B's retry delay, an unkeyed transport read is B's own: it never rejects with A's refusal.
    const outcome = await currentMode().then(
      (mode) => ({ mode }),
      (error: unknown) => ({ error }),
    )
    expect(JSON.stringify(outcome, (_key, value: unknown) => value instanceof Error ? { ...value, message: value.message } : value))
      .not.toContain("inc-workspace-a")
    expect(outcome).toMatchObject({ mode: { kind: "live", scope: scopeB } })
    await client.cancelQueries({ queryKey: ["studio-mode"] })
    await modeB
  } finally {
    fetch.mockRestore()
    client.clear()
  }
})

test("syncStudioScopeChange(client, undefined) immediately flips mounted scope observers to pending without writing null", () => {
  const client = new QueryClient()
  const scopeA = { orgId: "org_a", environmentId: "env_a" }
  client.setQueryData(["studio-scope"], scopeA)
  client.setQueryData(["studio-mode", JSON.stringify([scopeA.orgId, scopeA.environmentId])], { kind: "live", scope: scopeA })
  const pending = new Promise<typeof scopeA | null>(() => {})
  const observer = new QueryObserver(client, {
    queryKey: ["studio-scope"],
    queryFn: () => pending,
  })
  const unsubscribe = observer.subscribe(() => {})
  try {
    expect(observer.getCurrentResult().status).toBe("success")
    expect(observer.getCurrentResult().data).toEqual(scopeA)
    syncStudioScopeChange(client, undefined)
    const after = observer.getCurrentResult()
    expect(after.status).toBe("pending")
    expect(after.isPending).toBe(true)
    expect(after.isSuccess).toBe(false)
    expect(after.data).toBeUndefined()
    expect(client.getQueryData(["studio-scope"])).toBeUndefined()
    expect(client.getQueryData(["studio-mode", JSON.stringify([scopeA.orgId, scopeA.environmentId])])).toBeUndefined()
  } finally {
    unsubscribe()
    client.clear()
  }
})
