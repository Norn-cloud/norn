import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import type { PickerRecords } from "../src/actions/action-form.tsx"

mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { RecordPicker, pickerMode } = await import("../src/actions/action-form.tsx")

const KIND = "purchasing.Supplier"
const render = (client: QueryClient) =>
  renderToStaticMarkup(<QueryClientProvider client={client}><RecordPicker objectKind={KIND} value="" onPick={() => {}} aria={{ "aria-label": "Supplier" }} /></QueryClientProvider>)
const seeded = (data: PickerRecords) => {
  const client = new QueryClient()
  client.setQueryData(["picker", KIND], data)
  return client
}
const records = [{ id: "SUP-0001", values: { supplier_name: "Acme" } }]

// ACTION-CATALOG-LINKS-1: a complete page is a choice of records.
test("every record of the type on one page is a choice", () => {
  const html = render(seeded({ records, complete: true }))
  expect(html).toContain("<select")
  expect(html).toContain("SUP-0001 · Acme")
})

// Past the first page the id is typed, the first page offered as suggestions: no record is unreachable.
test("more records than one page keeps the id typeable", () => {
  const html = render(seeded({ records, complete: false }))
  expect(html).not.toContain("<select")
  expect(html).toContain("<input")
  expect(html).toContain("<datalist")
  expect(html).toContain('value="SUP-0001"')
})

// A read that failed (not only one the cell refused) falls back to typing the id.
test("a failed read falls back to typing the id", () => {
  expect(pickerMode({ data: undefined, isError: true })).toBe("type")
  expect(pickerMode({ data: null, isError: false })).toBe("type")
  expect(pickerMode({ data: undefined, isError: false })).toBe("choose")
  expect(pickerMode({ data: { records, complete: false }, isError: false })).toBe("suggest")
})
