import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { catalogOf } from "../src/lib/catalog.ts"
import type { RecordsRead } from "../src/lib/records-source.ts"
import type { Artifact } from "../src/lib/transport.ts"
import { BOOK_EQUIPMENT, INTERVAL_ARTIFACTS } from "../e2e/interval-fixtures.ts"

const catalog = catalogOf(INTERVAL_ARTIFACTS as unknown as readonly Artifact[])
// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { RecordPicker, choiceKey, fetchPickerPage, pickedTitle, pickerChoices, pickerMode, pickerPage, pickerPageError, pickerQuery, pickerSnapshotMoved, titleProperty } = await import("../src/actions/record-picker.tsx")
const { ActionForm } = await import("../src/actions/action-form.tsx")

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
  expect(pickerPage({ kind: "restart" }, "label")).toEqual({ records: [], next: undefined, snapshotMoved: true })
  expect(pickerSnapshotMoved([{ records: [{ id: "equipment/2", title: "Forklift" }], next: "c2" }, { records: [], next: undefined, snapshotMoved: true }])).toBe(true)
  expect(pickerSnapshotMoved([{ records: [{ id: "equipment/2", title: "Forklift" }], next: undefined }])).toBe(false)
  expect(pickerPage({ kind: "no-cell" }, "label")).toBeNull()
  expect(pickerMode({ data: undefined, isError: true })).toBe("type")
  expect(pickerMode({ data: null, isError: false })).toBe("type")
  expect(pickerMode({ data: undefined, isError: false })).toBe("search")
})

test("readable records make a combobox with a clear button only on optional fields; unreadable ones a typed id", () => {
  const seeded = (page: unknown, aria: Record<string, string | boolean | undefined> = { "aria-label": "Supplier" }) => {
    const client = new QueryClient()
    client.setQueryData(["picker", "purchasing.Supplier", null, ""], { pages: [page], pageParams: [undefined] })
    return renderToStaticMarkup(<QueryClientProvider client={client}><RecordPicker objectKind="purchasing.Supplier" value="SUP-0001" onPick={() => {}} aria={aria} /></QueryClientProvider>)
  }
  const optionalHtml = seeded({ records: [{ id: "SUP-0001", title: "Acme" }], next: undefined })
  expect(optionalHtml).toContain('role="combobox"')
  expect(optionalHtml).toContain("Acme")
  expect(optionalHtml).toContain("SUP-0001")
  expect(optionalHtml).toContain('aria-label="Clear selection"')
  const requiredHtml = seeded({ records: [{ id: "SUP-0001", title: "Acme" }], next: undefined }, { "aria-label": "Supplier", "aria-required": true })
  expect(requiredHtml).not.toContain('aria-label="Clear selection"')
  const typed = seeded(null)
  expect(typed).not.toContain('role="combobox"')
  expect(typed).toContain('value="SUP-0001"')
})

test("a snapshot_moved page on Show more stays a combobox (does not collapse to a typed input)", () => {
  const client = new QueryClient()
  client.setQueryData(["picker", "purchasing.Supplier", null, ""], {
    pages: [{ records: [{ id: "SUP-0001", title: "Acme" }], next: "c2" }, { records: [], next: undefined, snapshotMoved: true }],
    pageParams: [undefined, "c2"],
  })
  const html = renderToStaticMarkup(<QueryClientProvider client={client}><RecordPicker objectKind="purchasing.Supplier" value="" onPick={() => {}} aria={{ "aria-label": "Supplier" }} /></QueryClientProvider>)
  expect(html).toContain('role="combobox"')
})

test("a refusal on page 2 preserves page 1, keeps the cursor retryable, and retries the same cursor", async () => {
  const refused: RecordsRead = { kind: "refused", status: 403, code: "authz.denied", message: "Access changed.", diagnostics: [] }
  expect(pickerPageError(refused)).toBe("Access changed.")
  // Page 1 (`after: undefined`) returns null so RecordPicker falls back to typing an id.
  expect(await fetchPickerPage(async () => refused, "purchasing.Supplier", "", "label", undefined)).toBeNull()
  // Page 2 (`after: "c2"`) throws the cell's refusal message so TanStack Query preserves page 1 and keeps the cursor retryable.
  await expect(fetchPickerPage(async () => refused, "purchasing.Supplier", "", "label", "c2")).rejects.toThrow("Access changed.")

  // Drive a real TanStack InfiniteQueryObserver through page 1 -> page 2 refusal -> Retry on cursor `c2`.
  const { InfiniteQueryObserver } = await import("@tanstack/react-query")
  const cursors: (string | undefined)[] = []
  let failSecondPage = true
  const read = async (_kind: string, query: { after?: string }): Promise<RecordsRead> => {
    cursors.push(query.after)
    if (query.after === undefined) {
      return {
        kind: "page", origin: "live", served: null, next: "c2", total: null,
        read: { document: { records: [{ id: "SUP-0001", values: { label: "Acme" } }] }, numbers: new Map() },
      } as unknown as RecordsRead
    }
    if (failSecondPage) { failSecondPage = false; return refused }
    return {
      kind: "page", origin: "live", served: null, next: undefined, total: null,
      read: { document: { records: [{ id: "SUP-0002", values: { label: "Bravo" } }] }, numbers: new Map() },
    } as unknown as RecordsRead
  }
  const client = new QueryClient()
  const observer = new InfiniteQueryObserver(client, {
    queryKey: ["picker", "purchasing.Supplier", "label", ""],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => fetchPickerPage(read, "purchasing.Supplier", "", "label", pageParam),
    getNextPageParam: (last) => last?.snapshotMoved ? undefined : last?.next,
    retry: false,
  })
  await observer.refetch()
  const first = observer.getCurrentResult()
  expect(first.data?.pages).toEqual([{ records: [{ id: "SUP-0001", title: "Acme" }], next: "c2" }])
  expect(first.hasNextPage).toBe(true)

  const failed = await observer.fetchNextPage()
  expect(failed.isFetchNextPageError).toBe(true)
  expect((failed.error as Error | null)?.message).toBe("Access changed.")
  expect(failed.data?.pages).toEqual([{ records: [{ id: "SUP-0001", title: "Acme" }], next: "c2" }])
  expect(failed.hasNextPage).toBe(true)

  const retried = await observer.fetchNextPage()
  expect(retried.isFetchNextPageError).toBe(false)
  expect(retried.data?.pages).toEqual([
    { records: [{ id: "SUP-0001", title: "Acme" }], next: "c2" },
    { records: [{ id: "SUP-0002", title: "Bravo" }], next: undefined },
  ])
  expect(cursors).toEqual([undefined, "c2", "c2"])
})

test("ActionForm DateTimePicker preserves an explicit fall-back offset (-05:00) and matches UTC Z instants (including fractional seconds) by epoch", () => {
  const client = new QueryClient()
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <ActionForm action={BOOK_EQUIPMENT as never} objectTitle="Equipment booking"
        remembered={{ id: "b/1", label: "Fall back", equipment: "equipment/1", starts_at: "2026-11-01T01:30:00-05:00", ends_at: "2026-11-01T03:00:00-05:00", inspect_on: "", daily_check: "" }} />
    </QueryClientProvider>,
  )
  expect(html).toContain("sent as ")
  expect(html).toContain("2026-11-01T01:30:00-05:00")

  const utcHtml = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <ActionForm action={BOOK_EQUIPMENT as never} objectTitle="Equipment booking"
        remembered={{ id: "b/2", label: "Fall back UTC", equipment: "equipment/1", starts_at: "2026-11-01T06:30:00.250Z", ends_at: "2026-11-01T08:00:00Z", inspect_on: "", daily_check: "" }} />
    </QueryClientProvider>,
  )
  expect(utcHtml).toContain("2026-11-01T06:30:00.250Z")
  if (Intl.DateTimeFormat().resolvedOptions().timeZone === "America/New_York") {
    expect(utcHtml).toContain('value="2026-11-01T01:30:00.250-05:00" selected=""')
    expect(utcHtml).toContain("-05:00 (second occurrence)")
    expect(utcHtml).not.toContain(".250-05:00 (second occurrence)")
  }
})

// #1705: options are keyed by kind and id, so a record whose id reads like an action stays that record.
test("a record named like an action is still a record: options are keyed by kind, never by bare value", () => {
  const records = [{ id: "__clear", title: "Clear" }, { id: "__more", title: "More" }, { id: "equipment/2", title: "Forklift" }]
  const choices = pickerChoices({ records, moved: false, optional: true, value: "equipment/2", more: true, typed: "" })
  expect(choices.map((choice) => choice.kind)).toEqual(["clear", "record", "record", "record", "more"])
  const keys = choices.map((choice) => choice.key)
  expect(new Set(keys).size).toBe(keys.length)
  expect(choices.find((choice) => choice.kind === "record" && choice.id === "__clear")?.key).toBe("record:__clear")
  expect(choiceKey({ kind: "clear" })).not.toBe(choiceKey({ kind: "record", id: "__clear" }))
  expect(choiceKey({ kind: "typed", id: "__clear" })).not.toBe(choiceKey({ kind: "record", id: "__clear" }))
})

test("the options follow the list's state: Reload alone when the snapshot moved; Use the id only for an unlisted id", () => {
  const records = [{ id: "equipment/2", title: "Forklift" }]
  expect(pickerChoices({ records, moved: true, optional: true, value: "equipment/2", more: true, typed: "" }).map((choice) => choice.kind)).toEqual(["reload"])
  expect(pickerChoices({ records, moved: false, optional: false, value: "equipment/2", more: false, typed: "equipment/2" }).map((choice) => choice.kind)).toEqual(["record"])
  expect(pickerChoices({ records, moved: false, optional: true, value: "", more: false, typed: "equipment/9" }).map((choice) => choice.key)).toEqual(["record:equipment/2", "typed:equipment/9"])
})
