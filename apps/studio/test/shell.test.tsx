import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { ListDetail } from "@/components/list-detail"
import { SidebarProvider } from "@/components/ui/sidebar"
import { Submenu, sectionOf, target, type Section } from "../src/shell.tsx"

const sources: Section = {
  path: "/sources", label: "Sources", icon: Database, summary: "",
  useMenu: () => [{ href: "/sources/canon.Holding", label: "Holding", group: "Object types" }],
}
const questions: Section = { path: "/questions", label: "Questions", icon: ChartLine, summary: "" }

/** Renders a node at a route, the way the app's router would. */
async function at(pathname: string, node: () => ReactNode) {
  const router = createRouter({ routeTree: createRootRoute({ component: node }), history: createMemoryHistory({ initialEntries: [pathname] }) })
  await router.load()
  return renderToStaticMarkup(<RouterProvider router={router} />)
}

test("a submenu shows its title, data context and entries, and no action the server has not declared", async () => {
  const html = await at("/sources/canon.Holding", () => <SidebarProvider><Submenu section={sources} /></SidebarProvider>)
  expect(html).toContain("Sources")
  expect(html).toContain("Recorded · synthetic world")
  expect(html).toContain('href="/sources/canon.Holding"')
  expect(html).toContain('data-active="true"')
  expect(html).not.toContain('data-slot="button"')
})

test("a record page belongs to the section above it", () => {
  expect(sectionOf([sources, questions], "/sources")).toBe(sources)
  expect(sectionOf([sources, questions], "/sources/canon.Holding")).toBe(sources)
  expect(sectionOf([sources, questions], "/sourcesx")).toBeUndefined()
})

test("an entry is a route, or an anchor on its section's page", () => {
  expect(target(sources, "/sources/canon.Holding")).toEqual({ to: "/sources/canon.Holding" })
  expect(target(questions, "#corrections")).toEqual({ to: "/questions", hash: "corrections" })
})

test("a list beside a record groups its items in order and marks the open one", async () => {
  const html = await at("/questions/b", () => (
    <ListDetail title="Questions" placeholder="Find a question" selected="b" items={[
      { id: "a", to: "/questions/a", title: "Alpha", group: "Holding" },
      { id: "b", to: "/questions/b", title: "Beta", group: "Lot" },
    ]}>detail</ListDetail>
  ))
  expect(html.indexOf("Holding")).toBeLessThan(html.indexOf("Lot"))
  expect(html).toMatch(/aria-current="page"[^>]*href="\/questions\/b"|href="\/questions\/b"[^>]*aria-current="page"/)
  expect(html).toContain("detail")
})
