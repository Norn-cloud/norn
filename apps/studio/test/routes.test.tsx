import { expect, mock, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { Outlet, RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { renderToStaticMarkup } from "react-dom/server"
import { PAGES, SERVED_BY, type PagePath } from "../src/lib/pages.ts"

// The recorded transport reads fixtures through Vite's import.meta.glob, which bun test lacks.
mock.module("../src/lib/transport.ts", () => ({ transport: {}, recordedTransport: {} }))
const { routeTree } = await import("../src/routes.tsx")

test("every page in PAGES renders through the router at its example URL, matched to its own route, without an error screen", async () => {
  for (const path of Object.keys(PAGES) as PagePath[]) {
    const router = createRouter({ routeTree: routeTree(createRootRoute({ component: Outlet })), history: createMemoryHistory({ initialEntries: [PAGES[path]] }) })
    await router.load()
    expect({ path, matched: router.state.matches.at(-1)?.routeId }).toEqual({ path, matched: SERVED_BY[path] ?? path })
    const html = renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>)
    expect({ path, empty: html.trim() === "" }).toEqual({ path, empty: false })
    for (const broken of ["Something went wrong", "Not Found", "data is undefined"]) expect({ path, broken: html.includes(broken) }).toEqual({ path, broken: false })
  }
})
