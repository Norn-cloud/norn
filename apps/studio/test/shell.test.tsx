import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { SidebarProvider } from "@/components/ui/sidebar"
import { Shell, Submenu, sectionOf, target, type Section } from "../src/shell.tsx"

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

test("a submenu offers a search over its entries", async () => {
  const html = await at("/sources", () => <SidebarProvider><Submenu section={sources} /></SidebarProvider>)
  expect(html).toContain('placeholder="Find in Sources"')
})

test("the rail divides its groups once, and the page bar holds search, shortcuts and notifications", async () => {
  const rules: Section = { path: "/workflows", label: "Workflows", icon: Database, summary: "", group: "rules" }
  const html = await at("/sources", () => <Shell sections={[{ ...sources, group: "model" }, { ...questions, group: "model" }, rules]} />)
  expect(html.match(/data-slot="separator"/g)?.length).toBe(1)
  expect(html).toContain("Search or add…")
  expect(html).toContain("Keyboard shortcuts")
  expect(html).toContain("Notifications")
  expect(html).not.toContain(">RD<")
})

test("a long group shows six entries and offers the rest, keeping the open one in view", async () => {
  const many: Section = { ...sources, useMenu: () => Array.from({ length: 9 }, (_, index) => ({ href: `/sources/t${index}`, label: `Type ${index}`, group: "Object types" })) }
  const html = await at("/sources/t8", () => <SidebarProvider><Submenu section={many} /></SidebarProvider>)
  expect(html).toContain("Type 5")
  expect(html).not.toContain("Type 6")
  expect(html).toContain("Type 8")
  expect(html).toContain("Show more")
})

test("a rail count sits inside its tile in the theme's destructive pair", async () => {
  const counted: Section = { ...questions, useBadge: () => 3 }
  const html = await at("/sources", () => <Shell sections={[sources, counted]} />)
  const badge = html.match(/<span class="[^"]*bg-destructive[^"]*"[^>]*>3<\/span>/)?.[0] ?? ""
  expect(badge).toContain("text-destructive-foreground")
  expect(badge).toContain("top-0.5 right-0.5")
  expect(badge).not.toContain("text-white")
})
