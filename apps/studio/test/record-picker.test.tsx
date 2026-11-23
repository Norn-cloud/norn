import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { catalogOf } from "../src/lib/catalog.ts"
import type { RecordsRead } from "../src/lib/records-source.ts"
import type { Artifact } from "../src/lib/transport.ts"
import { INTERVAL_ARTIFACTS } from "../e2e/interval-fixtures.ts"

const catalog = catalogOf(INTERVAL_ARTIFACTS as unknown as readonly Artifact[])
// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { RecordPicker, pickedTitle, pickerMode, pickerPage, pickerQuery, titleProperty } = await import("../src/actions/record-picker.tsx")

// ACTION-CATALOG-LINKS-1: the picker titles and searches records by the type's declared title property.
test("a type's title property is its label or display name, else a text field named like a name", () => {
  expect(titleProperty(catalog.byName.get("facilities.Equipment"))).toBe("label")
  expect(titleProperty(undefined)).toBeUndefined()
  expect(pickedTitle({ id: "SUP-1", values: { supplier_name: "Acme" } }, "supplier_name")).toBe("Acme")
  expect(pickedTitle({ id: "SUP-1", values: { label: "Acme Ltd", supplier_name: "Acme" } }, "supplier_name")).toBe("Acme Ltd")
  expect(pickedTitle({ id: "SUP-1", values: null }, "label")).toBe("SUP-1")
})

// Studio never filters a page: a search is the cell's `contains` clause; an unsearched read sends no query.
test("search is the cell's contains clause on the title property; paging carries the cursor", () => {
  expect(pickerQuery("", "label", undefined)).toEqual({ sort: [], filter: null, limit: 25 })
  expect(pickerQuery(" fork ", "label", "c2")).toEqual({ sort: [], filter: { all: [{ property: "label", op: "contains", value: "fork" }] }, limit: 25, after: "c2" })
  expect(pickerQuery("fork", undefined, undefined).filter).toBeNull()
})

test("a page lists its readable records by title and drops withheld ones; anything but a page is unreadable", () => {
  const page = {
    kind: "page", origin: "live", served: null, next: "c2", total: null,
    read: { document: { records: [{ id: "equipment/2", values: { label: "Forklift" } }, { id: "equipment/9", withheld: true, values: null }] }, numbers: new Map() },
  } as unknown as RecordsRead
  expect(pickerPage(page, "label")).toEqual({ records: [{ id: "equipment/2", title: "Forklift" }], next: "c2" })
  expect(pickerPage({ kind: "no-cell" }, "label")).toBeNull()
  expect(pickerMode({ data: undefined, isError: true })).toBe("type")
  expect(pickerMode({ data: null, isError: false })).toBe("type")
  expect(pickerMode({ data: undefined, isError: false })).toBe("search")
})

test("readable records make a combobox that shows the chosen record's title and id; unreadable ones a typed id", () => {
  const seeded = (page: unknown) => {
    const client = new QueryClient()
    client.setQueryData(["picker", "purchasing.Supplier", null, ""], { pages: [page], pageParams: [undefined] })
    return renderToStaticMarkup(<QueryClientProvider client={client}><RecordPicker objectKind="purchasing.Supplier" value="SUP-0001" onPick={() => {}} aria={{ "aria-label": "Supplier" }} /></QueryClientProvider>)
  }
  const html = seeded({ records: [{ id: "SUP-0001", title: "Acme" }], next: undefined })
  expect(html).toContain('role="combobox"')
  expect(html).toContain("Acme")
  expect(html).toContain("SUP-0001")
  const typed = seeded(null)
  expect(typed).not.toContain('role="combobox"')
  expect(typed).toContain('value="SUP-0001"')
})
