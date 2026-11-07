import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { SidebarProvider } from "@/components/ui/sidebar"
import type { Account } from "../src/lib/session.ts"
import { Shell, Submenu, groupOpen, hiddenLabelTabIndex, initials, submenuInert, sectionOf, target, type Section } from "../src/shell.tsx"

const account: Account = { user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" }, organization: { id: "o1", name: "Analytical", role: "viewer" } }

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
  const html = await at("/sources", () => <Shell sections={[{ ...sources, group: "model" }, { ...questions, group: "model" }, rules]} account={account} onSignOut={() => {}} />)
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
  const html = await at("/sources", () => <Shell sections={[sources, counted]} account={account} onSignOut={() => {}} />)
  const badge = html.match(/<span class="[^"]*bg-destructive[^"]*"[^>]*>3<\/span>/)?.[0] ?? ""
  expect(badge).toContain("text-destructive-foreground")
  expect(badge).toContain("top-0.5 right-0.5")
  expect(badge).not.toContain("text-white")
})

test("a search shows matches in a group the user closed", () => {
  const closed = new Set(["Object types"])
  expect(groupOpen(closed, "Object types", "")).toBe(false)
  expect(groupOpen(closed, "Object types", "hold")).toBe(true)
  expect(groupOpen(closed, "Metrics", "")).toBe(true)
})

test("the page bar keeps a sidebar trigger on every screen and a search label that fits a narrow bar", async () => {
  const html = await at("/sources", () => <Shell sections={[sources, questions]} account={account} onSignOut={() => {}} />)
  const trigger = html.match(/<button[^>]*data-sidebar="trigger"[^>]*>/)?.[0] ?? ""
  expect(trigger).not.toBe("")
  expect(trigger).not.toContain("md:hidden")
  expect(html).toMatch(/<span class="min-w-0 truncate">Search or add…<\/span>/)
  expect(html).toMatch(/<kbd class="[^"]*hidden[^"]*sm:inline[^"]*">⌘K<\/kbd>/)
})

test("the rail shows the signed-in user's initials and never the recorded-demo placeholder", async () => {
  const html = await at("/sources", () => <Shell sections={[sources]} account={account} onSignOut={() => {}} />)
  expect(html).toContain(">AL<")
  expect(html).not.toContain("Recorded demo")
  expect(html).not.toContain("Not signed in")
  expect(initials("")).toBe("?")
  expect(initials("ada@example.com")).toBe("A")
})

test("a group label's button leaves the tab order only while the desktop sidebar is collapsed", () => {
  expect(hiddenLabelTabIndex("collapsed", false)).toBe(-1)
  expect(hiddenLabelTabIndex("expanded", false)).toBeUndefined()
  expect(hiddenLabelTabIndex("collapsed", true)).toBeUndefined()
})

test("the collapsed desktop submenu is inert, so its links and Show more take no focus; open, it is live", async () => {
  const many: Section = { ...sources, useMenu: () => Array.from({ length: 9 }, (_, index) => ({ href: `/sources/t${index}`, label: `Type ${index}`, group: "Object types" })) }
  const collapsed = await at("/sources", () => <SidebarProvider defaultOpen={false}><Submenu section={many} /></SidebarProvider>)
  expect(collapsed).toMatch(/<div[^>]*data-slot="sidebar"[^>]*inert=""/)
  expect(collapsed).toContain("Show more")
  const open = await at("/sources", () => <SidebarProvider><Submenu section={many} /></SidebarProvider>)
  expect(open).not.toContain('inert=""')
  expect(submenuInert("collapsed", false)).toBe(true)
  expect(submenuInert("collapsed", true)).toBe(false)
  expect(submenuInert("expanded", false)).toBe(false)
})
