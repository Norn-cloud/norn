import { afterAll, expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { readDocument } from "../src/lib/exact.ts"
import type { SourceLineage } from "../src/lib/transport.ts"

// These pages read the view from localStorage, which the tests replace; put the original back so later files in the same bun process see their own.
const originalStorage = globalThis.localStorage
afterAll(() => { globalThis.localStorage = originalStorage })

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks; the reads are answered below instead.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { ObjectPage, SourcesItemPage, summaryOf } = await import("../src/objects.tsx")
const { ObjectTypePage } = await import("../src/sources.tsx")
const { ViewProvider } = await import("../src/lib/view.tsx")
const { ProvenanceChain } = await import("../src/components/provenance.tsx")

const fixture = (name: string) => readDocument(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8"), "recorded")

/** The record page at its route, with the recorded reads already answered. */
async function page(concept: string, id: string, view = "business", links?: unknown, access?: unknown) {
  globalThis.localStorage = { getItem: () => view, setItem: () => {} } as unknown as Storage
  const client = new QueryClient()
  const file = (read: string) => fixture(`objects/${concept}/${id.replaceAll("/", "~")}.${read}.json`)
  client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
  if (access !== undefined) client.setQueryData(["live-access"], access)
  client.setQueryData(["object", concept, id], { object: file("object"), links: links === undefined ? file("links") : readDocument(JSON.stringify(links), "recorded"), source: file("source") })
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: ObjectPage })]),
    history: createMemoryHistory({ initialEntries: [`/sources/${concept}/${id}`] }),
  })
  await router.load()
  return renderToStaticMarkup(<QueryClientProvider client={client}><ViewProvider><RouterProvider router={router} /></ViewProvider></QueryClientProvider>)
}

test("a record page names its type and copyable id, and offers every tab", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toContain("Gold scrap")
  expect(html).toContain("Copy demo/item/gold")
  for (const tab of ["Overview", "Lineage", "Guarantees", "Access"]) expect(html).toContain(`>${tab}</button>`)
  expect(html).toContain("No personal data marked")
})

test("a value the server recorded a source for opens its provenance", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toMatch(/aria-haspopup="dialog"[^>]*>GOLD</)
})

test("the technical view shows only Norn artefacts: the definition, the mapping pin and ids, and what the read lacks", async () => {
  const html = await page("canon.Item", "demo/item/gold", "technical")
  expect(html).toContain("Record document")
  expect(html).toContain("Copy erpnext.three.mapping@0.1.0")
  expect(html).toContain("Copy fcf977dc241bf9021511c536c502b22f9635821cc500b5db5e6d6cdfe4ca5fea")
  expect(html).toContain("This page shows a recorded snapshot. Once live reads are available, they will return the access decision and receipt for your account.")
  expect(html).toContain("Viewing as")
  expect(html).not.toContain("no signed-in principal")
  expect(html).toContain("&quot;objectType&quot;: &quot;canon.Item&quot;")
})

test("a record id with slashes is the rest of the path, so a reload finds it without an encoded %2F", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toContain("Gold scrap")
  expect(html).not.toContain("Not Found")
})

test("the object type's own URL still opens the type, not an empty record", async () => {
  const client = new QueryClient()
  client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([
      createRoute({ getParentRoute: () => root, path: "/sources/$concept", component: ObjectTypePage }),
      createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: SourcesItemPage }),
    ]),
    history: createMemoryHistory({ initialEntries: ["/sources/canon.Holding"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
  expect(html).not.toContain("This record is not available yet")
  expect(html).toContain("Owner and location partitioned holding")
  expect(html).toContain("Facts recorded")
})

test("the overview says in one sentence what the record is and what its stored links show, and lists who points at it", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  // main's recorded snapshot stores two holdings of the gold item; the empty case is summaryOf's test below.
  expect(html).toContain("Gold scrap is an item record, read from ERPNext by mapping erpnext.three.mapping. 2 stored links connect it to other records.")
  expect(html).toContain("Holding records (item)")
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/owner/north/gold"')
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/owner/south/gold"')
})

test("stored links show on the overview in both directions; a link that is not stored shows nothing", async () => {
  // The shape of the recording once mapped link claims resolve (#877): one page of live links touching the object.
  const links = {
    objectType: "canon.Item", id: "demo/item/gold", hasMore: false,
    items: [
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/vault-1", targetType: "canon.Item", target: "demo/item/gold", version: 1 },
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/vault-2", targetType: "canon.Item", target: "demo/item/gold", version: 1 },
      // Not this object: never shown against it.
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/other", targetType: "canon.Item", target: "demo/item/silver", version: 1 },
      // The same id under another type is another record: never shown against this one.
      { sourceType: "canon.Holding", link: "item", source: "demo/holding/elsewhere", targetType: "canon.Party", target: "demo/item/gold", version: 1 },
    ],
  }
  const html = await page("canon.Item", "demo/item/gold", "business", links)
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/vault-1"')
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/vault-2"')
  expect(html).not.toContain("demo/holding/other")
  expect(html).not.toContain("demo/holding/elsewhere")
  expect(html).toContain("2 stored links connect it to other records.")
})

test("when the server returned only the first page of links, the summary does not claim the snapshot has none", () => {
  const concept = { name: "canon.Item", title: "Item", relationships: [] } as unknown as Parameters<typeof summaryOf>[0]
  const holding = { name: "canon.Holding", title: "Holding", relationships: [{ id: "item", target: "canon.Item" }] } as unknown as Parameters<typeof summaryOf>[0]
  const reads = (hasMore: boolean, items: unknown[]) => ({
    object: { document: { id: "demo/item/gold", objectType: "canon.Item", values: { label: "Gold scrap" } } },
    links: { document: { objectType: "canon.Item", id: "demo/item/gold", items, hasMore } },
    source: null,
  }) as unknown as Parameters<typeof summaryOf>[1]
  const edge = { sourceType: "canon.Holding", link: "item", source: "h1", targetType: "canon.Item", target: "demo/item/gold", version: 1 }
  expect(summaryOf(concept, reads(true, []), [holding])).toContain("No stored link appears on the first page the server returned; more may exist.")
  expect(summaryOf(concept, reads(true, [edge]), [holding])).toContain("At least 1 stored link connects it to other records.")
  expect(summaryOf(concept, reads(false, []), [holding])).toContain("Its one declared relation has no stored link in this snapshot.")
})

test("Business shows a type's plain summary only; Technical shows the registry's description and the type's id", async () => {
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const catalog = catalogOf((fixture("three-systems.definitions.json") as { document: Parameters<typeof catalogOf>[0] }).document)
  expect(catalog.byName.get("canon.Holding")).toMatchObject({ summary: "Owner and location partitioned holding", description: undefined })
  expect(catalog.byName.get("core.Resource")?.summary).toBeUndefined()
  expect(catalog.byName.get("core.Resource")?.description).toContain("valuation_method")
  const render = async (view: string) => {
    globalThis.localStorage = { getItem: () => view, setItem: () => {} } as unknown as Storage
    const client = new QueryClient()
    client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
    const root = createRootRoute()
    const router = createRouter({ routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept", component: ObjectTypePage })]), history: createMemoryHistory({ initialEntries: ["/sources/core.Resource"] }) })
    await router.load()
    return renderToStaticMarkup(<QueryClientProvider client={client}><ViewProvider><RouterProvider router={router} /></ViewProvider></QueryClientProvider>)
  }
  const business = await render("business")
  expect(business).not.toContain("valuation_method")
  expect(business).not.toContain(">core.Resource<")
  const technical = await render("technical")
  expect(technical).toContain("valuation_method")
  expect(technical).toContain(">core.Resource<")
})

test("the technical view shows the host's live access decision as the server wrote it, or its refusal, or the snapshot", async () => {
  // The host's access document (#1022) for a viewer: Population only.
  const document = JSON.stringify({ principal: { subjectId: "user:u1", actorKind: "user", roles: ["viewer"], roleTable: "studio-roles/1" }, policy: "demo.host.read", grantTable: "norn-cell-do-grants/1",
    capabilities: { population: { decision: "permit", rule: "norn-cell-do-grants/1:viewer" }, values: { decision: "deny", rule: null }, sources: { decision: "deny", rule: null } } })
  const live = await page("canon.Item", "demo/item/gold", "technical", undefined, { kind: "read", read: readDocument(document, "live") })
  expect(live).toContain("read live from the cell for your account")
  expect(live).toContain("Copy user:u1")
  expect(live).toContain("norn-cell-do-grants/1:viewer")
  expect(live.match(/>Permit</g)?.length).toBe(1)
  expect(live.match(/>Deny</g)?.length).toBe(2)
  expect(live).toContain("no rule grants it")
  const refused = await page("canon.Item", "demo/item/gold", "technical", undefined, { kind: "refused", refusal: { code: "authz.denied", message: "the read request is not authorized" } })
  expect(refused).toContain("the read request is not authorized")
  expect(refused).not.toContain(">Permit<")
  const unavailable = await page("canon.Item", "demo/item/gold", "technical", undefined, { kind: "unavailable", status: 503 })
  expect(unavailable).toContain("This page shows a recorded snapshot.")
})


const lineage = (digests: Partial<Pick<SourceLineage, "bundleDigest" | "documentDigest">>): SourceLineage => ({
  mappingPin: { artifactId: "demo.mapping", version: "0.1.0", sha256: "m".repeat(64) },
  sourceRecordRef: { system: "synthetic-erpnext", record: "Rows/1" },
  documentRef: { system: "synthetic-erpnext", record: "DOC-1" },
  fieldProvenance: {
    recovered: { source: { system: "synthetic-erpnext", record: "Rows/1" }, mappingVersion: "0.1.0", transform: "rule.recovered", recordedAt: "2026-01-01T00:00:00Z", sourceFields: ["recovered"] },
  },
  ...digests,
}) as SourceLineage

const html = (value: SourceLineage) => renderToStaticMarkup(<ProvenanceChain lineage={value} fact="recovered" />)

test("digests the server withheld read withheld, absent or null, and never leak an old value", () => {
  for (const withheld of [{}, { bundleDigest: null, documentDigest: null }]) {
    const out = html(lineage(withheld))
    expect(out.match(/withheld/g)?.length).toBe(2)
    expect(out).not.toContain("b".repeat(12))
    expect(out).not.toContain("d".repeat(12))
  }
})

test("disclosed digests render, and one withheld digest does not hide the other", () => {
  const both = html(lineage({ bundleDigest: "b".repeat(64), documentDigest: "d".repeat(64) }))
  expect(both).toContain("b".repeat(12))
  expect(both).toContain("d".repeat(12))
  expect(both).not.toContain("withheld")
  const one = html(lineage({ bundleDigest: "b".repeat(64) }))
  expect(one).toContain("b".repeat(12))
  expect(one.match(/withheld/g)?.length).toBe(1)
})

test("rendering again with a different lineage shows only that lineage's digests", () => {
  const first = html(lineage({ bundleDigest: "b".repeat(64), documentDigest: "d".repeat(64) }))
  const second = html(lineage({}))
  expect(first).toContain("b".repeat(12))
  expect(second).not.toContain("b".repeat(12))
})

test("LABEL-LOCALE-1: a partially labelled enum shows every declared value, labelled or as stored and marked English", async () => {
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const { FactsTable } = await import("../src/sources.tsx")
  const { LocaleProvider } = await import("../src/lib/locale.tsx")
  const envelope = (artifactId: string, kind: string, payload: unknown) => ({
    irVersion: "0.1", artifactId, kind, namespace: "sales", name: "Invoice", version: "1.0.0", lifecycle: "active",
    scope: {}, dependencies: [], provenance: { source: "system" }, payload,
  })
  const values = ["draft", "posted", "on_hold", "void"]
  const status = { kind: "enumValue", objectType: "sales.Invoice", property: "status" }
  const artifacts = [
    envelope("sales.Invoice.entity", "entity", { entityVersion: "2.0", objectType: { name: "sales.Invoice", properties: [{ id: "status", type: { kind: "enum", values: values.map((value) => ({ value })) } }] } }),
    envelope("sales.labels", "labels", { labelsVersion: "0.1", entries: [
      { target: { ...status, value: "posted" }, label: { ar: "مُرحّلة" } },
      { target: { ...status, value: "void" }, label: { ar: "ملغاة" } },
    ] }),
  ] as unknown as Parameters<typeof catalogOf>[0]
  const concept = catalogOf(artifacts, "ar").byName.get("sales.Invoice")!
  for (const technical of [false, true]) {
    const html = renderToStaticMarkup(<LocaleProvider initial="ar"><FactsTable concept={concept} technical={technical} /></LocaleProvider>)
    // Every declared value is on the page, in declaration order: nothing is hidden for lack of a label.
    const shown = ["draft", "مُرحّلة", technical ? "on_hold" : "on hold", "ملغاة"]
    const at = shown.map((text) => html.indexOf(text))
    expect(at.every((index) => index >= 0)).toBe(true)
    expect([...at].sort((a, b) => a - b)).toEqual(at)
    expect(html).toContain(`data-values="${values.length}"`)
    // An unlabelled value is marked as English; a labelled one is not.
    expect(html).toMatch(/<bdi[^>]*lang="en"[^>]*>draft<\/bdi>/)
    expect(html).not.toMatch(/lang="en"[^>]*>مُرحّلة/)
  }
  // With no label at all in the reader's language, the type's own description lists every value.
  const english = renderToStaticMarkup(<FactsTable concept={catalogOf(artifacts, "en").byName.get("sales.Invoice")!} technical={false} />)
  expect(english).toContain("One of: draft, posted, on hold, void")
})

test("an English summary under an Arabic title is marked lang=en; an Arabic one carries its own tag", async () => {
  const { catalogOf } = await import("../src/lib/catalog.ts")
  const document = (fixture("three-systems.definitions.json") as { document: Parameters<typeof catalogOf>[0] }).document
  const arabic = catalogOf(document, "ar")
  // The locale pack describes canon.Holding in Arabic; core.Agent has an Arabic label and no Arabic description.
  expect(arabic.byName.get("canon.Holding")).toMatchObject({ titleLang: "ar", summary: "رصيد مقسَّم حسب المالك والموقع", summaryLang: "ar" })
  expect(arabic.byName.get("core.Agent")).toMatchObject({ titleLang: "ar", summaryLang: "en" })
  expect(catalogOf(document, "en").byName.get("canon.Holding")).toMatchObject({ titleLang: "en", summary: "Owner and location partitioned holding", summaryLang: "en" })
})
