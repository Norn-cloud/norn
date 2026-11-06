import { expect, test } from "bun:test"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { Database } from "lucide-react"
import type { ReactNode } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { SidebarProvider } from "@/components/ui/sidebar"
import type { Account } from "../src/lib/session.ts"
import { Shell, Submenu, initials, sectionOf, target, type Section } from "../src/shell.tsx"

const account: Account = { user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" }, organization: { id: "o1", name: "Analytical", role: "viewer" } }

const section: Section = {
  path: "/sources", label: "Sources", icon: Database, summary: "",
  useMenu: () => [{ href: "/sources/canon.Holding", label: "Holding", group: "Object types" }, { href: "#concept-canon.Party", label: "Party" }],
}

/** Renders a node at a route, the way the app's router would. */
async function at(pathname: string, node: () => ReactNode) {
  const router = createRouter({ routeTree: createRootRoute({ component: node }), history: createMemoryHistory({ initialEntries: [pathname] }) })
  await router.load()
  return renderToStaticMarkup(<RouterProvider router={router} />)
}

test("a submenu shows its title, data context and entries, and no action the server has not declared", async () => {
  const html = await at("/sources", () => <SidebarProvider><Submenu section={section} /></SidebarProvider>)
  expect(html).toContain("Sources")
  expect(html).toContain("Recorded · synthetic world")
  expect(html).toContain("Holding")
  expect(html).not.toContain('data-slot="button"')
})

test("a record page under a section keeps that section current", () => {
  const questions: Section = { path: "/questions", label: "Questions", icon: Database, summary: "" }
  expect(sectionOf([section, questions], "/sources")).toBe(section)
  expect(sectionOf([section, questions], "/sources/canon.Item")).toBe(section)
  expect(sectionOf([section, questions], "/sources/canon.Item/demo/item/gold")).toBe(section)
  expect(sectionOf([section, questions], "/sourcesx")).toBeUndefined()
})

test("an anchor entry links to its section's page, so it navigates from a record page", async () => {
  expect(target(section, "#concept-canon.Party")).toEqual({ to: "/sources", hash: "concept-canon.Party" })
  expect(target(section, "/sources/canon.Holding")).toEqual({ to: "/sources/canon.Holding" })
  const html = await at("/sources/canon.Item/demo%2Fitem%2Fgold", () => <SidebarProvider><Submenu section={section} /></SidebarProvider>)
  expect(html).toContain('href="/sources#concept-canon.Party"')
})

test("the rail and submenu sit in one row that does not depend on the outer sidebar's classes", async () => {
  const html = await at("/sources", () => <Shell sections={[section]} account={account} onSignOut={() => {}} />)
  expect(html).toContain("flex size-full min-h-0 flex-row")
  expect(html).toContain("w-[calc(4rem+1px)]!")
})

test("the rail shows the signed-in user's initials and never the recorded-demo placeholder", async () => {
  const html = await at("/sources", () => <Shell sections={[section]} account={account} onSignOut={() => {}} />)
  expect(html).toContain(">AL<")
  expect(html).not.toContain("Recorded demo")
  expect(initials("")).toBe("?")
  expect(initials("ada@example.com")).toBe("A")
})
