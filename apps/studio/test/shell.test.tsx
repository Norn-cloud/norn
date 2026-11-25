import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database, House, Rows3 } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { modeQuery, type Mode } from "../src/lib/live-transport.ts"
import type { Account } from "../src/lib/session.ts"
import { Shell, decoded, diffExpanded, initials, perspectiveOf, sectionOf, target, type MenuEntry, type Section } from "../src/shell.tsx"
import { isEmptyWorkspace } from "../src/home.tsx"
import { ViewProvider } from "../src/lib/view.tsx"
import { WORKSPACES } from "../src/lib/workspaces.ts"
import { WORKSPACES_QUERY_KEY, type WorkspacesDocument } from "../src/lib/workspaces-api.ts"

const account: Account = { user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" }, organization: { id: "o1", name: "Analytical", role: "viewer" } }

const sources: Section = {
  path: "/sources", label: "Sources", icon: Database, perspective: "builder", kind: "source", group: "Model",
  useMenu: () => [{ href: "/sources/canon.Holding", label: "Holding", group: "Object types", kind: "objectType" }],
}
const questions: Section = { path: "/questions", label: "Questions", icon: ChartLine, perspective: "builder", group: "Model" }
const home: Section = { path: "/", label: "Home", icon: House, perspective: "global" }
const records: Section = {
  path: "/objects", label: "Records", icon: Rows3, perspective: "workspace", group: "Data", index: false,
  useMenu: () => [{ href: "/objects/canon.Holding", also: "/sources/canon.Holding", label: "Holding", kind: "objectType" }],
}

/** Renders a node at a route, the way the app's router would. */
async function at(pathname: string, node: () => ReactNode) {
  const router = createRouter({ routeTree: createRootRoute({ component: node }), history: createMemoryHistory({ initialEntries: [pathname] }) })
  await router.load()
  // The shell reads the session and the cell's mode as queries, as the app's root provides them.
  return renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>)
}

test("the open section lists its entries under their heading, the open entry marked", async () => {
  const html = await at("/sources/canon.Holding", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
  expect(html).toContain("Object types")
  expect(html).toContain('href="/sources/canon.Holding"')
  // The current page's item carries aria-current, and its row links to the page.
  expect(html).toMatch(/role="treeitem"[^>]*aria-current="page"[^>]*>\s*<a[^>]*href="\/sources\/canon.Holding"/)
  // Exactly one row is current (DESIGN.md 9.1): the router's own marking on the matching links is dropped.
  const nav = html.slice(html.indexOf('aria-label="Studio navigation"'), html.indexOf("</nav>", html.indexOf('aria-label="Studio navigation"')))
  expect(nav.match(/aria-current="page"/g)).toHaveLength(1)
  // A breadcrumb back to a parent page is not the current page either.
  expect(html).not.toMatch(/data-slot="breadcrumb-link"[^>]*aria-current/)
  // Rows are muted at rest; no kind colour on sidebar icons (colour belongs to dots and marks).
  expect(html).not.toContain("[&amp;&gt;svg]:text-kind")
})

test("a record page belongs to the section above it", () => {
  expect(sectionOf([sources, questions], "/sources")).toBe(sources)
  expect(sectionOf([sources, questions], "/sources/canon.Holding")).toBe(sources)
  expect(sectionOf([sources, questions], "/sourcesx")).toBeUndefined()
  // A type's records live under its applet's other prefix.
  const owning = { ...sources, paths: ["/objects"] }
  expect(sectionOf([owning, questions], "/objects/canon.Holding")).toBe(owning)
  expect(sectionOf([owning, questions], "/objectsx")).toBeUndefined()
})

test("an entry is a route, or an anchor on its section's page", () => {
  expect(target(sources, "/sources/canon.Holding")).toEqual({ to: "/sources/canon.Holding" })
  expect(target(questions, "#corrections")).toEqual({ to: "/questions", hash: "corrections" })
})

test("the breadcrumbs name the section, the item and the record id", async () => {
  const html = await at("/sources/canon.Holding/demo%2Fitem%2Fgold", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
  const crumbs = html.match(/<nav aria-label="breadcrumb"[\s\S]*?<\/nav>/)?.[0] ?? ""
  expect(crumbs).toContain('href="/sources"')
  expect(crumbs).toContain('href="/sources/canon.Holding"')
  expect(crumbs).toContain("demo/item/gold")
})

test("the page bar holds search, shortcuts and notifications (the view and language live in the account menu); the sidebar names the workspace and the viewer", async () => {
  const html = await at("/sources", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
  expect(html).toContain("Search…")
  expect(html).not.toContain('aria-label="Business or technical view"')
  // The phone's view-and-language menu is not drawn on a desktop, so it never shadows the account menu's choices.
  expect(html).not.toContain("Language and view")
  expect(html).toContain("Keyboard shortcuts")
  expect(html).toContain("Notifications")
  // Before the mode is decided the switcher says it is connecting; it names the recording only once Studio knows there is no cell.
  expect(html).toContain("Connecting…")
  expect(html).not.toContain("Recorded · synthetic gold-scrap data")
  expect(html).toContain("Ada Lovelace")
})

test("only the recorded demo world opens; the NC benchmark points to its offline results; the rest say why they are empty", () => {
  expect(new Set(WORKSPACES.map(({ group }) => group))).toEqual(new Set(["Class demos", "Industry benchmarks", "First-party"]))
  expect(WORKSPACES.filter(({ recorded }) => recorded).map(({ id, group }) => `${group}/${id}`)).toEqual(["First-party/recorded-demo"])
  const olist = WORKSPACES.find(({ id }) => id === "olist")
  expect(olist?.detail).toBe("Benchmark run offline (licence NC)")
  expect(olist?.results).toBe("https://github.com/Norn-cloud/norn/issues/531")
  expect(WORKSPACES.find(({ group }) => group === "Class demos")?.detail).toBe("Coming soon")
})

test("one Studio navigation landmark holds the shared destinations, then the perspective's spaces, counting each group of items", async () => {
  const many: Section = { ...sources, useMenu: () => Array.from({ length: 9 }, (_, index) => ({ href: `/sources/t${index}`, label: `Type ${index}`, group: "Object types" })) }
  const html = await at("/sources/t8", () => <Shell sections={[home, many, questions, records]} account={account} onSignOut={() => {}} />)
  expect(html.match(/<nav[^>]*aria-label="Studio navigation"/g)).toHaveLength(1)
  expect(html).not.toMatch(/role="toolbar"[^>]*aria-label="Studio"/)
  expect(html).toMatch(/role="tree"[^>]*aria-label="Builder pages"|aria-label="Builder pages"[^>]*role="tree"/)
  // The shared destinations lead the list, as rows of the same tree.
  const nav = html.slice(html.indexOf('aria-label="Studio navigation"'))
  expect(nav.indexOf('href="/"')).toBeGreaterThan(-1)
  expect(nav.indexOf('href="/"')).toBeLessThan(nav.indexOf('href="/questions"'))
  expect(html).toMatch(/>Model<\/span>/)
  expect(html).toMatch(/href="\/questions"[^>]*>[\s\S]*?Questions/)
  expect(html).toMatch(/>Object types<\/span><span[^>]*>9<\/span>/)
  expect(html).toContain("Type 8")
  // Group headers are quiet: step 1, weight 400, sentence case (no uppercase or tracking).
  const header = /<span[^>]*data-slot="tree-label"[^>]*class="([^"]*)"[^>]*>\s*<span[^>]*>Model<\/span>/.exec(html)?.[1] ?? /class="([^"]*group\/heading[^"]*)"/.exec(html)?.[1]
  expect(header).toBeDefined()
  expect(header).toContain("text-meta")
  expect(header).not.toMatch(/uppercase|font-semibold|tracking-/)
  // The other perspective's destinations are not listed; the switch says which one is shown.
  expect(html).not.toContain(">Records<")
  expect(html).toMatch(/<button[^>]*aria-pressed="true"[^>]*>[\s\S]*?Builder/)
  // The toggle is named "Sidebar" in both states and controls the column it names.
  expect(html).toMatch(/<button[^>]*aria-label="Sidebar"[^>]*aria-expanded="true"[^>]*aria-controls="studio-sidebar"/)
  expect(html).toContain('id="studio-sidebar"')
})

test("the perspective is the reader's while the page belongs to it, else the one that lists the page", () => {
  const menus = new Map<string, readonly MenuEntry[]>([["/objects", records.useMenu?.() ?? []]])
  const all = [home, sources, questions, records]
  expect(perspectiveOf(all, "/", "workspace", menus)).toBe("workspace")
  expect(perspectiveOf(all, "/", "builder", menus)).toBe("builder")
  expect(perspectiveOf(all, "/sources", "workspace", menus)).toBe("builder")
  expect(perspectiveOf(all, "/objects/canon.Holding", "workspace", menus)).toBe("workspace")
  // A record page is the operator's when one of their items claims it, and the builder's otherwise.
  expect(perspectiveOf(all, "/sources/canon.Holding/demo%2Fitem", "workspace", menus)).toBe("workspace")
  expect(perspectiveOf(all, "/sources/canon.Holding/demo%2Fitem", "builder", menus)).toBe("builder")
  expect(perspectiveOf(all, "/sources/canon.Holding/demo%2Fitem", "workspace", new Map())).toBe("builder")
})

test("in the workspace a record page reads Records, its type, then its id", async () => {
  const html = await at("/sources/canon.Holding/demo%2Fitem%2Fgold", () => <Shell sections={[home, sources, records]} account={account} onSignOut={() => {}} />)
  const crumbs = html.match(/<nav aria-label="breadcrumb"[\s\S]*?<\/nav>/)?.[0] ?? ""
  expect(crumbs).toContain("Records")
  expect(crumbs).not.toContain('href="/objects"')
  expect(crumbs).toContain('href="/objects/canon.Holding"')
  expect(crumbs).toContain("demo/item/gold")
})

test("a section's count is a quiet step-1 number, never a filled badge", async () => {
  const counted: Section = { ...questions, useBadge: () => 3 }
  const html = await at("/sources", () => <Shell sections={[sources, counted]} account={account} onSignOut={() => {}} />)
  const count = html.match(/<span[^>]*data-count[^>]*>3<\/span>/)?.[0] ?? ""
  expect(count).toContain("text-meta")
  expect(count).not.toMatch(/bg-|font-semibold|rounded-full/)
})

test("a malformed escape in a record path is shown as written, not thrown", async () => {
  expect(decoded("demo%2Fitem")).toBe("demo/item")
  expect(decoded("%E0%A4%A")).toBe("%E0%A4%A")
  // Through the whole shell: the breadcrumbs render the segment as written.
  const html = await at("/sources/canon.Holding/%E0%A4%A", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
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
  const html = await at("/sources", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
  expect(html).toMatch(/<button[^>]*aria-label="Search"/)
})

test("the rail shows the signed-in user's initials and never the recorded-demo placeholder", async () => {
  const html = await at("/sources", () => <Shell sections={[sources]} account={account} onSignOut={() => {}} />)
  expect(html).toContain(">AL<")
  expect(html).toContain("Analytical · viewer")
  expect(html).not.toContain("Recorded demo")
  expect(initials("")).toBe("?")
  expect(initials("ada@example.com")).toBe("A")
})

test("an assistant mounts in the shell's panel, opened from the rail; a page can hand it a prompt", async () => {
  const html = await at("/sources", () => <Shell sections={[sources]} assistant={{ title: "Assistant", content: <p>agent here</p> }} account={account} onSignOut={() => {}} />)
  expect(html).toMatch(/<button[^>]*aria-label="Assistant"[^>]*aria-pressed="false"/)
  expect(html).toMatch(/data-slot="assistant-panel"[^>]*hidden/)
  expect(html).toContain("agent here")
  const none = await at("/sources", () => <Shell sections={[sources]} account={account} onSignOut={() => {}} />)
  expect(none).not.toContain('data-slot="assistant-panel"')
  expect(none).not.toContain('aria-label="Assistant"')
})

test("collapsing one space records only that branch, leaving untouched destinations free to auto-expand when active", () => {
  // Initial expanded branches on Home: spaces are open, Records is open, Metrics (inactive) is closed by default.
  const before = ["space:Data", "space:Analysis", "space:Operations", "dest:/objects"]
  const after = ["space:Data", "space:Analysis", "dest:/objects"]
  const chosen = diffExpanded(before, after, {})
  expect(chosen).toEqual({ "space:Operations": false })
  // Untouched destination dest:/metrics stays absent from chosen, so navigating to /metrics lets its default (true) apply.
  expect(chosen["dest:/metrics"]).toBeUndefined()
  // Expanding an untouched destination later records only that destination as true while preserving the earlier space collapse.
  const next = diffExpanded(after, [...after, "dest:/metrics"], chosen)
  expect(next).toEqual({ "space:Operations": false, "dest:/metrics": true })
})

test("#1414: the workspace switcher shows the workspace name and role, never raw org or environment ids", async () => {
  const rawAccount: Account = {
    user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" },
    organization: { id: "MPu3nAkCMSqDEjamX5eb9IB6or836fCS", name: "Analytical", role: "owner" },
  }
  const client = new QueryClient()
  const scope = { orgId: "MPu3nAkCMSqDEjamX5eb9IB6or836fCS", environmentId: "rehearsal2" }
  client.setQueryData(["session"], rawAccount)
  client.setQueryData(["studio-scope"], scope)
  const liveMode: Mode = {
    kind: "live",
    scope,
    catalog: { document: [], numbers: new Map(), origin: "live" },
    counts: {},
    publishedQuestions: new Set(),
  }
  client.setQueryData(modeQuery(scope).queryKey, liveMode)
  const doc: WorkspacesDocument = {
    workspaces: [{ id: "MPu3nAkCMSqDEjamX5eb9IB6or836fCS", name: "Analytical", slug: "analytical", role: "owner", active: true, hasCell: true }],
  }
  client.setQueryData(WORKSPACES_QUERY_KEY, doc)
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <Shell sections={[sources]} account={rawAccount} onSignOut={() => {}} /> }),
    history: createMemoryHistory({ initialEntries: ["/sources"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>)
  expect(html).toContain("Analytical")
  expect(html).toContain("Live · Owner")
  expect(html).not.toContain("MPu3nAkCMSqDEjamX5eb9IB6or836fCS")
  expect(html).not.toContain("rehearsal2")
  expect(html).not.toContain("New workspace")
  expect(html).not.toContain("Archive workspace")
  expect(html).not.toContain("Delete workspace")
})

test("#1414: isEmptyWorkspace identifies cells holding only trusted base artifacts", () => {
  expect(isEmptyWorkspace([])).toBe(true)
  expect(isEmptyWorkspace([{ artifactId: "core.units" }, { artifactId: "std.value_types" }])).toBe(true)
  expect(isEmptyWorkspace([{ artifactId: "core.units" }, { artifactId: "canon.Holding.entity" }])).toBe(false)
})
