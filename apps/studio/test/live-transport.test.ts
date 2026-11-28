import { afterEach, expect, mock, spyOn, test } from "bun:test"
import { QueryClient, QueryObserver, onlineManager, queryOptions } from "@tanstack/react-query"
import { CellUnavailable, studioScopeQuery } from "../src/lib/definitions.ts"
import { NotServed, ScopeSupersededError, currentMode, liveTransport, modeQuery, readMode, refreshMode, retryMode, retryWorkspace, scopeAnswered, syncStudioScopeChange } from "../src/lib/live-transport.ts"
import { EMPTY_QUERY } from "../src/lib/records-query.ts"
import { workspaceOf } from "../src/lib/workspace.tsx"

const realFetch = globalThis.fetch
afterEach(() => {
  globalThis.fetch = realFetch
  refreshMode()
})

const ME = { user: { id: "user:dev", name: "user:dev", email: "" }, organization: { id: "local", name: "shop", role: "local developer" }, environmentId: "dev" }
const SCOPE_LOCAL = { orgId: ME.organization.id, environmentId: ME.environmentId }
const SCOPE_A = { orgId: "org_a", environmentId: "env_a" }
const SCOPE_B = { orgId: "org_b", environmentId: "env_b" }
const DEFINITIONS = '{"admitted":true,"version":3,"artifacts":[{"artifactId":"shop.Product.entity","version":"0.1.0","payload":{"scale":2.50}}]}'
const OBJECT = '{"objectType":"shop.Product","id":"SKU-1","scope":"local/dev","version":1,"values":{"price":19.990,"sku":"SKU-1"},"withheld":false,"withheldProperties":[]}'

const meFor = (scope: { readonly orgId: string; readonly environmentId: string }) =>
  ({ ...ME, organization: { ...ME.organization, id: scope.orgId }, environmentId: scope.environmentId })
const jsonHeaders = { "content-type": "application/json" }
const definitionsResponse = () => new Response(DEFINITIONS, { headers: jsonHeaders })
const badGateway = () => new Response("<html><body>502 Bad Gateway</body></html>", { status: 502, headers: { "content-type": "text/html" } })
const unopenable = (incidentId: string, explanation = "the cell cannot open") =>
  Response.json({ code: "cell.unopenable", retryable: false, incidentId, explanation }, { status: 503 })
const serializeOutcome = (value: unknown) =>
  JSON.stringify(value, (_key, v: unknown) => (v instanceof Error ? { ...v, message: v.message } : v))
const settled = (result: { readonly isError: boolean; readonly isFetching: boolean }) => result.isError && !result.isFetching
const terminal = (result: { readonly isError: boolean; readonly isSuccess: boolean; readonly isFetching: boolean }) =>
  (result.isError || result.isSuccess) && !result.isFetching

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

type FetchProbe = (...args: Parameters<typeof globalThis.fetch>) => Promise<Response>

async function withProbe(
  probe: FetchProbe,
  run: (ctx: {
    readonly client: QueryClient
    readonly track: (cleanup: () => void) => void
  }) => Promise<void>,
): Promise<void> {
  const fetchSpy = spyOn(globalThis, "fetch").mockImplementation(Object.assign(probe, { preconnect: globalThis.fetch.preconnect }))
  const client = new QueryClient()
  const cleanups: (() => void)[] = []
  refreshMode()
  try {
    await run({ client, track: (fn) => cleanups.push(fn) })
  } finally {
    onlineManager.setOnline(true)
    for (const fn of cleanups.reverse()) fn()
    client.clear()
    refreshMode()
    fetchSpy.mockRestore()
  }
}

function until<R>(observer: { getCurrentResult(): R; subscribe(listener: (result: R) => void): () => void }, ready: (result: R) => boolean): Promise<void> {
  return new Promise((resolve) => {
    if (ready(observer.getCurrentResult())) return resolve()
    const off = observer.subscribe((result) => {
      if (!ready(result)) return
      off()
      resolve()
    })
  })
}

async function waitForFailure(client: QueryClient, queryKey: readonly unknown[], label: string): Promise<void> {
  for (let tick = 0; (client.getQueryState(queryKey)?.fetchFailureCount ?? 0) < 1; tick += 1) {
    if (tick > 200) throw new Error(`${label}'s first mode read never failed`)
    await new Promise((resolve) => setTimeout(resolve, 5))
  }
}

function mountUseMode(client: QueryClient) {
  const scopeObs = new QueryObserver(client, studioScopeQuery())
  const options = () => {
    const scope = scopeObs.getCurrentResult()
    const outage = scope.isError && scope.error instanceof CellUnavailable && (scope.error.status === 0 || scope.error.status >= 500)
    const active = scope.isSuccess ? scope.data : null
    return { ...modeQuery(outage ? undefined : active, !scope.isPending, outage ? client : undefined), retryDelay: 0 }
  }
  let effectDeps: readonly unknown[] = []
  const render = () => {
    const scope = scopeObs.getCurrentResult()
    const active = scope.isSuccess ? scope.data : null
    const deps = [scope.isSuccess, options().queryKey[1], active]
    if (deps.some((dep, index) => !Object.is(dep, effectDeps[index]))) {
      effectDeps = deps
      if (scope.isSuccess) scopeAnswered(client, active)
    }
    modeObs.setOptions(options())
  }
  const modeObs = new QueryObserver(client, options())
  const offMode = modeObs.subscribe(() => {})
  const offScope = scopeObs.subscribe(render)
  return { scopeObs, modeObs, unmount: () => { offMode(); offScope() } }
}

test("a cell that answers its definitions is live, with its scope, artifacts and exact numbers", async () => {
  const { fetcher, calls } = server({ "/v1/definitions": [200, DEFINITIONS] })
  const mode = await readMode(fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  expect(mode.scope).toEqual(SCOPE_LOCAL)
  expect(mode.catalog.origin).toBe("live")
  expect(mode.catalog.document.map((artifact) => artifact.artifactId)).toEqual(["shop.Product.entity"])
  expect(mode.catalog.numbers.get("/0/payload/scale")).toBe("2.50")
  expect(calls).toEqual(["/api/host/v1/definitions?orgId=local&environmentId=dev"])
})

test("Retry during a stalled /api/me scope discovery cancels the stalled request and starts a replacement", async () => {
  const events: string[] = []
  const firstStarted = Promise.withResolvers<void>()
  await withProbe(async (input, init) => {
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
      firstStarted.resolve()
    })
  }, async ({ client, track }) => {
    const query = studioScopeQuery()
    const observer = new QueryObserver(client, query)
    track(observer.subscribe(() => {}))
    await firstStarted.promise
    expect(events).toEqual(["first"])

    await retryMode(client)

    expect(events).toEqual(["first", "aborted", "replacement"])
    expect<unknown>(client.getQueryData(query.queryKey)).toEqual(SCOPE_LOCAL)
  })
})

test("Retry after a scope outage does not refetch the stale outage mode query or clobber the recovered scope", async () => {
  let meCalls = 0
  let failMe = true
  await withProbe(async (input) => {
    if (input === "/api/me") {
      meCalls += 1
      return failMe ? new Response("unavailable", { status: 503 }) : Response.json(meFor({ orgId: "org_b", environmentId: "prod" }))
    }
    return new Response("not found", { status: 404 })
  }, async ({ client, track }) => {
    const scopeObs = new QueryObserver(client, studioScopeQuery())
    const outageObs = new QueryObserver(client, modeQuery(undefined, true, client))
    track(scopeObs.subscribe(() => {}))
    await until(scopeObs, terminal)
    expect(client.getQueryState(["studio-scope"])?.status).toBe("error")

    track(outageObs.subscribe(() => {}))
    meCalls = 0
    failMe = false

    await retryMode(client)

    expect(meCalls).toBe(1)
    expect<unknown>(client.getQueryData(["studio-scope"])).toEqual({ orgId: "org_b", environmentId: "prod" })
  })
})

test("the workspace Retry reads an errored page again only after the mode settles: one /api/me, no page read before it", async () => {
  const events: string[] = []
  let failMe = true
  let meCalls = 0
  await withProbe(async (input) => {
    if (input === "/api/me") {
      meCalls += 1
      events.push("me")
      return failMe ? new Response("unavailable", { status: 503 }) : Response.json(ME)
    }
    return new Response("not found", { status: 404 })
  }, async ({ client, track }) => {
    const scopeObs = new QueryObserver(client, studioScopeQuery())
    const page = new QueryObserver(client, {
      queryKey: ["page-read"],
      retry: false,
      queryFn: async () => {
        const scope = client.getQueryState(["studio-scope"])?.status
        events.push(`page:${scope}`)
        if (scope !== "success") throw new CellUnavailable(503, "unavailable")
        return "read"
      },
    })
    track(scopeObs.subscribe(() => {}))
    track(page.subscribe(() => {}))
    await until(scopeObs, terminal)
    await until(page, terminal)
    expect(client.getQueryState(["studio-scope"])?.status).toBe("error")
    expect(client.getQueryState(["page-read"])?.status).toBe("error")

    failMe = false
    meCalls = 0
    events.length = 0
    await retryWorkspace(client)

    expect(meCalls).toBe(1)
    expect(events).toEqual(["me", "page:success"])
    expect(client.getQueryState(["page-read"])?.status).toBe("success")
  })
})

test("Retry cancels the transport's shared probe before starting its replacement", async () => {
  const probes: string[] = []
  const firstStarted = Promise.withResolvers<void>()
  await withProbe(async (input, init) => {
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
      firstStarted.resolve()
    })
  }, async ({ client, track }) => {
    const query = modeQuery(SCOPE_LOCAL)
    const observer = new QueryObserver(client, query)
    const transportResult = currentMode().catch((error: unknown) => error)
    await firstStarted.promise
    track(observer.subscribe(() => {}))
    expect(observer.getCurrentResult().fetchStatus).toBe("fetching")
    expect(probes).toEqual(["first"])

    await retryMode(client)

    expect(probes).toEqual(["first", "aborted", "replacement"])
    expect(await transportResult).toBeInstanceOf(DOMException)
    const replacement = client.getQueryData(query.queryKey)
    if (replacement?.kind !== "live") throw new Error("expected a live replacement")
    expect(await currentMode()).toBe(replacement)
  })
})

test("Retry during the session body read leaves the catalog and mode caches live", async () => {
  const recorded = mock(async () => ({ document: [], numbers: new Map<string, string>(), origin: "recorded" as const }))
  // Mirrors transport.catalog() without importing ../src/lib/transport.ts, which other test files mock.module().
  const catalogQuery = queryOptions({
    queryKey: ["catalog"],
    queryFn: async () => {
      const active = await currentMode()
      return active.kind === "live" ? liveTransport(active.catalog, active.counts).catalog() : recorded()
    },
  })
  const bodyReading = Promise.withResolvers<void>()
  const catalogReady = Promise.withResolvers<void>()
  let firstSignal: AbortSignal | undefined
  await withProbe(async (input, init) => {
    if (input !== "/api/me") {
      await catalogReady.promise
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
      pull() { bodyReading.resolve() },
    }, { highWaterMark: 0 }))
  }, async ({ client, track }) => {
    track(() => catalogReady.resolve())
    const query = modeQuery(undefined)
    const observer = new QueryObserver(client, query)
    const firstCatalog = client.fetchQuery(catalogQuery).catch((error: unknown) => error)
    await bodyReading.promise
    track(observer.subscribe(() => {}))
    const retry = retryMode(client)

    expect(firstSignal?.reason).toBeInstanceOf(DOMException)
    expect(await firstCatalog).toBe(firstSignal?.reason)
    expect(client.getQueryData(catalogQuery.queryKey)).toBeUndefined()
    expect(client.getQueryData(query.queryKey)).toBeUndefined()

    const nextCatalog = client.fetchQuery(catalogQuery)
    catalogReady.resolve()
    await retry
    const catalog = await nextCatalog
    expect(catalog.origin).toBe("live")
    expect(catalog.document.map((artifact) => artifact.artifactId)).toEqual(["shop.Product.entity"])
    expect(client.getQueryData(catalogQuery.queryKey)?.origin).toBe("live")
    expect(client.getQueryData(query.queryKey)).toMatchObject({ kind: "live", catalog })
    expect(recorded).not.toHaveBeenCalled()
  })
})

test("currentMode keys cached catalogs by org + environment and rejects superseded in-flight reads (S1)", async () => {
  refreshMode()
  const scopeA = { orgId: "org_demo", environmentId: "env-a" }
  const scopeB = { orgId: "org_demo", environmentId: "env-b" }
  const calls: string[] = []
  const gateA = Promise.withResolvers<void>()
  const fetcher = async (input: string) => {
    calls.push(input)
    if (input.includes("environmentId=env-a")) {
      await gateA.promise
      return new Response('{"admitted":true,"version":1,"artifacts":[{"artifactId":"learning.Episode.entity","kind":"entity","lifecycle":"active","payload":{"objectType":{"name":"learning.Episode"}}}]}', { status: 200 })
    }
    return new Response('{"admitted":true,"version":2,"artifacts":[]}', { status: 200 })
  }

  const inflightA = currentMode(fetcher, scopeA)
  const modeB = await currentMode(fetcher, scopeB)
  if (modeB.kind !== "live") throw new Error("expected live env-b")
  expect(modeB.scope).toEqual(scopeB)
  expect(modeB.catalog.document).toEqual([])

  gateA.resolve()
  await expect(inflightA).rejects.toBeInstanceOf(ScopeSupersededError)
  expect(await currentMode(fetcher, scopeB)).toBe(modeB)
  expect(calls).toEqual([
    "/api/host/v1/definitions?orgId=org_demo&environmentId=env-a",
    "/api/host/v1/definitions?orgId=org_demo&environmentId=env-b",
  ])
})

test.each([
  { selectedEnvironment: "env-b", discoveryStatus: 200 },
  { selectedEnvironment: "env-b", discoveryStatus: 404 },
  { selectedEnvironment: "dev", discoveryStatus: 200 },
])("unkeyed discovery with status $discoveryStatus respects the selected $selectedEnvironment scope", async ({ selectedEnvironment, discoveryStatus }) => {
  refreshMode()
  const selected = { orgId: "local", environmentId: selectedEnvironment }
  const started = Promise.withResolvers<void>()
  const release = Promise.withResolvers<void>()
  const discover = async (input: string) => {
    if (input === "/api/me") return Response.json(ME)
    expect(input).toBe("/api/host/v1/definitions?orgId=local&environmentId=dev")
    started.resolve()
    await release.promise
    return new Response(DEFINITIONS, { status: discoveryStatus })
  }
  const pending = currentMode(discover)
  await started.promise
  const discoveryOutcome = pending.catch((error: unknown) => error)
  try {
    const keyedFetch = async () => new Response(DEFINITIONS)
    const next = currentMode(keyedFetch, selected)
    if (selectedEnvironment !== SCOPE_LOCAL.environmentId) await next
    release.resolve()
    if (selectedEnvironment === SCOPE_LOCAL.environmentId) {
      expect(next).toBe(pending)
      expect(await discoveryOutcome).toMatchObject({ kind: "live", scope: SCOPE_LOCAL })
    } else {
      expect(await discoveryOutcome).toBeInstanceOf(ScopeSupersededError)
    }
    expect(await currentMode(keyedFetch)).toBe(await next)
  } finally {
    release.resolve()
  }
})

test("the catalog's built-in pages are the cell's: each served artifact, none when it sends none", async () => {
  const page = { artifactId: "norn.studio.record.widget", kind: "widget", payload: {} }
  const served = JSON.stringify({ admitted: true, version: 3, artifacts: [], builtins: [{ artifact: page, sha256: "ab" }, { artifact: null }, { sha256: "cd" }, 7] })
  const mode = await readMode(server({ "/v1/definitions": [200, served] }).fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  expect(mode.builtins as readonly unknown[]).toEqual([page])
  expect(await liveTransport(mode.catalog, mode.counts, mode.builtins).builtins() as readonly unknown[]).toEqual([page])
  const plain = await readMode(server({ "/v1/definitions": [200, DEFINITIONS] }).fetcher)
  expect(plain.kind === "live" && plain.builtins).toEqual([])
})

test("the catalog's per-type counts are the cell's, kept only when well formed; none when it sends none", async () => {
  const counted = '{"admitted":true,"version":3,"artifacts":[],"objectCounts":{"shop.Product":{"count":12,"approximate":false},"shop.Order":{"count":500,"approximate":true},"shop.Bad":{"count":-1,"approximate":false},"shop.Odd":{"count":3}}}'
  const mode = await readMode(server({ "/v1/definitions": [200, counted] }).fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  expect(mode.counts).toEqual({ "shop.Product": { count: 12, approximate: false }, "shop.Order": { count: 500, approximate: true } })
  expect(await liveTransport(mode.catalog, mode.counts).objectCounts()).toEqual(mode.counts)
  const plain = await readMode(server({ "/v1/definitions": [200, DEFINITIONS] }).fetcher)
  expect(plain.kind === "live" && plain.counts).toEqual({})
  expect(plain.kind === "live" && plain.publishedQuestions.size).toBe(0)

  const withQuestions = '{"admitted":true,"version":3,"artifacts":[],"publishedQuestions":["learning.misfit_by_law","",42,"learning.triage_over_time"]}'
  const qMode = await readMode(server({ "/v1/definitions": [200, withQuestions] }).fetcher)
  if (qMode.kind !== "live") throw new Error("expected live")
  expect([...qMode.publishedQuestions]).toEqual(["learning.misfit_by_law", "learning.triage_over_time"])
})

test("no session, no cell, no host or a page that is not a document reads the recording; a refusal is the cell's error", async () => {
  for (const fetcher of [
    server({}, [401, { error: "Sign in first." }]).fetcher,
    server({}, [200, { ...ME, environmentId: null }]).fetcher,
    server({}, [200, "<!doctype html>"]).fetcher,
    server({ "/v1/definitions": [503, '{"error":"No Norn host is connected to this deployment."}'] }).fetcher,
    server({ "/v1/definitions": [404, "<!doctype html>"] }).fetcher,
    server({ "/v1/definitions": [200, "<!doctype html>"] }).fetcher,
  ]) {
    expect(await readMode(fetcher)).toEqual({ kind: "recorded" })
  }
  const outages: readonly [(input: string) => Promise<Response>, string][] = [
    [server({}, [503, { error: "upstream unavailable" }]).fetcher, "did not answer (503)"],
    [async () => { throw new TypeError("Failed to fetch") }, "did not answer (network)"],
    [async (input: string) => (input === "/api/me" ? Response.json(ME) : Promise.reject(new TypeError("Failed to fetch"))), "did not answer (network)"],
    [server({ "/v1/definitions": [502, "<html><body>502 Bad Gateway</body></html>"] }).fetcher, "did not answer (502)"],
    [server({ "/v1/definitions": [503, '{"error":"norn dev at http://127.0.0.1:4174 did not answer; is it still running?"}'] }).fetcher, "did not answer"],
    [server({ "/v1/definitions": [403, '{"code":"authz.denied","message":"the read request is not authorized"}'] }).fetcher, "the read request is not authorized"],
  ]
  for (const [fetcher, expected] of outages) {
    await expect(readMode(fetcher)).rejects.toThrow(expected)
  }
})

test("an HTML catalog outage rejects the mode read and the next attempt can connect", async () => {
  refreshMode()
  const { fetcher } = server({ "/v1/definitions": [502, "<!doctype html>Bad Gateway"] })
  const recovered = server({ "/v1/definitions": [200, DEFINITIONS] })
  await expect(currentMode(fetcher)).rejects.toThrow("did not answer (502)")
  await expect(currentMode(recovered.fetcher)).resolves.toMatchObject({ kind: "live" })
})

const live = async (routes: Record<string, readonly [number, string]>) => {
  const { fetcher, calls } = server({ "/v1/definitions": [200, DEFINITIONS], ...routes })
  const mode = await readMode(fetcher)
  if (mode.kind !== "live") throw new Error("expected live")
  const workspace = workspaceOf({ status: "success", data: mode }, fetcher)
  if (workspace.kind !== "live") throw new Error("expected a live workspace")
  return { transport: liveTransport(mode.catalog), reader: workspace.reader, calls }
}
const signal = new AbortController().signal

test("an object reads with its links (a `next` cursor is `hasMore`) and its lineage; a 404 lineage is no source", async () => {
  const { reader } = await live({
    "/v1/objects/shop.Product/SKU-1": [200, OBJECT],
    "/v1/objects/shop.Product/SKU-1/links": [200, '{"objectType":"shop.Product","id":"SKU-1","items":[{"sourceType":"shop.OrderLine","link":"product","source":"L-1","targetType":"shop.Product","target":"SKU-1","version":1}],"next":"L-1"}'],
    "/v1/objects/shop.Product/SKU-1/lineage": [404, '{"code":"object.not_found"}'],
  })
  const reads = await reader.object("shop.Product", "SKU-1").queryFn({ signal })
  expect(reads?.object.origin).toBe("live")
  expect(reads?.object.numbers.get("/values/price")).toBe("19.990")
  expect(reads?.links.document).toEqual({ objectType: "shop.Product", id: "SKU-1", items: [{ sourceType: "shop.OrderLine", link: "product", source: "L-1", targetType: "shop.Product", target: "SKU-1", version: 1 }], hasMore: true })
  expect(reads?.source).toBeNull()
})

test("an object the cell does not have is null, never a recording; reads the cell does not serve say so", async () => {
  const { transport, reader } = await live({})
  expect(await reader.object("canon.Item", "demo/item/gold").queryFn({ signal })).toBeNull()
  expect(await reader.records("canon.Item", EMPTY_QUERY).queryFn({ signal })).toEqual({ kind: "none" })
  await expect(transport.questionBoard()).rejects.toBeInstanceOf(NotServed)
  await expect(transport.guarantees()).rejects.toThrow("not served by this cell")
})

test("a refused read is the cell's error, not an empty page", async () => {
  const { reader } = await live({ "/v1/objects/shop.Product?limit=50": [403, '{"code":"authz.denied","message":"the read request is not authorized"}'] })
  expect(await reader.records("shop.Product", EMPTY_QUERY).queryFn({ signal })).toMatchObject({ kind: "refused", status: 403, message: "the read request is not authorized" })
})

test("once the mode query settles on an outage, page reads reuse its error instead of probing /v1/definitions until Retry", async () => {
  const definitionReads: string[] = []
  let gatewayDown = true
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(ME)
    definitionReads.push(url)
    return gatewayDown ? badGateway() : definitionsResponse()
  }, async ({ client }) => {
    await expect(client.fetchQuery({ ...modeQuery(SCOPE_LOCAL), retryDelay: 0 })).rejects.toThrow("did not answer (502)")
    expect(definitionReads).toHaveLength(4)

    await expect(currentMode()).rejects.toThrow("did not answer (502)")
    expect(definitionReads).toHaveLength(4)

    gatewayDown = false
    refreshMode()
    await expect(currentMode()).resolves.toMatchObject({ kind: "live" })
    expect(definitionReads).toHaveLength(5)
  })
})

test("workspace A's settled refusal never reaches a read in workspace B, even during B's retry delay", async () => {
  let me = meFor(SCOPE_A)
  const definitionReads: string[] = []
  let bReads = 0
  const bFirstRead = Promise.withResolvers<void>()
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(me)
    definitionReads.push(url)
    if (url.includes(`orgId=${SCOPE_A.orgId}`)) return unopenable("inc-workspace-a", "workspace A cannot open")
    bReads += 1
    if (bReads === 1) {
      bFirstRead.resolve()
      return badGateway()
    }
    return definitionsResponse()
  }, async ({ client }) => {
    const settledA = await client.fetchQuery(modeQuery(SCOPE_A)).then(() => undefined, (error: unknown) => error)
    expect(settledA).toMatchObject({ incidentId: "inc-workspace-a" })
    await expect(currentMode()).rejects.toMatchObject({ incidentId: "inc-workspace-a" })
    expect(definitionReads).toHaveLength(1)

    me = meFor(SCOPE_B)
    const queryB = { ...modeQuery(SCOPE_B), retryDelay: 60_000 }
    const modeB = client.fetchQuery(queryB).catch((error: unknown) => error)
    await bFirstRead.promise
    await waitForFailure(client, queryB.queryKey, "B")
    expect(client.getQueryState(queryB.queryKey)?.fetchStatus).toBe("fetching")

    const outcome = await currentMode().then((mode) => ({ mode }), (error: unknown) => ({ error }))
    expect(serializeOutcome(outcome)).not.toContain("inc-workspace-a")
    expect(outcome).toMatchObject({ mode: { kind: "live", scope: SCOPE_B } })
    await client.cancelQueries({ queryKey: ["studio-mode"] })
    await modeB
  })
})

test("outage recovery that discovers workspace B never stores B's refusal as active workspace A's settled failure", async () => {
  let me = meFor(SCOPE_A)
  const definitionReads: string[] = []
  let aReads = 0
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(me)
    definitionReads.push(url)
    if (url.includes(`orgId=${SCOPE_B.orgId}`)) return unopenable("inc-workspace-b", "workspace B cannot open")
    aReads += 1
    return aReads === 1 ? badGateway() : definitionsResponse()
  }, async ({ client }) => {
    const queryA = { ...modeQuery(SCOPE_A), retryDelay: 60_000 }
    const modeA = client.fetchQuery(queryA).catch((error: unknown) => error)
    await waitForFailure(client, queryA.queryKey, "A")
    expect(client.getQueryState(queryA.queryKey)?.fetchStatus).toBe("fetching")

    me = meFor(SCOPE_B)
    const recovered = await client.fetchQuery(modeQuery(undefined, true, client)).then(() => undefined, (error: unknown) => error)
    expect(recovered).toMatchObject({ incidentId: "inc-workspace-b" })
    expect(definitionReads.filter((url) => url.includes(`orgId=${SCOPE_B.orgId}`))).toHaveLength(1)

    me = meFor(SCOPE_A)
    const outcome = await currentMode().then((mode) => ({ mode }), (error: unknown) => ({ error }))
    expect(serializeOutcome(outcome)).not.toContain("inc-workspace-b")
    expect(outcome).toMatchObject({ mode: { kind: "live", scope: SCOPE_A } })
    await client.cancelQueries({ queryKey: ["studio-mode"] })
    await modeA
  })
})

test("outage recovery with no active workspace settles on A's refusal: page reads that discover A reuse it, and a read that discovers B is B's own (#1731 case Q)", async () => {
  let me = meFor(SCOPE_A)
  let meReads = 0
  const definitionReads: string[] = []
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      meReads += 1
      return Response.json(me)
    }
    definitionReads.push(url)
    return url.includes(`orgId=${SCOPE_A.orgId}`) ? unopenable("inc-workspace-a", "workspace A cannot open") : definitionsResponse()
  }, async ({ client }) => {
    const recovered = await client.fetchQuery(modeQuery(undefined, true, client)).then(() => undefined, (error: unknown) => error)
    expect(recovered).toMatchObject({ incidentId: "inc-workspace-a" })
    expect(definitionReads).toHaveLength(1)
    expect(meReads).toBe(1)

    for (let read = 0; read < 3; read += 1) {
      await expect(currentMode()).rejects.toMatchObject({ incidentId: "inc-workspace-a" })
    }
    expect(definitionReads).toHaveLength(1)
    expect(meReads).toBe(4)

    me = meFor(SCOPE_B)
    const outcome = await currentMode().then((mode) => ({ mode }), (error: unknown) => ({ error }))
    expect(serializeOutcome(outcome)).not.toContain("inc-workspace-a")
    expect(outcome).toMatchObject({ mode: { kind: "live", scope: SCOPE_B } })
    expect(definitionReads.filter((url) => url.includes(`orgId=${SCOPE_B.orgId}`))).toHaveLength(1)
  })
})

test("outage recovery that settles on /api/me itself records a session outage: page reads reuse it with no /api/me or definitions read until Retry (#1731 case M)", async () => {
  let sessionDown = true
  let meReads = 0
  const definitionReads: string[] = []
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      meReads += 1
      return sessionDown ? badGateway() : Response.json(ME)
    }
    definitionReads.push(url)
    return definitionsResponse()
  }, async ({ client }) => {
    const recovered = await client.fetchQuery({ ...modeQuery(undefined, true, client), retryDelay: 0 }).then(() => undefined, (error: unknown) => error)
    expect(recovered).toMatchObject({ status: 502 })
    expect(meReads).toBe(4)
    expect(definitionReads).toHaveLength(0)

    sessionDown = false
    for (let read = 0; read < 3; read += 1) {
      await expect(currentMode()).rejects.toBe(recovered)
    }
    expect(meReads).toBe(4)
    expect(definitionReads).toHaveLength(0)

    refreshMode()
    await expect(currentMode()).resolves.toMatchObject({ kind: "live" })
    expect(meReads).toBe(5)
    expect(definitionReads).toHaveLength(1)
  })
})

test("a settled session outage clears once /api/me answers for the page: the workspace it makes active reads its own mode (#1731 case M)", async () => {
  let sessionDown = true
  const definitionReads: string[] = []
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") return sessionDown ? badGateway() : Response.json(ME)
    definitionReads.push(url)
    return definitionsResponse()
  }, async ({ client }) => {
    const recovered = await client.fetchQuery({ ...modeQuery(undefined, true, client), retryDelay: 0 }).then(() => undefined, (error: unknown) => error)
    expect(recovered).toMatchObject({ status: 502 })
    sessionDown = false
    await expect(currentMode()).rejects.toBe(recovered)

    await expect(client.fetchQuery(modeQuery(SCOPE_LOCAL))).resolves.toMatchObject({ kind: "live", scope: SCOPE_LOCAL })
    await expect(currentMode()).resolves.toMatchObject({ kind: "live", scope: SCOPE_LOCAL })
    expect(definitionReads).toHaveLength(1)
  })
})

test.each([
  ["case M: the Retry settles on /api/me itself", "session"],
  ["case Q: the Retry's recovery discovers A and A refuses", "workspace"],
] as const)("a settle recorded with no active workspace clears when the scope query answers, even when A's mode is cached (#1731 %s)", async (_name, outage) => {
  const meScript: Array<"down" | "up"> = []
  let sessionDown = false
  let aRefuses = false
  let meReads = 0
  const definitionReads: string[] = []
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      meReads += 1
      const scripted = meScript.shift()
      return (scripted === undefined ? sessionDown : scripted === "down") ? badGateway() : Response.json(ME)
    }
    definitionReads.push(url)
    return aRefuses ? unopenable("inc-workspace-a", "workspace A cannot open") : definitionsResponse()
  }, async ({ client, track }) => {
    const page = mountUseMode(client)
    track(page.unmount)

    await until(page.modeObs, (result) => result.isSuccess)
    await expect(currentMode()).resolves.toMatchObject({ kind: "live", scope: SCOPE_LOCAL })

    sessionDown = true
    await page.scopeObs.refetch()
    await until(page.modeObs, settled)

    if (outage === "workspace") {
      meScript.push("down", "up")
      aRefuses = true
    }
    await retryMode(client)
    await until(page.modeObs, settled)
    expect(page.modeObs.getCurrentResult().error).toMatchObject(outage === "session" ? { status: 502 } : { incidentId: "inc-workspace-a" })

    sessionDown = false
    aRefuses = false
    await page.scopeObs.refetch()
    await until(page.modeObs, (result) => result.isSuccess && !result.isFetching)
    expect(page.modeObs.getCurrentResult().data).toMatchObject({ kind: "live", scope: SCOPE_LOCAL })

    const meBefore = meReads
    const definitionsBefore = definitionReads.length
    for (let read = 0; read < 3; read += 1) {
      await expect(currentMode()).resolves.toMatchObject({ kind: "live", scope: SCOPE_LOCAL })
    }
    expect(meReads - meBefore).toBe(1)
    expect(definitionReads.length - definitionsBefore).toBe(1)
  })
})

test.each(["definitions", "me"] as const)(
  "a recovery read held in %s when Retry runs stays the older generation's: its later success never clears the newer settle",
  async (stallStage) => {
    const definitionReads: string[] = []
    let meReads = 0
    const held = Promise.withResolvers<void>()
    const started = Promise.withResolvers<void>()
    await withProbe(async (input) => {
      const url = String(input)
      if (url === "/api/me") {
        meReads += 1
        if (stallStage === "me" && meReads === 1) {
          started.resolve()
          await held.promise
        }
        return Response.json(ME)
      }
      definitionReads.push(url)
      if (stallStage === "definitions") {
        if (definitionReads.length === 1) {
          started.resolve()
          await held.promise
          return definitionsResponse()
        }
        return unopenable("inc-newer")
      }
      return definitionReads.length === 1 ? unopenable("inc-newer") : definitionsResponse()
    }, async ({ client, track }) => {
      track(() => held.resolve())
      const recovery = client.fetchQuery(modeQuery(undefined, true, client))
      await started.promise

      refreshMode()
      await expect(client.fetchQuery(modeQuery(SCOPE_LOCAL))).rejects.toMatchObject({ incidentId: "inc-newer" })

      held.resolve()
      await expect(recovery).resolves.toMatchObject({ kind: "live" })
      expect(definitionReads).toHaveLength(2)
      await expect(currentMode()).rejects.toMatchObject({ incidentId: "inc-newer" })
      expect(definitionReads).toHaveLength(2)
    })
  },
)

test("syncStudioScopeChange(client, undefined) immediately flips mounted scope observers to pending without writing null", () => {
  const client = new QueryClient()
  client.setQueryData(["studio-scope"], SCOPE_A)
  client.setQueryData(["studio-mode", JSON.stringify([SCOPE_A.orgId, SCOPE_A.environmentId])], { kind: "live", scope: SCOPE_A })
  const observer = new QueryObserver(client, {
    queryKey: ["studio-scope"],
    queryFn: () => new Promise<typeof SCOPE_A | null>(() => {}),
  })
  const unsubscribe = observer.subscribe(() => {})
  try {
    expect(observer.getCurrentResult()).toMatchObject({ status: "success", data: SCOPE_A })
    syncStudioScopeChange(client, undefined)
    expect(observer.getCurrentResult()).toMatchObject({ status: "pending", isPending: true, isSuccess: false, data: undefined })
    expect(client.getQueryData(["studio-scope"])).toBeUndefined()
    expect(client.getQueryData(["studio-mode", JSON.stringify([SCOPE_A.orgId, SCOPE_A.environmentId])])).toBeUndefined()
  } finally {
    unsubscribe()
    client.clear()
  }
})

test.each([
  ["a pushed refusal held as the read's data (CellRefused)", { kind: "refused", refusal: new CellUnavailable(503, "unavailable", { retryable: false }), seq: 1 }],
  ["a cell call's refused result (the decisions inbox)", { result: { kind: "refused", refusal: { status: 503, code: "cell.unopenable", message: "the cell cannot open", diagnostics: [], body: { retryable: false } } } }],
] as const)("the workspace Retry reads again a read whose answer is %s, not only an errored one", async (_name, refused) => {
  const client = new QueryClient()
  const reads: string[] = []
  const observe = (key: string) => new QueryObserver(client, {
    queryKey: [key],
    staleTime: Infinity,
    retry: false,
    queryFn: async () => {
      reads.push(key)
      return { kind: "document" }
    },
  })
  client.setQueryData(["refused-read"], refused)
  client.setQueryData(["answered-read"], { kind: "document" })
  const offRefused = observe("refused-read").subscribe(() => {})
  const offAnswered = observe("answered-read").subscribe(() => {})
  try {
    expect(client.getQueryState(["refused-read"])?.status).toBe("success")
    expect(await retryWorkspace(client)).toBe(true)
    expect(reads).toEqual(["refused-read"])
    expect(client.getQueryData<unknown>(["refused-read"])).toEqual({ kind: "document" })
  } finally {
    offRefused()
    offAnswered()
    client.clear()
  }
})

test("when the workspace Retry's mode read settles in error, nothing else is read again (no fan-out to the cell that did not answer)", async () => {
  const definitionReads: string[] = []
  const pageReads: string[] = []
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(ME)
    definitionReads.push(url)
    return badGateway()
  }, async ({ client, track }) => {
    client.setQueryData(["studio-scope"], SCOPE_LOCAL)
    const mode = new QueryObserver(client, { ...modeQuery(SCOPE_LOCAL), retryDelay: 0 })
    const page = new QueryObserver(client, {
      queryKey: ["page-read"],
      retry: false,
      queryFn: async () => {
        pageReads.push("page")
        throw new CellUnavailable(502, "unavailable")
      },
    })
    track(mode.subscribe(() => {}))
    track(page.subscribe(() => {}))
    await until(mode, settled)
    await until(page, settled)
    definitionReads.length = 0
    pageReads.length = 0

    expect(await retryWorkspace(client)).toBe(false)
    expect(mode.getCurrentResult().isError).toBe(true)
    expect(definitionReads.length).toBeGreaterThan(0)
    expect(pageReads).toEqual([])
  })
})

test.each([
  ["workspace A goes down, and Retry finds the session moved to workspace B", true],
  ["the first /api/me read fails, and Retry finds workspace B", false],
] as const)("Retry after an outage: %s; one /api/me read, and nothing is read in A", async (_name, startsInA) => {
  const scopeB = { orgId: "org-b", environmentId: "env-b" }
  const meB = meFor(scopeB)
  let me: "a" | "down" | "b" = startsInA ? "a" : "down"
  let meReads = 0
  const hostReads: string[] = []
  await withProbe(async (input) => {
    const url = String(input)
    if (url === "/api/me") {
      meReads += 1
      return me === "down" ? badGateway() : Response.json(me === "a" ? ME : meB)
    }
    hostReads.push(url)
    if (me !== "b" && url.includes("orgId=local")) {
      return me === "a" ? new Response(url.includes("/v1/definitions") ? DEFINITIONS : "{}", { headers: jsonHeaders }) : badGateway()
    }
    if (url.includes("orgId=org-b")) return new Response(url.includes("/v1/definitions") ? DEFINITIONS : "{}", { headers: jsonHeaders })
    return badGateway()
  }, async ({ client, track }) => {
    const page = mountUseMode(client)
    track(page.unmount)
    const scopedA = new QueryObserver(client, {
      queryKey: ["records", "T", "local", "dev"],
      retry: false,
      enabled: startsInA,
      queryFn: async () => {
        const response = await globalThis.fetch("/api/host/v1/records?orgId=local&environmentId=dev")
        if (!response.ok) throw new CellUnavailable(response.status, "unavailable")
        return response.text()
      },
    })
    // Mirrors transport.catalog() without importing ../src/lib/transport.ts, which other test files mock.module().
    const catalog = new QueryObserver(client, {
      queryKey: ["catalog"],
      retry: false,
      queryFn: async () => {
        const active = await currentMode()
        if (active.kind !== "live") throw new Error("expected a live mode")
        return liveTransport(active.catalog, active.counts).catalog()
      },
    })
    track(scopedA.subscribe(() => {}))
    track(catalog.subscribe(() => {}))

    if (startsInA) {
      await until(page.modeObs, (result) => result.isSuccess)
      await until(scopedA, (result) => result.isSuccess)
      await until(catalog, (result) => result.isSuccess)
      me = "down"
      await page.scopeObs.refetch()
      await scopedA.refetch()
      await until(scopedA, settled)
      await until(page.modeObs, settled)
    } else {
      await until(page.modeObs, settled)
      await catalog.refetch()
      await until(catalog, settled)
    }

    me = "b"
    meReads = 0
    hostReads.length = 0
    await retryWorkspace(client)

    expect(meReads).toBe(1)
    expect(hostReads.filter((url) => url.includes("orgId=local"))).toEqual([])
    expect(client.getQueryState(["studio-mode", JSON.stringify([scopeB.orgId, scopeB.environmentId])])?.status).toBe("success")
    await until(page.modeObs, (result) => result.isSuccess && !result.isFetching)
    expect(page.modeObs.getCurrentResult().data).toMatchObject({ kind: "live", scope: scopeB })
    if (!startsInA) expect(catalog.getCurrentResult().isSuccess).toBe(true)
    expect(meReads).toBe(1)
  })
})

test("aborting one observer of a shared ModeProbe never aborts the underlying fetch for another observer", async () => {
  refreshMode()
  const gate = Promise.withResolvers<void>()
  let fetchAborted = false
  let fetchCount = 0
  const fetcher: typeof globalThis.fetch = Object.assign(
    async (_input: Parameters<typeof globalThis.fetch>[0], init?: RequestInit) => {
      fetchCount += 1
      init?.signal?.addEventListener("abort", () => { fetchAborted = true })
      await gate.promise
      return definitionsResponse()
    },
    { preconnect: globalThis.fetch.preconnect },
  )
  const a = new AbortController()
  const b = new AbortController()
  const first = currentMode(fetcher, SCOPE_LOCAL, a.signal)
  const second = currentMode(fetcher, SCOPE_LOCAL, b.signal)
  a.abort(new DOMException("observer A unmounted", "AbortError"))
  await expect(first).rejects.toThrow()
  expect(fetchAborted).toBe(false)
  gate.resolve()
  await expect(second).resolves.toMatchObject({ kind: "live", scope: SCOPE_LOCAL })
  expect(fetchCount).toBe(1)
})

test("overlapping retryWorkspace calls: superseded retry returns false and never refetches pages while the newer retry is in flight", async () => {
  let probeNumber = 0
  const secondGate = Promise.withResolvers<void>()
  const firstBegan = Promise.withResolvers<void>()
  let pageReads = 0
  await withProbe(async (input, init) => {
    if (String(input) === "/api/me") return Response.json(ME)
    probeNumber += 1
    if (probeNumber === 1) {
      return new Response('{"code":"cell.unopenable","retryable":false,"message":"down"}', { status: 503, headers: jsonHeaders })
    }
    if (probeNumber === 2) {
      firstBegan.resolve()
      return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")))
      })
    }
    await secondGate.promise
    return definitionsResponse()
  }, async ({ client, track }) => {
    track(() => secondGate.resolve())
    client.setQueryData(["studio-scope"], SCOPE_LOCAL)
    const mode = new QueryObserver(client, { ...modeQuery(SCOPE_LOCAL), retryDelay: 0 })
    const page = new QueryObserver(client, {
      queryKey: ["page-read"],
      retry: false,
      queryFn: async () => {
        pageReads += 1
        if (pageReads === 1) throw new CellUnavailable(503, "down")
        return "ok"
      },
    })
    track(mode.subscribe(() => {}))
    track(page.subscribe(() => {}))
    await until(mode, settled)
    await until(page, settled)
    pageReads = 0

    const retry1 = retryWorkspace(client)
    await firstBegan.promise
    const retry2 = retryWorkspace(client)

    expect(await retry1).toBe(false)
    expect(pageReads).toBe(0)

    secondGate.resolve()
    expect(await retry2).toBe(true)
    expect(pageReads).toBe(1)
  })
})

test("retryWorkspace returns false while offline/paused even when mode previously succeeded", async () => {
  let pageReads = 0
  await withProbe(async (input) => (String(input) === "/api/me" ? Response.json(ME) : definitionsResponse()), async ({ client, track }) => {
    client.setQueryData(["studio-scope"], SCOPE_LOCAL)
    const mode = new QueryObserver(client, { ...modeQuery(SCOPE_LOCAL), retryDelay: 0 })
    const page = new QueryObserver(client, {
      queryKey: ["page-read-offline"],
      retry: false,
      queryFn: async () => {
        pageReads += 1
        throw new CellUnavailable(502, "offline")
      },
    })
    track(mode.subscribe(() => {}))
    track(page.subscribe(() => {}))
    await until(mode, (result) => result.isSuccess)
    await until(page, settled)
    pageReads = 0

    onlineManager.setOnline(false)
    expect(await retryWorkspace(client)).toBe(false)
    expect(pageReads).toBe(0)
  })
})

test("retryWorkspace recovers errored pages even when TanStack structural sharing keeps the previous Mode reference", async () => {
  let failPage = true
  let pageReads = 0
  await withProbe(async (input) => (String(input) === "/api/me" ? Response.json(ME) : definitionsResponse()), async ({ client, track }) => {
    client.setQueryData(["studio-scope"], SCOPE_LOCAL)
    const mode = new QueryObserver(client, { ...modeQuery(SCOPE_LOCAL), retryDelay: 0 })
    const page = new QueryObserver(client, {
      queryKey: ["page-read-structural-sharing"],
      retry: false,
      queryFn: async () => {
        pageReads += 1
        if (failPage) throw new CellUnavailable(503, "transient")
        return "recovered"
      },
    })
    track(mode.subscribe(() => {}))
    track(page.subscribe(() => {}))
    await until(mode, (result) => result.isSuccess)
    await until(page, settled)
    failPage = false
    pageReads = 0

    expect(await retryWorkspace(client)).toBe(true)
    expect(pageReads).toBe(1)
    expect(page.getCurrentResult().data).toBe("recovered")
  })
})

test("retryWorkspace resolves false promptly when going offline mid-probe and when superseded during page refetch", async () => {
  let modeCalls = 0
  const secondModeGate = Promise.withResolvers<void>()
  const pageRefetchBegan = Promise.withResolvers<void>()
  const pageGate = Promise.withResolvers<void>()
  let pageCalls = 0
  await withProbe(async (input, init) => {
    if (String(input) === "/api/me") return Response.json(ME)
    modeCalls += 1
    if (modeCalls <= 2) return definitionsResponse()
    if (modeCalls === 3) {
      await secondModeGate.promise
      return definitionsResponse()
    }
    if (init?.signal?.aborted) throw new DOMException("aborted", "AbortError")
    return new Response("down", { status: 503 })
  }, async ({ client, track }) => {
    track(() => { pageGate.resolve(); secondModeGate.resolve() })
    client.setQueryData(["studio-scope"], SCOPE_LOCAL)
    const mode = new QueryObserver(client, { ...modeQuery(SCOPE_LOCAL), retryDelay: 50 })
    const page = new QueryObserver(client, {
      queryKey: ["page-read-overlap-race"],
      retry: false,
      queryFn: async () => {
        pageCalls += 1
        if (pageCalls === 1) throw new CellUnavailable(503, "down")
        pageRefetchBegan.resolve()
        await pageGate.promise
        return "ok"
      },
    })
    track(mode.subscribe(() => {}))
    track(page.subscribe(() => {}))
    await until(mode, (result) => result.isSuccess)
    await until(page, settled)

    const retry1 = retryWorkspace(client)
    await pageRefetchBegan.promise
    const retry2 = retryWorkspace(client)
    pageGate.resolve()
    expect(await retry1).toBe(false)
    secondModeGate.resolve()
    expect(await retry2).toBe(true)

    const retry3 = retryWorkspace(client)
    setTimeout(() => onlineManager.setOnline(false), 10)
    expect(await retry3).toBe(false)
  })
})

test("going offline during a scope-discovered unobserved mode probe cancels it without leaving it paused", async () => {
  let scopeReads = 0
  const firstFailure = Promise.withResolvers<void>()
  await withProbe(async (input) => {
    if (String(input) === "/api/me") {
      scopeReads += 1
      return scopeReads === 1 ? new Response("down", { status: 503 }) : Response.json(ME)
    }
    firstFailure.resolve()
    return new Response("down", { status: 503 })
  }, async ({ client, track }) => {
    const scopeObs = new QueryObserver(client, { ...studioScopeQuery(), retry: false })
    track(scopeObs.subscribe(() => {}))
    await until(scopeObs, settled)

    const retry = retryMode(client)
    await firstFailure.promise
    onlineManager.setOnline(false)
    expect(await retry).toBe(false)
    await new Promise((resolve) => setTimeout(resolve, 20))
    expect(client.getQueryState(modeQuery(SCOPE_LOCAL).queryKey)?.fetchStatus).not.toBe("paused")
  })
})
