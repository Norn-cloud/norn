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
import type { RecordedReader } from "../src/lib/workspace.tsx"

mock.module("../src/lib/transport.ts", () => ({ transport: {} }))
const { RecordsGrid } = await import("../src/grid/records-grid.tsx")
const { RecordsTimeline } = await import("../src/grid/records-timeline.tsx")
const { LocaleProvider } = await import("../src/lib/locale.tsx")
const { cellText } = await import("../src/grid/cells.tsx")
const { EMPTY_QUERY, recordsQueryParams, SERVER_QUERY_UNAVAILABLE } = await import("../src/lib/records-query.ts")
const { FilterEditor, blankClause, fromPredicate, sameDraft, settled, toPredicate } = await import("../src/grid/filter-editor.tsx")

const explorer = (name: string) => readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8")
const catalog = catalogOf(readDocument<readonly Artifact[]>(explorer("three-systems.definitions.json"), "recorded").document)
const item = catalog.byName.get("canon.Item")
if (item === undefined) throw new Error("canon.Item is not in the recorded catalog")
const concept = item
/** Text as React writes it into markup. */
const inHtml = (text: string) => text.replaceAll("'", "&#x27;")

/** A stand-in reader that answers every page with `first`, under the key the test seeds. */
const answering = (first: RecordsRead): RecordedReader => ({
  kind: "recorded",
  records: (objectType, query) => ({ queryKey: ["records", objectType, recordsQueryParams(query).toString()], queryFn: async () => first }),
  object: (objectType, id) => ({ queryKey: ["object", "recorded", objectType, id], queryFn: async () => null }),
})

/** The grid with its first page already answered, rendered to markup. */
async function grid(first: RecordsRead) {
  const client = new QueryClient()
  client.setQueryData(["records", concept.name, recordsQueryParams(EMPTY_QUERY).toString()], first)
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/", component: () => <RecordsGrid concept={concept} reader={answering(first)} /> })]),
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
  expect(html).toContain("code: sort (Shift adds a key)")
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

test("'is one of' on a free-form field keeps what was typed, and applying leaves the draft settled", () => {
  const name = concept.fields.find((field) => field.scalar.kind === "text")
  if (name === undefined) throw new Error("canon.Item has no text field")
  const typed = [{ ...blankClause(name), op: "in" as const, value: "New York\n Gold \n\n" }]
  expect(toPredicate(typed, concept.fields)).toEqual({ all: [{ property: name.id, op: "in", value: ["New York", "Gold"] }] })
  // The raw text (a trailing newline, a space) is not "not applied" once its predicate is.
  const applied = fromPredicate(toPredicate(typed, concept.fields))
  expect(sameDraft(settled(typed, concept.fields), applied)).toBe(true)
  // A leftover from an earlier test (extra `values`, untrimmed text) settles the same way.
  const leftover = [{ ...blankClause(name), op: "eq" as const, value: " Gold ", values: ["x"] }]
  expect(settled(leftover, concept.fields)).toEqual([{ property: name.id, op: "eq", value: "Gold", high: "", values: [] }])
})

test("the filter editor associates an invalid list item with the list control", () => {
  const field = { id: "quantity", type: "decimal", optional: false, decimal: undefined, scalar: { kind: "decimal" } } satisfies Parameters<typeof blankClause>[0]
  const message = "a decimal value is exact decimal text"
  const noop = () => {}
  const html = renderToStaticMarkup(
    <FilterEditor fields={[field]} draft={[{ ...blankClause(field), op: "in", value: "1\n12,5" }]}
      applied={[]} onDraft={noop} onApply={noop} onClear={noop}
      diagnostics={[{ code: "query.filter_invalid", message, pointer: "/filter/all/0/value/1" }]} />,
  )
  const list = html.match(/<textarea[^>]*aria-label="Condition 1 value"[^>]*>/)?.[0] ?? ""
  expect(list).toContain('aria-invalid="true"')
  const descriptionId = list.match(/aria-describedby="([^"]+)"/)?.[1]
  expect(descriptionId).toBeDefined()
  expect(html).toContain(`<span id="${descriptionId}">${message}</span>`)
})

test("RecordsTimeline hides stale bars while pending, alerts on filter overflow, and formats headings in Studio's active locale", () => {
  const client = new QueryClient()
  const window = { start: Date.UTC(2026, 10, 23), end: Date.UTC(2026, 10, 30) }
  const row = {
    record: { objectType: "facilities.EquipmentBooking", id: "booking/1", scope: "s", version: 1, values: { label: "Prototype run", starts_at: "2026-11-23T08:00:00Z", ends_at: "2026-11-23T12:00:00Z" }, withheld: false },
    at: "/records/0",
    numbers: new Map(),
  } as never
  const noop = () => {}
  const render = (props: { loaded?: boolean; pending?: boolean; filterOverflow?: number; locale?: "en" | "ar"; rows?: readonly unknown[]; read?: Parameters<typeof RecordsTimeline>[0]["read"] }) =>
    renderToStaticMarkup(
      <LocaleProvider initial={props.locale ?? "en"}>
        <QueryClientProvider client={client}>
          <RecordsTimeline concept={concept} rows={(props.rows ?? [row]) as never} window={window} zoom="week"
            loaded={props.loaded ?? (!(props.pending ?? false) && (props.filterOverflow ?? 0) === 0)}
            pending={props.pending ?? false} fetching={props.pending ?? false} filter={null} filterOverflow={props.filterOverflow ?? 0}
            read={props.read ?? (async () => ({ kind: "no-cell" }))} onZoom={noop} onStep={noop} onToday={noop} onJump={noop} onOpen={noop} />
        </QueryClientProvider>
      </LocaleProvider>,
    )

  const stalePending = render({ pending: true })
  expect(stalePending).toContain('aria-label="Reading records"')
  expect(stalePending).not.toContain("Prototype run")

  // Refused or failed read (`loaded: false`, `rows: []`): neither renders "Nothing in this week" nor fires nearest queries.
  let nearestCalls = 0
  const failedRead = render({
    loaded: false,
    pending: false,
    rows: [],
    read: async () => { nearestCalls += 1; return { kind: "no-cell" } },
  })
  expect(failedRead).not.toContain("Nothing in this week")
  expect(nearestCalls).toBe(0)

  // Confirmed empty page (`loaded: true`, `rows: []`) renders the empty-window state.
  const confirmedEmpty = render({ loaded: true, pending: false, rows: [] })
  expect(confirmedEmpty).toContain("Nothing in this week")

  const overflow = render({ filterOverflow: 2 })
  expect(overflow).toContain("Too many filter conditions for Timeline")
  expect(overflow).toContain("Remove 2 conditions")
  expect(overflow).not.toContain("Prototype run")

  const arabic = render({ locale: "ar" })
  expect(arabic).toContain("Prototype run")
  expect(arabic).toContain("نوفمبر")
})

test("RecordsGrid offers the Lifecycle view toggle beside Table and Timeline only when the type declares a lifecycle", async () => {
  const renderGridFor = async (target: typeof concept, timeline: boolean) => {
    const client = new QueryClient()
    const first = live([])
    client.setQueryData(["records", target.name, recordsQueryParams(EMPTY_QUERY).toString()], first)
    const root = createRootRoute()
    const router = createRouter({
      routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/", component: () => <RecordsGrid concept={target} reader={answering(first)} timeline={timeline} /> })]),
      history: createMemoryHistory({ initialEntries: ["/"] }),
    })
    await router.load()
    return renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
  }

  expect(await renderGridFor(concept, false)).not.toContain(">Lifecycle<")
  const restatement = catalog.byName.get("core.analytics.Restatement")
  if (restatement === undefined) throw new Error("core.analytics.Restatement is not in the recorded catalog")
  const lifecycleOnly = await renderGridFor(restatement, false)
  expect(lifecycleOnly).toContain(">Table<")
  expect(lifecycleOnly).toContain(">Lifecycle<")
  expect(lifecycleOnly).not.toContain(">Timeline<")
  const both = await renderGridFor(restatement, true)
  expect(both).toContain(">Table<")
  expect(both).toContain(">Timeline<")
  expect(both).toContain(">Lifecycle<")
})
