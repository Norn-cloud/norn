import { afterEach, expect, test } from "bun:test"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { CellUnavailable } from "../src/lib/definitions.ts"
import { CellRefusal, NotServed } from "../src/lib/live-transport.ts"
import { AMBIGUOUS_LOAD_MESSAGE, columnId, createSingleFlight, define, isAmbiguousLoadError, kindOf, loadDataset, loadRecords, upload, uploadFailure, uploadSourceId, type Completed } from "../src/lib/uploads.ts"
import { AmbiguousLoadNotice, UploadCsvConfigurePanel } from "../src/upload-csv.tsx"

const original = globalThis.fetch
afterEach(() => { globalThis.fetch = original })

const ME = { user: { id: "u", name: "U", email: "" }, organization: { id: "org", name: "Org", role: "admin" }, environmentId: "dev" }

/** A cell answering /api/me and one route; every host call is recorded with its body and signal. */
function cell(status: number, body: unknown) {
  const calls: { url: string; body: unknown; signal: AbortSignal | null | undefined }[] = []
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(ME)
    calls.push({ url, body: init?.body === undefined ? undefined : JSON.parse(String(init.body)), signal: init?.signal })
    return Response.json(body, { status })
  }) as typeof fetch
  return calls
}

test("a column is named by the cell's snake id, and starts as the kind the cell inferred, in define's words", () => {
  expect(columnId({ id: "unit_price", name: "Unit Price", inferred: "decimal", nullable: false, samples: [] })).toBe("unit_price")
  expect(columnId({ name: " Unit Price (USD) ", inferred: "decimal", nullable: false, samples: [] })).toBe("unit_price_usd")
  expect(kindOf("string")).toBe("text")
  expect(kindOf("date")).toBe("datetime")
  expect(kindOf("decimal")).toBe("decimal")
})

test("define sends the type, key, column choices and the upload's digest, and returns the admitted type", async () => {
  const calls = cell(200, { objectType: "upload.Sale", definitionsVersion: 7, sourceSchema: "s", mapping: "m" })
  const defined = await define("up_1", "Sale", ["sku"], [{ id: "sku", type: "text", include: true, measure: false }, { id: "amount", type: "decimal", include: true, measure: true }], "ab".repeat(32))
  expect(defined.objectType).toBe("upload.Sale")
  expect(calls[0]?.url).toBe("/api/host/v1/uploads/up_1:define?orgId=org&environmentId=dev")
  expect(calls[0]?.body).toEqual({ typeName: "Sale", key: ["sku"], columns: [{ id: "sku", type: "text", include: true, measure: false }, { id: "amount", type: "decimal", include: true, measure: true }], sha256: "ab".repeat(32) })
  expect(calls[0]?.signal).toBeUndefined()
})

test("a dataset load reports the cell's counts; a cell with no engine says so and nothing else is tried", async () => {
  const loaded = { objectType: "upload.Sale", load: "l1", uri: "s3://x", sha256: "s", rows: 3000, rejected: 2, reasons: { "bulk.not_decimal": 2 }, bytes: 1, millis: 40 }
  const calls = cell(200, loaded)
  expect(await loadDataset("up_1", "upload.Sale")).toEqual(loaded)
  expect(calls[0]?.body).toEqual({ uploadId: "up_1", objectType: "upload.Sale" })
  expect(calls[0]?.signal).toBeUndefined()
  cell(501, { code: "analytics.unconfigured", message: "no analytical engine" })
  expect(await loadDataset("up_1", "upload.Sale")).toBe("no-engine")
  cell(404, { code: "route.not_found", message: "no such route" })
  expect(await loadDataset("up_1", "upload.Sale")).toBe("no-engine")
})

test("any other refusal of a dataset load is the cell's, shown as sent", async () => {
  cell(422, { code: "bulk.missing_column", message: "the file has no column amount" })
  await expect(loadDataset("up_1", "upload.Sale")).rejects.toThrow("the file has no column amount")
})

test("an upload step that gives up or cannot reach the storage says so in words", () => {
  const timeout = Object.assign(new Error("signal timed out"), { name: "TimeoutError" })
  expect(uploadFailure("cell", timeout).message).toContain("The cell did not answer within 90 seconds")
  expect(uploadFailure("storage", timeout).message).toContain("The storage did not take the file")
  expect(uploadFailure("storage", new TypeError("Failed to fetch")).message).toContain("could not reach the storage")
  const refusal = new Error("the upload was read but not kept")
  expect(uploadFailure("cell", refusal)).toBe(refusal)
})

test("a storage the browser cannot reach fails the upload with that reason, before the cell is asked to read it", async () => {
  const calls = cell(200, { uploadId: "u1", putUrl: "https://storage.example/u1", putHeaders: { "content-type": "text/csv" }, expiresAt: "2026-11-13T00:00:00Z" })
  const unreachable = async () => { throw new TypeError("Failed to fetch") }
  await expect(upload(new File(["a,b\n1,2\n"], "x.csv", { type: "text/csv" }), unreachable)).rejects.toThrow("could not reach the storage")
  expect(calls.map(({ url }) => new URL(url, "http://studio").pathname)).toEqual(["/api/host/v1/uploads:begin"])
})

test("CSV load writes apply no client timeout and classify network errors and 502/503/504 as ambiguous while keeping 4xx refusals clear", async () => {
  const recordCalls = cell(200, { connectionId: "upload.sale", objectType: "upload.Sale", definitionsVersion: 2, rowsRead: 2, rowsLanded: 2, rowsQuarantined: 0 })
  await loadRecords("up_1", "Sale", ["sku"], [{ id: "sku", type: "text", include: true, measure: false }], "ab".repeat(32))
  expect(recordCalls[0]?.signal).toBeUndefined()

  // Direct error classification: network errors and 502/503/504 are ambiguous; 4xx refusals, 500, and NotServed are not.
  expect(isAmbiguousLoadError(new TypeError("Failed to fetch"))).toBe(true)
  expect(isAmbiguousLoadError(new CellUnavailable(0, undefined, { retryable: true }))).toBe(true)
  expect(isAmbiguousLoadError(new CellUnavailable(502))).toBe(true)
  expect(isAmbiguousLoadError(new CellUnavailable(503))).toBe(true)
  expect(isAmbiguousLoadError(new CellUnavailable(504))).toBe(true)
  expect(isAmbiguousLoadError(new CellRefusal(502, "gateway.bad_gateway", "bad gateway"))).toBe(true)
  expect(isAmbiguousLoadError(new CellRefusal(504, "gateway.timeout", "gateway timeout"))).toBe(true)
  expect(isAmbiguousLoadError(new CellRefusal(422, "bulk.missing_column", "missing column"))).toBe(false)
  expect(isAmbiguousLoadError(new CellRefusal(403, "authz.denied", "denied"))).toBe(false)
  expect(isAmbiguousLoadError(new CellRefusal(409, "upload.changed", "changed"))).toBe(false)
  expect(isAmbiguousLoadError(new CellUnavailable(500, "internal error"))).toBe(false)
  expect(isAmbiguousLoadError(new NotServed("Uploads are"))).toBe(false)

  // End-to-end through loadDataset: gateway 504 (HTML) and network drop reject as ambiguous; 422 rejects as clear refusal.
  for (const status of [502, 503, 504] as const) {
    globalThis.fetch = (async (input: string | URL | Request) => {
      if (String(input) === "/api/me") return Response.json(ME)
      return new Response(`<html><body>${status} Gateway Error</body></html>`, { status, headers: { "content-type": "text/html" } })
    }) as typeof fetch
    let caught: unknown
    try { await loadDataset("up_1", "upload.Sale") } catch (error) { caught = error }
    expect(isAmbiguousLoadError(caught)).toBe(true)
  }

  globalThis.fetch = (async (input: string | URL | Request) => {
    if (String(input) === "/api/me") return Response.json(ME)
    throw new TypeError("network error")
  }) as typeof fetch
  let netErr: unknown
  try { await loadRecords("up_1", "Sale", ["sku"], [{ id: "sku", type: "text", include: true, measure: false }], "ab".repeat(32)) } catch (error) { netErr = error }
  expect(isAmbiguousLoadError(netErr)).toBe(true)

  cell(422, { code: "bulk.missing_column", message: "the file has no column amount" })
  let refusalErr: unknown
  try { await loadDataset("up_1", "upload.Sale") } catch (error) { refusalErr = error }
  expect(isAmbiguousLoadError(refusalErr)).toBe(false)
})

async function renderInRouter(renderNode: () => ReturnType<typeof createElement>): Promise<string> {
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources", component: renderNode })]),
    history: createMemoryHistory({ initialEntries: ["/sources"] }),
  })
  await router.load()
  return renderToStaticMarkup(createElement(RouterProvider, { router } as never))
}

const SAMPLE_COMPLETED: Completed = {
  uri: "s3://uploads/up_1.csv",
  sha256: "cd".repeat(32),
  size: 42,
  rows: 2,
  columns: [
    { id: "sku", name: "sku", inferred: "text", nullable: false, samples: ["A1", "A2"] },
    { id: "amount", name: "amount", inferred: "decimal", nullable: false, samples: ["10.00", "20.00"] },
  ],
}

function buttonOpeningTag(html: string, label: string): string {
  const buttons = html.match(/<button\b[^>]*>[\s\S]*?<\/button>/g) ?? []
  const found = buttons.find((tag) => tag.includes(label))
  expect(found, `expected button containing "${label}"`).toBeDefined()
  return found!.slice(0, found!.indexOf(">") + 1)
}

test("ambiguous CSV load failures render honest copy with a link to the source and no one-click retry; clear 4xx refusals keep retry", async () => {
  expect(AMBIGUOUS_LOAD_MESSAGE).toBe("The load may or may not have finished. Check this source's latest load in Sources before trying again.")
  expect(uploadSourceId("Sale")).toBe("upload.sale")
  expect(uploadSourceId("upload.Sale")).toBe("upload.sale")

  const ambiguousHtml = await renderInRouter(() => createElement(AmbiguousLoadNotice, {
    objectType: "upload.Sale",
    connectionId: "upload.sale",
    error: new CellUnavailable(504),
  }))
  expect(ambiguousHtml).toContain('data-load-outcome="ambiguous"')
  expect(ambiguousHtml).toContain("The load may or may not have finished. Check this source&#x27;s latest load in Sources before trying again.")
  expect(ambiguousHtml).toContain('href="/sources#upload.sale"')
  expect(ambiguousHtml).not.toContain("Retry")
  expect(ambiguousHtml).not.toContain("Check again")
  expect(ambiguousHtml).not.toContain("Load 2 rows as a dataset")
  expect(ambiguousHtml).not.toContain("As governed records")

  const refusalHtml = renderToStaticMarkup(createElement(UploadCsvConfigurePanel, {
    completed: SAMPLE_COMPLETED,
    typeName: "Sale",
    choices: { sku: { include: true, key: true, type: "text", measure: false }, amount: { include: true, key: false, type: "decimal", measure: true } },
    pending: null,
    problem: new CellRefusal(422, "bulk.missing_column", "the file has no column amount"),
    onTypeNameChange: () => {},
    onChoose: () => {},
    onReset: () => {},
    onSubmitRecords: () => {},
    onSubmitDataset: () => {},
  }))
  expect(refusalHtml).toContain("the file has no column amount")
  expect(buttonOpeningTag(refusalHtml, "Load 2 rows as a dataset")).not.toContain("disabled=")
  expect(buttonOpeningTag(refusalHtml, "As governed records")).not.toContain("disabled=")
})

test("CSV load submission is blocked while a request is in flight (single-flight guard and disabled controls)", async () => {
  const flight = createSingleFlight()
  let runs = 0
  let finish!: () => void
  const first = flight.run(() => new Promise<string>((resolve) => {
    runs += 1
    finish = () => resolve("done")
  }))
  expect(first).not.toBeNull()
  expect(flight.inFlight).toBe(true)
  // A duplicate submission while the first request is in flight is blocked and does not invoke work().
  expect(flight.run(async () => { runs += 1; return "duplicate" })).toBeNull()
  expect(runs).toBe(1)

  finish()
  expect(await first).toBe("done")
  expect(flight.inFlight).toBe(false)

  // Once settled, a subsequent submission is permitted again (including after a rejection).
  const failed = flight.run(async () => { runs += 1; throw new Error("refused") })
  await expect(failed!).rejects.toThrow("refused")
  expect(flight.inFlight).toBe(false)
  expect(runs).toBe(2)

  // While in flight, the form disables submit and reset controls so the user cannot double-submit.
  const inFlightDatasetHtml = renderToStaticMarkup(createElement(UploadCsvConfigurePanel, {
    completed: SAMPLE_COMPLETED,
    typeName: "Sale",
    choices: { sku: { include: true, key: true, type: "text", measure: false }, amount: { include: true, key: false, type: "decimal", measure: true } },
    pending: "dataset",
    problem: null,
    onTypeNameChange: () => {},
    onChoose: () => {},
    onReset: () => {},
    onSubmitRecords: () => {},
    onSubmitDataset: () => {},
  }))
  expect(buttonOpeningTag(inFlightDatasetHtml, "Start over")).toContain('disabled=""')
  expect(buttonOpeningTag(inFlightDatasetHtml, "As governed records")).toContain('disabled=""')
  expect(buttonOpeningTag(inFlightDatasetHtml, "Loading…")).toContain('disabled=""')

  const inFlightRecordsHtml = renderToStaticMarkup(createElement(UploadCsvConfigurePanel, {
    completed: SAMPLE_COMPLETED,
    typeName: "Sale",
    choices: { sku: { include: true, key: true, type: "text", measure: false }, amount: { include: true, key: false, type: "decimal", measure: true } },
    pending: "records",
    problem: null,
    onTypeNameChange: () => {},
    onChoose: () => {},
    onReset: () => {},
    onSubmitRecords: () => {},
    onSubmitDataset: () => {},
  }))
  expect(buttonOpeningTag(inFlightRecordsHtml, "Start over")).toContain('disabled=""')
  expect(buttonOpeningTag(inFlightRecordsHtml, "Loading…")).toContain('disabled=""')
  expect(buttonOpeningTag(inFlightRecordsHtml, "Load 2 rows as a dataset")).toContain('disabled=""')
})
