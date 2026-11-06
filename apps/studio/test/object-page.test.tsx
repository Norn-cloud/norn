import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRoute, createRouter } from "@tanstack/react-router"
import { renderToStaticMarkup } from "react-dom/server"
import { ObjectPage } from "../src/objects.tsx"
import { recordedTransport } from "../src/lib/transport.ts"

/** The record page at its route, with the recorded reads already answered. */
async function page(concept: string, id: string) {
  const client = new QueryClient()
  client.setQueryData(["catalog"], await recordedTransport.catalog())
  client.setQueryData(["object", concept, id], await recordedTransport.object(concept, id))
  const root = createRootRoute()
  const router = createRouter({
    routeTree: root.addChildren([createRoute({ getParentRoute: () => root, path: "/sources/$concept/$id", component: ObjectPage })]),
    history: createMemoryHistory({ initialEntries: [`/sources/${concept}/${encodeURIComponent(id)}`] }),
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
