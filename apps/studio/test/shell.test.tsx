import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { Blocks, Bookmark, ChartLine, Database, House, Inbox, Rows3, Table2 } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { modeQuery, type Mode } from "../src/lib/live-transport.ts"
import {
  SHOW_FIRST, appletHome, appletMenu, appletOf, appletsOf, currentIn, decoded, diffExpanded, initials, matches, placeOf, sectionOf, target, visibleItems,
  type Applet, type MenuEntry, type Section,
} from "../src/lib/nav-model.ts"
import type { Account } from "../src/lib/session.ts"
import { Shell } from "../src/shell.tsx"
import { appletBadge } from "../src/shell-nav.tsx"
import { isEmptyWorkspace } from "../src/home.tsx"
import { ViewProvider } from "../src/lib/view.tsx"
import { WORKSPACES } from "../src/lib/workspaces.ts"
import { WORKSPACES_QUERY_KEY, type WorkspacesDocument } from "../src/lib/workspaces-api.ts"

const account: Account = { user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" }, organization: { id: "o1", name: "Analytical", role: "viewer" } }

const DATA: Applet = { id: "data", label: "Data", icon: Table2 }
const BUILDER: Applet = { id: "builder", label: "Builder", icon: Blocks }
const sources: Section = {
  path: "/sources", label: "Sources", icon: Database, perspective: "builder", applet: BUILDER, kind: "source", group: "Model",
  useMenu: () => [{ href: "/sources/canon.Holding", label: "Holding", group: "Object types", kind: "objectType" }],
}
const questions: Section = { path: "/questions", label: "Questions", icon: ChartLine, perspective: "builder", applet: BUILDER, group: "Model" }
const home: Section = { path: "/", label: "Home", icon: House, perspective: "global" }
const inbox: Section = { path: "/decisions", label: "Inbox", icon: Inbox, perspective: "global", useBadge: () => 3 }
const records: Section = {
  path: "/objects", label: "Records", icon: Rows3, perspective: "workspace", applet: DATA, index: false,
  useMenu: () => [{ href: "/objects/canon.Holding", also: "/sources/canon.Holding", label: "Holding", kind: "objectType" }],
}
const views: Section = {
  path: "/views", label: "Views", icon: Bookmark, perspective: "workspace", applet: DATA, pinned: true,
  useMenu: () => [{ href: "/views/a", label: "Open items" }, { href: "/views/b", label: "Stuck", group: "Builder" }, { href: "/views/c", label: "Night shift", group: "Floor" }],
}

/** Renders a node at a route, the way the app's router would. */
async function at(pathname: string, node: () => ReactNode) {
  const router = createRouter({ routeTree: createRootRoute({ component: node }), history: createMemoryHistory({ initialEntries: [pathname] }) })
  await router.load()
  // The shell reads the session and the cell's mode as queries, as the app's root provides them.
  return renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>)
}

/** The markup of the landmark with this name. */
const landmark = (html: string, name: string) => {
  const start = html.indexOf(`aria-label="${name}"`)
  return start < 0 ? "" : html.slice(start, html.indexOf("</nav>", start))
}

test("two landmarks: the Applets rail with exactly one current applet, and the open applet's menu with exactly one current row", async () => {
  const html = await at("/sources/canon.Holding", () => <Shell sections={[home, inbox, records, sources, questions]} account={account} onSignOut={() => {}} />)
  expect(html.match(/<nav[^>]*aria-label="Applets"/g)).toHaveLength(1)
  expect(html.match(/<nav[^>]*aria-label="Builder menu"/g)).toHaveLength(1)
  // The rail lists every applet once, in the model's order, and names the count of one that has it.
  const rail = landmark(html, "Applets")
  expect([...rail.matchAll(/aria-label="([^"]+)"/g)].map(([, name]) => name).filter((name) => name !== "Applets")).toEqual(["Home", "Inbox, 3 open", "Data", "Builder"])
  expect(rail.match(/aria-current="page"/g)).toHaveLength(1)
  expect(rail).toMatch(/aria-current="page"[^>]*data-applet="builder"|data-applet="builder"[^>]*aria-current="page"/)
  // The applet opens on its first destination with a page; one with no page opens its first item.
  expect(rail).toContain('href="/sources"')
  expect(rail).toContain('href="/objects/canon.Holding"')
  // The menu lists the applet's destinations under their heading, the items under their own heading.
  const menu = landmark(html, "Builder menu")
  expect(menu).toMatch(/>Model<\/span>/)
  expect(menu).toContain("Object types")
  expect(menu).toMatch(/role="treeitem"[^>]*aria-current="page"[^>]*>\s*<a[^>]*href="\/sources\/canon.Holding"/)
  expect(menu.match(/aria-current="page"/g)).toHaveLength(1)
  // Other applets' destinations are not in this menu.
  expect(menu).not.toContain(">Records<")
  // A breadcrumb back to a parent page is not the current page either; rows carry no kind colour.
  expect(html).not.toMatch(/data-slot="breadcrumb-link"[^>]*aria-current/)
  expect(html).not.toContain("[&amp;&gt;svg]:text-kind")
  // The toggle is named "Sidebar" in both states and controls the submenu island it names.
  expect(html).toMatch(/<button[^>]*aria-label="Sidebar"[^>]*aria-expanded="true"[^>]*aria-controls="studio-sidebar"/)
  expect(html).toMatch(/id="studio-sidebar"[^>]*data-island="nav"/)
  expect(html).toMatch(/data-slot="sidebar-inset"[^>]*data-island="main"/)
  // The rail replaced the perspective switch.
  expect(html).not.toContain('aria-label="Perspective"')
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

test("the main island's bar holds search, shortcuts and notifications; the rail holds the workspace and the viewer, and the submenu names the workspace's state", async () => {
  const html = await at("/sources", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
  expect(html).toContain("Search…")
  expect(html).not.toContain('aria-label="Business or technical view"')
  // The phone's view-and-language menu is not drawn on a desktop, so it never shadows the account menu's choices.
  expect(html).not.toContain("Language and view")
  expect(html).toContain("Keyboard shortcuts")
  expect(html).toContain("Notifications")
  // Before the mode is decided the island says the workspace is connecting; the recording is named only once Studio knows there is no cell.
  expect(html).toMatch(/data-workspace-state[^>]*>[^<]*Connecting…/)
  expect(html).not.toContain("Recorded · synthetic gold-scrap data")
  expect(html).toContain('aria-label="Account"')
  expect(html).toMatch(/data-slot="sidebar-rail"[\s\S]*aria-label="Workspace: /)
})

test("only the recorded demo world opens; the NC benchmark points to its offline results; the rest say why they are empty", () => {
  expect(new Set(WORKSPACES.map(({ group }) => group))).toEqual(new Set(["Class demos", "Industry benchmarks", "First-party"]))
  expect(WORKSPACES.filter(({ recorded }) => recorded).map(({ id, group }) => `${group}/${id}`)).toEqual(["First-party/recorded-demo"])
  const olist = WORKSPACES.find(({ id }) => id === "olist")
  expect(olist?.detail).toBe("Benchmark run offline (licence NC)")
  expect(olist?.results).toBe("https://github.com/Norn-cloud/norn/issues/531")
  expect(WORKSPACES.find(({ group }) => group === "Class demos")?.detail).toBe("Coming soon")
})

test("a long list shows its first rows, the current row when it falls past them, and a Show all row; a filter shows every match", async () => {
  const many: Section = { ...sources, useMenu: () => Array.from({ length: 15 }, (_, index) => ({ href: `/sources/t${index}`, label: `Type ${index}`, group: "Object types" })) }
  const html = await at("/sources/t13", () => <Shell sections={[home, many, questions]} account={account} onSignOut={() => {}} />)
  const menu = landmark(html, "Builder menu")
  expect(menu).toMatch(/>Object types<\/span><span[^>]*>15<\/span>/)
  for (let index = 0; index < SHOW_FIRST; index += 1) expect(menu).toContain(`>Type ${index}<`)
  expect(menu).not.toContain(">Type 8<")
  // The current page's row is never hidden by the cap.
  expect(menu).toContain(">Type 13<")
  expect(menu).toMatch(/data-more="builder:\/sources:Object types"[\s\S]*?Show all 15/)
  // The pure rule behind it.
  const items = Array.from({ length: 15 }, (_, index) => ({ href: `/t${index}` }))
  expect(visibleItems(items, false, null).shown).toHaveLength(SHOW_FIRST)
  expect(visibleItems(items, false, "/t12").shown.map(({ href }) => href)).toContain("/t12")
  expect(visibleItems(items, true, null).shown).toHaveLength(15)
  // One past the cap is not worth a "Show all" row.
  expect(visibleItems(items.slice(0, SHOW_FIRST + 1), false, null).hidden).toBe(0)
  // The filter folds case and accents; an empty filter matches everything; a non-text label never matches.
  expect(matches("Énergie Holding", "energie")).toBe(true)
  expect(matches("Holding", "")).toBe(true)
  expect(matches(<span>Holding</span>, "hold")).toBe(false)
  // Group headers are quiet: step 1, weight 400, sentence case (no uppercase or tracking).
  const header = /<span[^>]*data-slot="tree-label"[^>]*class="([^"]*)"[^>]*>\s*<span[^>]*>Model<\/span>/.exec(html)?.[1] ?? /class="([^"]*group\/heading[^"]*)"/.exec(html)?.[1]
  expect(header).toBeDefined()
  expect(header).toContain("text-meta")
  expect(header).not.toMatch(/uppercase|font-semibold|tracking-/)
})

test("the applet a page opens in: by the item that claims it, else its destination; a page two applets list stays where the reader came from", () => {
  const all = [home, inbox, records, views, sources, questions]
  const menus = new Map<string, readonly MenuEntry[]>([["/objects", records.useMenu?.() ?? []], ["/sources", sources.useMenu?.() ?? []], ["/views", views.useMenu?.() ?? []]])
  expect(appletsOf(all).map(({ id }) => id)).toEqual(["/", "/decisions", "data", "builder"])
  expect(appletOf(all, "/", menus)?.id).toBe("/")
  expect(appletOf(all, "/sources", menus)?.id).toBe("builder")
  expect(appletOf(all, "/objects/canon.Holding", menus)?.id).toBe("data")
  // A type's own page is the model's item; its records only also cover it.
  expect(appletOf(all, "/sources/canon.Holding", menus)?.id).toBe("builder")
  // A type's record page is listed by the records (Data) and by the model (Builder): with no history the
  // longest claim wins, then the model's order; with history the reader's applet holds.
  expect(appletOf(all, "/sources/canon.Holding/demo%2Fitem", menus)?.id).toBe("data")
  expect(appletOf(all, "/sources/canon.Holding/demo%2Fitem", menus, "builder")?.id).toBe("builder")
  expect(appletOf(all, "/sources/canon.Holding/demo%2Fitem", menus, "data")?.id).toBe("data")
  // A previous applet that does not list the page does not hold it.
  expect(appletOf(all, "/sources", menus, "data")?.id).toBe("builder")
  expect(appletOf(all, "/nowhere", menus)).toBeUndefined()
  // A pinned view goes to the applet its group names (under "Pinned views"), else under a heading of that name in its own applet.
  expect(placeOf(views, { href: "/views/b", label: "Stuck", group: "Builder" }, appletsOf(all))).toEqual({ applet: "builder", heading: "Pinned views" })
  expect(placeOf(views, { href: "/views/c", label: "Night shift", group: "Floor" }, appletsOf(all))).toEqual({ applet: "data", heading: "Floor" })
  expect(placeOf(views, { href: "/views/a", label: "Open items" }, appletsOf(all))).toEqual({ applet: "data" })
  expect(appletOf(all, "/views/b", menus)?.id).toBe("builder")
  const builder = appletMenu(BUILDER, all, menus, new Map())
  expect(builder.headings.map(({ heading }) => heading)).toEqual(["Model", "Pinned views"])
  expect(currentIn(builder, "/views/b")).toBe("/views/b")
  const data = appletMenu(DATA, all, menus, new Map())
  expect(data.headings.map(({ heading }) => heading)).toEqual(["", "Floor"])
  expect(data.headings[0]!.destinations.find(({ section }) => section === views)?.entries.map(({ href }) => href)).toEqual(["/views/a"])
  // Where an applet opens.
  expect(appletHome(DATA, all, menus)).toBe("/objects/canon.Holding")
  expect(appletHome(DATA, all, new Map())).toBe("/views")
  expect(appletHome(BUILDER, all, menus)).toBe("/sources")
})

test("in the records a record page reads Records, its type, then its id", async () => {
  const html = await at("/sources/canon.Holding/demo%2Fitem%2Fgold", () => <Shell sections={[home, records, sources]} account={account} onSignOut={() => {}} />)
  const crumbs = html.match(/<nav aria-label="breadcrumb"[\s\S]*?<\/nav>/)?.[0] ?? ""
  expect(crumbs).toContain("Records")
  expect(crumbs).not.toContain('href="/objects"')
  expect(crumbs).toContain('href="/objects/canon.Holding"')
  expect(crumbs).toContain("demo/item/gold")
})

test("a destination's count is a quiet step-1 number, never a filled badge; an applet's count is its destinations'", async () => {
  const counted: Section = { ...questions, useBadge: () => 3 }
  const html = await at("/sources", () => <Shell sections={[sources, counted]} account={account} onSignOut={() => {}} />)
  const count = html.match(/<span[^>]*data-count[^>]*>3<\/span>/)?.[0] ?? ""
  expect(count).toContain("text-meta")
  expect(count).not.toMatch(/bg-|font-semibold|rounded-full/)
  expect(landmark(html, "Applets")).toContain('aria-label="Builder, 3 open"')
  expect(appletBadge([undefined, "2", "3"])).toBe("5")
  expect(appletBadge(["500+", "3"])).toBe("500+")
  expect(appletBadge([undefined])).toBeUndefined()
})

test("a destination with no page whose items were read and are none says so; one not read yet claims nothing", async () => {
  const empty: Section = { ...records, useMenu: () => [] }
  const read = await at("/views", () => <Shell sections={[empty, views]} account={account} onSignOut={() => {}} />)
  expect(landmark(read, "Data menu")).toMatch(/data-empty[^>]*>None yet</)
  const unread: Section = { ...records, useMenu: () => undefined }
  const pending = await at("/views", () => <Shell sections={[unread, views]} account={account} onSignOut={() => {}} />)
  expect(landmark(pending, "Data menu")).not.toContain("None yet")
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

test("an assistant mounts in the main island's panel, opened from the rail; a page can hand it a prompt", async () => {
  const html = await at("/sources", () => <Shell sections={[sources]} assistant={{ title: "Assistant", content: <p>agent here</p> }} account={account} onSignOut={() => {}} />)
  expect(html).toMatch(/<button[^>]*aria-label="Assistant"[^>]*aria-pressed="false"/)
  expect(html).toMatch(/data-slot="sidebar-rail"[\s\S]*aria-label="Assistant"/)
  expect(html).toMatch(/data-slot="assistant-panel"[^>]*hidden/)
  expect(html).toContain("agent here")
  const none = await at("/sources", () => <Shell sections={[sources]} account={account} onSignOut={() => {}} />)
  expect(none).not.toContain('data-slot="assistant-panel"')
  expect(none).not.toContain('aria-label="Assistant"')
})

test("collapsing one heading records only that branch, leaving untouched lists free to follow their defaults", () => {
  const before = ["space:Model", "space:Logic", "dest:/sources"]
  const after = ["space:Model", "dest:/sources"]
  const chosen = diffExpanded(before, after, {})
  expect(chosen).toEqual({ "space:Logic": false })
  expect(chosen["dest:/rules"]).toBeUndefined()
  const next = diffExpanded(after, [...after, "dest:/rules"], chosen)
  expect(next).toEqual({ "space:Logic": false, "dest:/rules": true })
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
  // The state is visible text under the submenu island's title, never only the rail disc's tooltip.
  expect(html).toMatch(/data-workspace-state[^>]*>Analytical · Live · Owner</)
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
