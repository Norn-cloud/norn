import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { readDocument } from "../src/lib/exact.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks; the reads are answered below instead.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { ObjectPage, SourcesItemPage, summaryOf } = await import("../src/objects.tsx")
const { ObjectTypePage } = await import("../src/sources.tsx")
const { ViewProvider } = await import("../src/lib/view.tsx")

const fixture = (name: string) => readDocument(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8"), "recorded")

/** The record page at its route, with the recorded reads already answered. */
async function page(concept: string, id: string, view = "business", links?: unknown) {
  globalThis.localStorage = { getItem: () => view, setItem: () => {} } as unknown as Storage
  const client = new QueryClient()
  const file = (read: string) => fixture(`objects/${concept}/${id.replaceAll("/", "~")}.${read}.json`)
  client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
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
  for (const tab of ["Overview", "Relations", "Lineage", "Guarantees", "Ledger", "Access"]) expect(html).toContain(`>${tab}</button>`)
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
  expect(html).toContain("Copy 6ec08e5b9fc6375526a98b96eeff65f402fbad558e4df418ed603f32b3017980")
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
  expect(html).toContain("canon.Holding")
})

test("the overview says in one sentence what the record is and what its stored links show, and lists who points at it", async () => {
  const html = await page("canon.Item", "demo/item/gold")
  expect(html).toContain("Gold scrap is an item record, read from ERPNext by mapping erpnext.three.mapping. Its one declared relation has no stored link in this snapshot.")
  expect(html).toContain("Holding records (item)")
  expect(html).toContain("none in this snapshot")
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
    ],
  }
  const html = await page("canon.Item", "demo/item/gold", "business", links)
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/vault-1"')
  expect(html).toContain('href="/sources/canon.Holding/demo/holding/vault-2"')
  expect(html).not.toContain("demo/holding/other")
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
