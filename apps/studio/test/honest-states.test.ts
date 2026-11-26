import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { ChartStyle } from "../src/components/ui/chart.tsx"
import { safeConnectorUrl } from "../src/connections.tsx"
import { safeDirectEvidenceUri, safeScanImageUrl } from "../src/evidence/evidence-crop.tsx"
import { nextUrl } from "../src/oauth.tsx"
import { CellUnavailable, fetchWithTimeout, isReadMethod, lastReachableAt, recordCellReachable, resetCellReachable, scopedHostFetch } from "../src/lib/definitions.ts"
import { CellRefusal, NotServed, cellRead, isRetryableModeError, readMode } from "../src/lib/live-transport.ts"
import { explainError, plainMetricLabel, plainWords } from "../src/lib/plain-words.ts"
import { formatLastReachable, isWorkspaceUnavailableError } from "../src/components/load-state.tsx"

const ME = { user: { id: "u" }, organization: { id: "org_1", name: "Org", role: "admin" }, environmentId: "env_1" }

test("a scoped 503 keeps refusal details on reads and the existing NotServed outcome on writes", async () => {
  const fetcher = async (input: string) => input === "/api/me"
    ? Response.json(ME)
    : Response.json({ code: "cell.unopenable", retryable: false }, { status: 503 })

  await expect(cellRead("/v1/definitions", "Definitions are", {}, fetcher)).rejects.toMatchObject({
    status: 503, code: "cell.unopenable", retryable: false,
  })
  await expect(cellRead("/v1/datasets:load", "Analysis datasets are", { method: "POST" }, fetcher)).rejects.toBeInstanceOf(NotServed)
})

test("a non-retryable cell failure (503 cell.unopenable) is never auto-retried and preserves incident metadata", async () => {
  const fetcher = async (input: string) => {
    if (input === "/api/me") return Response.json(ME)
    return Response.json({
      format: "norn/refusal@2",
      code: "cell.unopenable",
      category: "internal",
      retryable: false,
      reason: null,
      incidentId: "inc-unopenable-42",
      message: "The workspace cell could not be opened.",
      explanation: "Storage migration is paused for this environment.",
      decidedAt: "2027-01-15T08:00:00Z",
    }, { status: 503 })
  }
  let caught: unknown
  try {
    await readMode(fetcher)
  } catch (error) {
    caught = error
  }
  expect(caught).toBeInstanceOf(CellUnavailable)
  const err = caught as CellUnavailable
  expect(err.status).toBe(503)
  expect(err.code).toBe("cell.unopenable")
  expect(err.retryable).toBe(false)
  expect(err.incidentId).toBe("inc-unopenable-42")
  expect(err.explanation).toBe("Storage migration is paused for this environment.")
  expect(err.decidedAt).toBe("2027-01-15T08:00:00Z")
  expect(isRetryableModeError(err)).toBe(false)
  expect(isWorkspaceUnavailableError(err)).toBe(true)
})

test("transient 503, HTML 502 gateway errors, and bare network errors on /v1/definitions remain retryable and never fall back to recorded", async () => {
  expect(isRetryableModeError(new CellUnavailable(503, "temporarily busy", { retryable: true }))).toBe(true)
  expect(isRetryableModeError(new CellUnavailable(503))).toBe(true)
  expect(isRetryableModeError(new CellUnavailable(0, "network down", { timedOut: true }))).toBe(true)
  expect(isRetryableModeError(new CellUnavailable(503, "broken", { retryable: false }))).toBe(false)
  expect(isRetryableModeError(new CellRefusal(403, "authz.denied", "denied", { retryable: false }))).toBe(false)

  // Bare TypeError on /v1/definitions normalizes into a retryable CellUnavailable(0).
  let networkErr: unknown
  try {
    await readMode(async (input: string) => {
      if (input === "/api/me") return Response.json(ME)
      throw new TypeError("Failed to fetch")
    })
  } catch (error) {
    networkErr = error
  }
  expect(networkErr).toBeInstanceOf(CellUnavailable)
  expect((networkErr as CellUnavailable).status).toBe(0)
  expect(isRetryableModeError(networkErr)).toBe(true)

  // HTML 502 Bad Gateway from proxy throws retryable CellUnavailable(502) instead of returning { kind: "recorded" }.
  let gatewayErr: unknown
  try {
    await readMode(async (input: string) => {
      if (input === "/api/me") return Response.json(ME)
      return new Response("<html><body>502 Bad Gateway</body></html>", { status: 502, headers: { "content-type": "text/html" } })
    })
  } catch (error) {
    gatewayErr = error
  }
  expect(gatewayErr).toBeInstanceOf(CellUnavailable)
  expect((gatewayErr as CellUnavailable).status).toBe(502)
  expect(isRetryableModeError(gatewayErr)).toBe(true)
})

test("plainMetricLabel formats derived metric labels and plainWords matches checker semantics accurately", () => {
  const raw = "sum(completed_count) / sum(application_count); sum(completion_rate); conservation []"
  expect(plainMetricLabel(raw)).toBe("Completion rate (completed count ÷ application count)")
  expect(plainMetricLabel("sum(quantity); conservation [law.a@1]")).toBe("Total quantity")
  expect(plainMetricLabel("sum(output_fine_g) / sum(input_fine_g); law melt@1")).toBe("Output fine g ÷ input fine g")
  expect(plainMetricLabel("quantity, as of observed_at; last value per canon.Holding at each period's end"))
    .toBe("Quantity at period end (as of observed at, per Holding)")

  // Accurate copy: invalid_currency covers counts with currency / unit+currency / invalid column (not just missing source);
  // semi_additive_time covers first/last/min/max (not only period end).
  const invalidCurrencyEn = plainWords("metric.validation.invalid_currency", "en") ?? ""
  expect(invalidCurrencyEn).not.toContain("record type doesn't say which currency")
  expect(invalidCurrencyEn).toContain("currency setup")
  const semiAdditiveEn = plainWords("question.validation.semi_additive_time", "en") ?? ""
  expect(semiAdditiveEn).not.toContain("read at the end of each period")
  expect(semiAdditiveEn).toContain("first, last, min, or max")
})

test("explainError hides raw internal worker strings in Business mode while preserving server explanation and Technical details", () => {
  const err = new CellUnavailable(500, "Worker failed: handler is waiting on nothing", {
    code: "host.internal",
    explanation: "The query planner could not open the table.",
    incidentId: "inc-77",
    retryable: true,
  })
  const business = explainError(err, "en", "business")
  expect(business.headline).toBe("The workspace hit an internal problem and couldn't finish this request.")
  expect(business.detail).toBe("The query planner could not open the table.")
  expect(business.incidentId).toBe("inc-77")
  expect(business.raw).toContain("Worker failed: handler is waiting on nothing")

  const technical = explainError(err, "en", "technical")
  expect(technical.headline).toBe("Worker failed: handler is waiting on nothing")
  expect(technical.detail).toBe("The query planner could not open the table.")
  expect(technical.code).toBe("host.internal")

  const uncodedNoise = explainError(new Error("Worker failed: handler is waiting on nothing"), "en", "business")
  expect(uncodedNoise.headline).not.toContain("waiting on nothing")
  expect(uncodedNoise.raw).toContain("Worker failed: handler is waiting on nothing")
})

test("fetchWithTimeout aborts stalled headers AND headers-first stalled body reads, and honors caller abort mid-body", async () => {
  // 1. Stalled before headers arrive.
  const stalledHeaders = (_input: string, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
    init?.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")))
  })
  let caught: unknown
  try {
    await fetchWithTimeout(stalledHeaders, "/api/host/v1/definitions", {}, 25)
  } catch (error) {
    caught = error
  }
  expect(caught).toBeInstanceOf(CellUnavailable)
  expect((caught as CellUnavailable).timedOut).toBe(true)
  expect((caught as CellUnavailable).code).toBe("cell.timeout")
  expect(isWorkspaceUnavailableError(caught)).toBe(true)

  // 2. Headers arrive immediately, then body stalls past deadline: must time out and cancel upstream reader.
  let upstreamCancelled = false
  let completed = false
  const stalledBody = async () => new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode('{"partial":'))
      },
      cancel() {
        upstreamCancelled = true
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  )
  const response = await fetchWithTimeout(stalledBody, "/api/host/v1/questions:metrics", {}, 30, () => { completed = true })
  let bodyErr: unknown
  try {
    await response.text()
  } catch (error) {
    bodyErr = error
  }
  expect(bodyErr).toBeInstanceOf(CellUnavailable)
  expect((bodyErr as CellUnavailable).timedOut).toBe(true)
  expect((bodyErr as CellUnavailable).code).toBe("cell.timeout")
  expect(upstreamCancelled).toBe(true)
  expect(completed).toBe(false)

  // 3. Headers arrive immediately, body stalls, caller aborts (e.g. Retry): rejects with caller abort and cancels upstream.
  let callerCancelled = false
  const caller = new AbortController()
  const stalledForCaller = async () => new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode("{"))
      },
      cancel() {
        callerCancelled = true
      },
    }),
    { status: 200, headers: { "content-type": "application/json" } },
  )
  const response2 = await fetchWithTimeout(stalledForCaller, "/api/host/v1/questions:metrics", { signal: caller.signal }, 5_000)
  setTimeout(() => caller.abort(new DOMException("Caller retry", "AbortError")), 20)
  let abortErr: unknown
  try {
    await response2.text()
  } catch (error) {
    abortErr = error
  }
  expect(abortErr).toBeInstanceOf(DOMException)
  expect((abortErr as DOMException).name).toBe("AbortError")
  expect(callerCancelled).toBe(true)
})

test("scopedHostFetch applies the default deadline to GET, HEAD, and OPTIONS and leaves writes unwrapped", async () => {
  expect(isReadMethod(undefined)).toBe(true)
  expect(isReadMethod("GET")).toBe(true)
  expect(isReadMethod("head")).toBe(true)
  expect(isReadMethod("OPTIONS")).toBe(true)
  expect(isReadMethod("POST")).toBe(false)
  expect(isReadMethod("PUT")).toBe(false)
  expect(isReadMethod("PATCH")).toBe(false)
  expect(isReadMethod("DELETE")).toBe(false)

  const seenSignals: { method: string | undefined; signal: AbortSignal | null | undefined }[] = []
  const fetcher = async (_input: string, init?: RequestInit) => {
    seenSignals.push({ method: init?.method, signal: init?.signal })
    return Response.json({ ok: true })
  }
  const scoped = scopedHostFetch(fetcher, "/api/host", async () => ({ orgId: "org_1", environmentId: "env_1" }))

  // Read gets a timeout-bound AbortSignal injected.
  await (await scoped("/v1/definitions")).text()
  expect(seenSignals[0]?.signal).toBeDefined()

  // Mutating POST with no signal gets NO injected 12 s AbortSignal.
  await (await scoped("/v1/actions/demo.act", { method: "POST", body: "{}" })).text()
  expect(seenSignals[1]?.signal).toBeUndefined()

  // Mutating POST with its own declared signal passes that exact signal through untouched.
  const own = new AbortController()
  await (await scoped("/v1/decisions/dec_1:decide", { method: "POST", body: "{}", signal: own.signal })).text()
  expect(seenSignals[2]?.signal).toBe(own.signal)
})

test("formatLastReachable reports null when unseen and keys reachability by workspace scope", () => {
  resetCellReachable()
  const scopeA = { orgId: "org_a", environmentId: "prod" }
  const scopeB = { orgId: "org_b", environmentId: "prod" }
  expect(lastReachableAt(null)).toBeNull()
  expect(lastReachableAt(scopeA)).toBeNull()
  expect(lastReachableAt(scopeB)).toBeNull()
  expect(formatLastReachable(null)).toBeNull()
  const now = Date.UTC(2026, 10, 20, 12, 0, 0)
  recordCellReachable(scopeA, now - 10_000)
  expect(lastReachableAt(scopeA)).toBe(now - 10_000)
  expect(lastReachableAt(scopeB)).toBeNull()
  expect(lastReachableAt(undefined)).toBeNull()
  expect(formatLastReachable(now - 10_000, now)).toBe("just now")
  expect(formatLastReachable(now - 5 * 60_000, now)).toBe("5 min ago")
})

test("ChartStyle strips unsafe CSS/HTML injection payloads from id, keys, and colors", () => {
  const html = renderToStaticMarkup(
    ChartStyle({
      id: 'evil]</style><script>alert(1)</script>',
      config: {
        safe_series: { color: "var(--chart-1)" },
        bad_color: { color: "red; } </style><script>alert(1)</script>" },
        "bad key}": { color: "#123456" },
      },
    }),
  )
  expect(html).not.toContain("<script>")
  expect(html).not.toContain("alert(1)")
  expect(html).toContain('--color-safe_series: var(--chart-1);')
  expect(html).not.toContain("bad_color")
  expect(html).not.toContain("bad key")
})

test("connector, OAuth, and evidence URL guards reject unsafe schemes and external image tracking URLs", () => {
  expect(safeConnectorUrl("javascript:alert(1)", "https://studio.norn.cloud")).toBeNull()
  expect(safeConnectorUrl("http://evil.example/cb", "https://studio.norn.cloud")).toBeNull()
  expect(safeConnectorUrl("https://gateway.norn.cloud/auth", "https://studio.norn.cloud")).toBe("https://gateway.norn.cloud/auth")
  expect(safeConnectorUrl("http://127.0.0.1:8787/cb", "https://studio.norn.cloud")).toBe("http://127.0.0.1:8787/cb")

  expect(nextUrl({ url: "custom://localhost/steal" }, "https://studio.norn.cloud")).toBeNull()
  expect(nextUrl({ url: "javascript:alert(1)" }, "https://studio.norn.cloud")).toBeNull()
  expect(nextUrl({ url: "http://localhost:8787/cb" }, "https://studio.norn.cloud")).toBe("http://localhost:8787/cb")
  expect(nextUrl({ url: "https://assistant.example/cb" }, "https://studio.norn.cloud")).toBe("https://assistant.example/cb")

  expect(safeDirectEvidenceUri("https://tracker.example/pixel.png", "https://studio.norn.cloud")).toBeNull()
  expect(safeDirectEvidenceUri("https://studio.norn.cloud/scans/p1.png", "https://studio.norn.cloud")).toBe("https://studio.norn.cloud/scans/p1.png")
  expect(safeDirectEvidenceUri("http://127.0.0.1:8787/scans/p1.png", "https://studio.norn.cloud")).toBe("http://127.0.0.1:8787/scans/p1.png")
  expect(safeScanImageUrl("javascript:alert(1)", "https://studio.norn.cloud")).toBeNull()
  expect(safeScanImageUrl("https://r2.example/signed?sig=1", "https://studio.norn.cloud")).toBe("https://r2.example/signed?sig=1")
})
