import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Shell, decoded, sectionOf, target, type Section } from "../src/shell.tsx"
import { ViewProvider } from "../src/lib/view.tsx"
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
  expect(html).toContain("Jewelry")
  expect(html).toContain("Viewing as Recorded demo")
})

test("only the recorded jewelry world opens; the NC benchmark points to its offline results; the rest say why they are empty", () => {
  expect(new Set(WORKSPACES.map(({ group }) => group))).toEqual(new Set(["Class demos", "Industry benchmarks", "First-party"]))
  expect(WORKSPACES.filter(({ recorded }) => recorded).map(({ id, group }) => `${group}/${id}`)).toEqual(["First-party/jewelry"])
  const olist = WORKSPACES.find(({ id }) => id === "olist")
  expect(olist?.detail).toBe("Benchmark run offline (licence NC)")
  expect(olist?.results).toBe("https://github.com/Norn-cloud/norn/issues/531")
  expect(WORKSPACES.find(({ group }) => group === "Class demos")?.detail).toBe("Coming soon")
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

test("a malformed escape in a record path is shown as written, not thrown", async () => {
  expect(decoded("demo%2Fitem")).toBe("demo/item")
  expect(decoded("%E0%A4%A")).toBe("%E0%A4%A")
  // Through the whole shell: the breadcrumbs render the segment as written.
  const html = await at("/sources/canon.Holding/%E0%A4%A", () => <Shell sections={[sources, questions]} />)
  expect(html).toContain("%E0%A4%A")
})

test("a browser that throws on storage still renders, in the business view", () => {
  const saved = Object.getOwnPropertyDescriptor(globalThis, "localStorage")
  Object.defineProperty(globalThis, "localStorage", { configurable: true, get: () => { throw new DOMException("blocked", "SecurityError") } })
  try {
    expect(renderToStaticMarkup(<ViewProvider><span>ok</span></ViewProvider>)).toBe("<span>ok</span>")
  } finally {
    if (saved) Object.defineProperty(globalThis, "localStorage", saved); else delete (globalThis as { localStorage?: unknown }).localStorage
  }
})

test("the search button keeps its name when its label is hidden on phones", async () => {
  const html = await at("/sources", () => <Shell sections={[sources, questions]} />)
  expect(html).toMatch(/<button[^>]*aria-label="Search"/)
})
