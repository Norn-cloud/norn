import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { renderToStaticMarkup } from "react-dom/server"
import { ObjectPage } from "../src/objects.tsx"
import { ViewProvider } from "../src/lib/view.tsx"
import { recordedTransport } from "../src/lib/transport.ts"

/** The record page at its route, with the recorded reads already answered. */
async function page(concept: string, id: string, view = "business") {
  globalThis.localStorage = { getItem: () => view, setItem: () => {} } as unknown as Storage
  const client = new QueryClient()
  client.setQueryData(["catalog"], await recordedTransport.catalog())
  client.setQueryData(["object", concept, id], await recordedTransport.object(concept, id))
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept/$id", component: ObjectPage })]),
    history: createMemoryHistory({ initialEntries: [`/sources/${concept}/${encodeURIComponent(id)}`] }),
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
  expect(html).toContain("None recorded: the demo read has no signed-in principal")
  expect(html).toContain("&quot;objectType&quot;: &quot;canon.Item&quot;")
})
