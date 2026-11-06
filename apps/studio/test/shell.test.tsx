import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { SidebarProvider } from "@/components/ui/sidebar"
import type { Account } from "../src/lib/session.ts"
import { Shell, Submenu, initials, sectionOf, target, type Section } from "../src/shell.tsx"

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

test("the rail and submenu sit in one row that does not depend on the outer sidebar's classes", async () => {
  const html = await at("/sources", () => <Shell sections={[sources]} account={account} onSignOut={() => {}} />)
  expect(html).toContain("flex size-full min-h-0 flex-row")
  expect(html).toContain("w-[calc(4rem+1px)]!")
})

test("the rail shows the signed-in user's initials and never the recorded-demo placeholder", async () => {
  const html = await at("/sources", () => <Shell sections={[sources]} account={account} onSignOut={() => {}} />)
  expect(html).toContain(">AL<")
  expect(html).not.toContain("Recorded demo")
  expect(initials("")).toBe("?")
  expect(initials("ada@example.com")).toBe("A")
})
