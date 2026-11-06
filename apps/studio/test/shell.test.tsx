import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Shell, sectionOf, target, type Section } from "../src/shell.tsx"
import { WORKSPACES } from "../src/lib/workspaces.ts"

const sources: Section = {
  path: "/sources", label: "Sources", icon: Database, summary: "", kind: "source", group: "Model",
  useMenu: () => [{ href: "/sources/canon.Holding", label: "Holding", group: "Object types", kind: "objectType" }],
}
const questions: Section = { path: "/questions", label: "Questions", icon: ChartLine, summary: "", group: "Model" }

/** Renders a node at a route, the way the app's router would. */
async function at(pathname: string, node: () => ReactNode) {
  const router = createRouter({ routeTree: createRootRoute({ component: node }), history: createMemoryHistory({ initialEntries: [pathname] }) })
  await router.load()
  return renderToStaticMarkup(<RouterProvider router={router} />)
}

test("the open section lists its entries under their heading, the open entry marked", async () => {
  const html = await at("/sources/canon.Holding", () => <Shell sections={[sources, questions]} />)
  expect(html).toContain("Object types")
  expect(html).toContain('href="/sources/canon.Holding"')
  expect(html).toMatch(/data-sidebar="menu-sub-button"[^>]*data-active="true"/)
  expect(html).toContain("[&amp;&gt;svg]:text-blue-600")
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

test("the breadcrumbs name the section, the item and the record id", async () => {
  const html = await at("/sources/canon.Holding/demo%2Fitem%2Fgold", () => <Shell sections={[sources, questions]} />)
  const crumbs = html.match(/<nav aria-label="breadcrumb"[\s\S]*?<\/nav>/)?.[0] ?? ""
  expect(crumbs).toContain('href="/sources"')
  expect(crumbs).toContain('href="/sources/canon.Holding"')
  expect(crumbs).toContain("demo/item/gold")
})

test("the page bar holds search, the view toggle, shortcuts and notifications; the sidebar names the workspace and the viewer", async () => {
  const html = await at("/sources", () => <Shell sections={[sources, questions]} />)
  expect(html).toContain("Search…")
  expect(html).toContain('aria-label="Business or technical view"')
  expect(html).toMatch(/aria-checked="true"[^>]*>Business</)
  expect(html).toContain("Keyboard shortcuts")
  expect(html).toContain("Notifications")
  expect(html).toContain("Synthetic world")
  expect(html).toContain("Viewing as Recorded demo")
})

test("only workspaces with recorded documents open; the rest say they have none", () => {
  expect(new Set(WORKSPACES.map(({ group }) => group))).toEqual(new Set(["Class demos", "Industry benchmarks", "First-party"]))
  expect(WORKSPACES.filter(({ recorded }) => recorded).map(({ id }) => id)).toEqual(["synthetic"])
  for (const workspace of WORKSPACES.filter(({ recorded }) => !recorded)) expect(workspace.detail).toBe("No recorded run in Studio yet")
})

test("a long section shows six entries and offers the rest, keeping the open one in view", async () => {
  const many: Section = { ...sources, useMenu: () => Array.from({ length: 9 }, (_, index) => ({ href: `/sources/t${index}`, label: `Type ${index}`, group: "Object types" })) }
  const html = await at("/sources/t8", () => <Shell sections={[many]} />)
  expect(html).toContain("Type 5")
  expect(html).not.toContain(">Type 6<")
  expect(html).toContain("Type 8")
  expect(html).toContain("Show 2 more")
})

test("a section's count sits in the theme's destructive pair", async () => {
  const counted: Section = { ...questions, useBadge: () => 3 }
  const html = await at("/sources", () => <Shell sections={[sources, counted]} />)
  const badge = html.match(/<span class="[^"]*bg-destructive[^"]*"[^>]*>3<\/span>/)?.[0] ?? ""
  expect(badge).toContain("text-destructive-foreground")
  expect(badge).not.toContain("text-white")
})
