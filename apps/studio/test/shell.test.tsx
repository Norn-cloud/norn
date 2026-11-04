import { expect, test } from "bun:test"
import { Database } from "lucide-react"
import { renderToStaticMarkup } from "react-dom/server"
import { SidebarProvider } from "@/components/ui/sidebar"
import { Submenu, type Section } from "../src/shell.tsx"

const section: Section = {
  path: "/sources", label: "Sources", icon: Database, summary: "",
  useMenu: () => [{ href: "/sources/canon.Holding", label: "Holding", group: "Object types" }],
}

test("a submenu shows its title, data context and entries, and no action the server has not declared", () => {
  const html = renderToStaticMarkup(<SidebarProvider><Submenu section={section} /></SidebarProvider>)
  expect(html).toContain("Sources")
  expect(html).toContain("Recorded · synthetic world")
  expect(html).toContain("Holding")
  expect(html).not.toContain('data-slot="button"')
})
