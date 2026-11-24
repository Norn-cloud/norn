import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { Database, House } from "lucide-react"
import { renderToStaticMarkup } from "react-dom/server"
import { ChromeDock, ChromeDockOption, ChromeDockTray, ChromeDockTrigger } from "../src/components/ui/chrome-dock.tsx"
import { AppearanceProvider, SHELLS, SKINS, applyAppearance, isShellChrome, isSkin, storedShellChrome, storedSkin } from "../src/lib/appearance.tsx"
import type { Account } from "../src/lib/session.ts"
import { Shell, type Section } from "../src/shell.tsx"

const account: Account = {
  user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" },
  organization: { id: "o1", name: "Analytical", role: "viewer" },
}

const sections: readonly Section[] = [
  { path: "/", label: "Home", icon: House, perspective: "global" },
  { path: "/sources", label: "Sources", icon: Database, perspective: "builder", group: "Model" },
]

test("appearance validators and defaults cover all four skins and both shell layouts", () => {
  expect(SKINS.map(({ id }) => id)).toEqual(["open", "polar", "luma", "glass"])
  expect(SHELLS.map(({ id }) => id)).toEqual(["sidebar", "dock"])
  expect(isSkin("open")).toBe(true)
  expect(isSkin("polar")).toBe(true)
  expect(isSkin("luma")).toBe(true)
  expect(isSkin("glass")).toBe(true)
  expect(isSkin("unknown")).toBe(false)
  expect(isShellChrome("sidebar")).toBe(true)
  expect(isShellChrome("dock")).toBe(true)
  expect(isShellChrome("other")).toBe(false)
})

test("applyAppearance writes data-skin and data-shell onto the root element", () => {
  const root = { dataset: {} as Record<string, string> } as unknown as HTMLElement
  applyAppearance("polar", "dock", root)
  expect(root.dataset.skin).toBe("polar")
  expect(root.dataset.shell).toBe("dock")
})

test("blocked localStorage falls back cleanly to open skin and sidebar chrome", () => {
  const saved = Object.getOwnPropertyDescriptor(globalThis, "localStorage")
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    get: () => { throw new DOMException("blocked", "SecurityError") },
  })
  try {
    expect(storedSkin()).toBe("open")
    expect(storedShellChrome()).toBe("sidebar")
    expect(renderToStaticMarkup(<AppearanceProvider><span>ok</span></AppearanceProvider>)).toBe("<span>ok</span>")
  } finally {
    if (saved) Object.defineProperty(globalThis, "localStorage", saved)
    else delete (globalThis as { localStorage?: unknown }).localStorage
  }
})

test("ChromeDock primitive renders semantic data-slot and data-state attributes via @ui-sdk/core + @ui-sdk/react", () => {
  const html = renderToStaticMarkup(
    <ChromeDock defaultOpen value="polar" items={["open", "polar", "luma", "glass"]}>
      <ChromeDockTrigger aria-label="Appearance">Theme</ChromeDockTrigger>
      <ChromeDockTray>
        <ChromeDockOption value="open">Open</ChromeDockOption>
        <ChromeDockOption value="polar">Polar</ChromeDockOption>
      </ChromeDockTray>
    </ChromeDock>,
  )
  expect(html).toContain('data-slot="chrome-dock"')
  expect(html).toContain('data-slot="chrome-dock-trigger"')
  expect(html).toContain('data-slot="chrome-dock-tray"')
  expect(html).toContain('data-slot="chrome-dock-option"')
  expect(html).toContain('data-state="open"')
  expect(html).toContain('data-state="selected"')
  expect(html).toContain('data-state="idle"')
  expect(html).toContain('aria-pressed="true"')
  expect(html).toContain('aria-pressed="false"')
})

test("Shell mounts the Appearance ChromeDock trigger with default open skin and sidebar chrome", async () => {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <Shell sections={sections} account={account} onSignOut={() => {}} /> }),
    history: createMemoryHistory({ initialEntries: ["/sources"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  expect(html).toContain('data-skin="open"')
  expect(html).toContain('data-shell="sidebar"')
  expect(html).toContain('aria-label="Appearance"')
  expect(html).toContain('data-slot="chrome-dock-trigger"')
})
