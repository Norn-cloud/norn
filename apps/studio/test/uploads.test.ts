import { afterEach, expect, test } from "bun:test"
import { columnId, define, kindOf, loadDataset } from "../src/lib/uploads.ts"

const original = globalThis.fetch
afterEach(() => { globalThis.fetch = original })

const ME = { user: { id: "u", name: "U", email: "" }, organization: { id: "org", name: "Org", role: "admin" }, environmentId: "dev" }

/** A cell answering /api/me and one route; every host call is recorded with its body. */
function cell(status: number, body: unknown) {
  const calls: { url: string; body: unknown }[] = []
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = String(input)
    if (url === "/api/me") return Response.json(ME)
    calls.push({ url, body: init?.body === undefined ? undefined : JSON.parse(String(init.body)) })
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
})

test("a dataset load reports the cell's counts; a cell with no engine says so and nothing else is tried", async () => {
  const loaded = { objectType: "upload.Sale", load: "l1", uri: "s3://x", sha256: "s", rows: 3000, rejected: 2, reasons: { "bulk.not_decimal": 2 }, bytes: 1, millis: 40 }
  const calls = cell(200, loaded)
  expect(await loadDataset("up_1", "upload.Sale")).toEqual(loaded)
  expect(calls[0]?.body).toEqual({ uploadId: "up_1", objectType: "upload.Sale" })
  cell(501, { code: "analytics.unconfigured", message: "no analytical engine" })
  expect(await loadDataset("up_1", "upload.Sale")).toBe("no-engine")
  cell(404, { code: "route.not_found", message: "no such route" })
  expect(await loadDataset("up_1", "upload.Sale")).toBe("no-engine")
})

test("any other refusal of a dataset load is the cell's, shown as sent", async () => {
  cell(422, { code: "bulk.missing_column", message: "the file has no column amount" })
  await expect(loadDataset("up_1", "upload.Sale")).rejects.toThrow("the file has no column amount")
})
