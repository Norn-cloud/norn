import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { catalogOf } from "../src/lib/catalog.ts"
import { readDocument } from "../src/lib/exact.ts"
import type { RecordsRead } from "../src/lib/records-source.ts"
import type { Artifact, ObjectPage } from "../src/lib/transport.ts"

mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { RecordsGrid } = await import("../src/grid/records-grid.tsx")
const { cellText } = await import("../src/grid/cells.tsx")
const { EMPTY_QUERY, recordsQueryParams, SERVER_QUERY_UNAVAILABLE } = await import("../src/lib/records-query.ts")

const explorer = (name: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8")
const catalog = catalogOf(readDocument<readonly Artifact[]>(explorer("three-systems.definitions.json"), "recorded").document)
const item = catalog.byName.get("canon.Item")
if (item === undefined) throw new Error("canon.Item is not in the recorded catalog")
const concept = item
/** Text as React writes it into markup. */
const inHtml = (text: string) => text.replaceAll("'", "&#x27;")

/** The grid with its first page already answered, rendered to markup. */
async function grid(first: RecordsRead) {
  const client = new QueryClient()
  client.setQueryData(["records", concept.name, recordsQueryParams(EMPTY_QUERY).toString()], first)
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/", component: () => <RecordsGrid concept={concept} read={async () => first} /> })]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  })
  await router.load()
  return renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
}

const recorded = (): RecordsRead => {
  const read = readDocument<ObjectPage>(explorer("objects/canon.Item.page.json"), "recorded")
  return { kind: "page", read, origin: "recorded", served: null, next: "demo/item/gold", total: null }
}

const live = (records: unknown[], total = { count: 2, approximate: false }): RecordsRead => {
  const text = JSON.stringify({ objectType: "canon.Item", records, quarantineCount: 0, scannedThrough: null, query: { sort: [], filter: null }, next: null, total })
  return { kind: "page", read: readDocument<ObjectPage>(text, "live"), origin: "live", served: { sort: [], filter: null }, next: undefined, total }
}

test("a recorded page shows its records, says it is recorded, and keeps server sort and filter off with the reason", async () => {
  const html = await grid(recorded())
  expect(html).toContain("Gold scrap")
  expect(html).toContain(">Recorded<")
  expect(html).toContain(inHtml(SERVER_QUERY_UNAVAILABLE))
  const filterButton = html.match(/<button[^>]*>(?:(?!<\/button>).)*?Filter(?:(?!<\/button>).)*<\/button>/)?.[0] ?? ""
  expect(filterButton).toContain('disabled=""')
  expect(filterButton).toContain("sort or filter records on the server yet")
  expect(html).toContain('role="grid"')
  expect(html).not.toContain('aria-sort="ascending"')
})

test("a live page that echoes its query turns sorting on and shows the server's total", async () => {
  const html = await grid(live([
    { objectType: "canon.Item", id: "a", scope: "s", version: 1, values: { code: "A", label: "Alpha" }, withheld: false },
    { objectType: "canon.Item", id: "b", scope: "s", version: 3, values: null, withheld: true },
  ]))
  expect(html).not.toContain(inHtml(SERVER_QUERY_UNAVAILABLE))
  expect(html).toContain("quantity: sort (Shift adds a key)")
  expect(html).toContain("2 records")
  expect(html).toContain("Alpha")
  expect(html).toMatch(/data-withheld="true"[^]*Withheld: you can&#x27;t see this record&#x27;s values/)
  // One row-spanning message, not one per cell.
  expect(html.match(/data-withheld="true"[^]*?<\/div><\/div>/)?.[0].match(/role="gridcell"/g)).toHaveLength(2)
})

test("an approximate total reads as about", async () => {
  expect(await grid(live([], { count: 1200, approximate: true }))).toContain("About 1,200 records")
})

test("a workspace with no data cell says so and shows no grid", async () => {
  const html = await grid({ kind: "no-cell" })
  expect(html).toContain("This workspace has no data cell yet.")
  expect(html).not.toContain('role="grid"')
})

test("an empty live page says nothing is stored yet; a refused query shows the server's words", async () => {
  expect(await grid(live([], { count: 0, approximate: false }))).toContain("No item records are stored yet.")
  const html = await grid({ kind: "refused", status: 400, code: "query.sort_invalid", message: "The cell refused this sort.", diagnostics: [{ code: "query.sort_invalid", message: "Records can't be sorted by notes.", pointer: "/sort/0" }] })
  expect(html).toContain("The cell refused this sort.")
  expect(html).toContain("Records can&#x27;t be sorted by notes.")
  expect(html).toContain("Reset sort and filter")
})

test("typed cells keep the server's exact text", () => {
  const read = readDocument<ObjectPage>('{"records":[{"objectType":"t","id":"x","scope":"s","version":1,"values":{"qty":100.500,"ok":true,"on":"2026-11-06","n":9007199254740993}}]}', "live")
  const row = { record: read.document.records[0]!, at: "/records/0", numbers: read.numbers }
  const field = (id: string, scalar: Parameters<typeof cellText>[0]["scalar"], decimal?: Parameters<typeof cellText>[0]["decimal"]) => ({ id, type: "", optional: false, decimal, scalar })
  expect(cellText(field("qty", { kind: "decimal" }, { unit: "g", perUnit: undefined, currencyFrom: undefined }), row)).toBe("100.500 g")
  expect(cellText(field("n", { kind: "integer" }), row)).toBe("9007199254740993")
  expect(cellText(field("ok", { kind: "boolean" }), row)).toBe("Yes")
  expect(cellText(field("on", { kind: "date" }), row)).toBe("2026-11-06")
  expect(cellText(field("missing", { kind: "text" }), row)).toBe("—")
  expect(cellText(field("qty", { kind: "decimal" }), { ...row, record: { ...row.record, values: null as never, withheld: true } as never })).toBe("Withheld")
  // A property the server lists is withheld; a missing one is a dash, never the other way round.
  const partly = { ...row, record: { ...row.record, withheld: false, withheldProperties: ["qty"] } as never }
  expect(cellText(field("qty", { kind: "decimal" }), partly)).toBe("Withheld")
  expect(cellText(field("missing", { kind: "text" }), partly)).toBe("—")
})
