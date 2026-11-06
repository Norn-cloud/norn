import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { renderToStaticMarkup } from "react-dom/server"
import { readDocument } from "../src/lib/exact.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks; the reads are answered below instead.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { ObjectPage, SourcesItemPage } = await import("../src/objects.tsx")
const { ObjectTypePage } = await import("../src/sources.tsx")

const fixture = (name: string) => readDocument(readFileSync(join(import.meta.dir, "../../../fixtures/demo/explorer", name), "utf8"), "recorded")

/** The record page at its route, with the recorded reads already answered. */
async function page(concept: string, id: string) {
  const client = new QueryClient()
  const file = (read: string) => fixture(`objects/${concept}/${id.replaceAll("/", "~")}.${read}.json`)
  client.setQueryData(["catalog"], fixture("three-systems.definitions.json"))
  client.setQueryData(["object", concept, id], { object: file("object"), links: file("links"), source: file("source") })
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: ObjectPage })]),
    history: createMemoryHistory({ initialEntries: [`/sources/${concept}/${id}`] }),
  })
  await router.load()
  return renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
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
