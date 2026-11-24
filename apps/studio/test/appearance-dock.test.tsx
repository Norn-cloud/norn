import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { Blend, Database, Fingerprint, House, Inbox, Network, Plug, ShieldCheck } from "lucide-react"
import { renderToStaticMarkup } from "react-dom/server"
import { ChromeDock, ChromeDockOption, ChromeDockTray, ChromeDockTrigger, chromeDockPrimitive } from "../src/components/ui/chrome-dock.tsx"
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "../src/components/ui/command.tsx"
import { ToggleGroup, ToggleGroupItem, createArrowSelectGuard, isRadioArrowKey } from "../src/components/ui/toggle-group.tsx"
import { AppearanceProvider, SHELLS, SKINS, applyAppearance, isShellChrome, isSkin, storedShellChrome, storedSkin } from "../src/lib/appearance.tsx"
import type { Account } from "../src/lib/session.ts"
import { Shell, type Section } from "../src/shell.tsx"

const account: Account = {
  user: { id: "u1", name: "Ada Lovelace", email: "ada@example.com" },
  organization: { id: "o1", name: "Analytical", role: "viewer" },
}

const sections: readonly Section[] = [
  { path: "/", label: "Home", icon: House, perspective: "global" },
  { path: "/decisions", label: "Inbox", icon: Inbox, perspective: "global", useBadge: () => 4 },
  { path: "/sources", label: "Sources", icon: Database, perspective: "builder", group: "Model" },
  { path: "/transforms", label: "Transforms", icon: Blend, perspective: "builder", group: "Model" },
  { path: "/canvas", label: "Canvas", icon: Network, perspective: "builder", group: "Logic" },
  { path: "/rules", label: "Rules", icon: ShieldCheck, perspective: "builder", group: "Logic" },
  { path: "/markings", label: "Markings", icon: Fingerprint, perspective: "builder", group: "Logic" },
  { path: "/connections", label: "Connections", icon: Plug, perspective: "builder", group: "Settings" },
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

test("chromeDockPrimitive fires onValueChange exactly once with a string and honors details.cancel()", () => {
  const calls: Array<{ value: unknown; hasCancel: boolean }> = []
  let shouldCancel = false
  const instance = chromeDockPrimitive({
    id: "test-dock",
    defaultValue: "open",
    items: ["open", "polar", "luma", "glass"],
    onValueChange(value, details) {
      calls.push({ value, hasCancel: typeof details?.cancel === "function" })
      if (shouldCancel) details.cancel()
    },
  })
  try {
    instance.api.select("polar")
    expect(calls).toEqual([{ value: "polar", hasCancel: true }])
    expect(instance.api.isSelected("polar")).toBe(true)
    expect(instance.api.isSelected("open")).toBe(false)

    // Re-selecting the already active value does not fire a redundant callback.
    instance.api.select("polar")
    expect(calls).toHaveLength(1)

    // Canceling inside onValueChange vetoes the selection transition.
    shouldCancel = true
    instance.api.select("glass")
    expect(calls).toHaveLength(2)
    expect(calls[1]).toEqual({ value: "glass", hasCancel: true })
    expect(instance.api.isSelected("polar")).toBe(true)
    expect(instance.api.isSelected("glass")).toBe(false)
  } finally {
    instance.destroy()
  }
})

test("Shell mounts the Appearance ChromeDock trigger in sidebar mode without duplicating data-skin in body", async () => {
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
  expect(html).not.toContain('data-skin="')
  expect(html).toContain('data-shell="sidebar"')
  expect(html).toContain('aria-label="Appearance"')
  expect(html).toContain('data-slot="chrome-dock-trigger"')
})

test("dock mode mounts AccountMenu, explicit accessible names on every dock link, overflow menu for >5 sections, and desktop bottom clearance", async () => {
  const router = createRouter({
    routeTree: createRootRoute({
      component: () => <Shell sections={sections} account={account} onSignOut={() => {}} initialShell="dock" />,
    }),
    history: createMemoryHistory({ initialEntries: ["/sources"] }),
  })
  await router.load()
  const html = renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  expect(html).toContain('data-shell="dock"')
  expect(html).toContain('data-slot="dock-topbar"')
  expect(html).toContain('data-slot="floating-dock"')
  expect(html).toContain("pb-32")
  expect(html).toContain("md:pb-32")

  // AccountMenu stays reachable in dock mode via the minimal v6 topbar (showing initials and Account trigger).
  expect(html).toContain('aria-label="Account"')
  expect(html).toContain(">AL<")

  const dockMatch = /<nav[^>]*data-slot="floating-dock"[\s\S]*?<\/nav>/.exec(html)
  expect(dockMatch).toBeTruthy()
  const dockHtml = dockMatch![0]!

  // Every anchor link in the icon-only v6 dock carries an explicit aria-label.
  const links = [...dockHtml.matchAll(/<a\b([^>]*)>/g)]
  expect(links.length).toBeGreaterThanOrEqual(7)
  for (const [, attrs] of links) {
    const ariaLabel = /aria-label="([^"]+)"/.exec(attrs!)?.[1]
    expect(ariaLabel, `dock link missing aria-label: <a${attrs}>`).toBeTruthy()
  }
  expect(dockHtml).toContain('aria-label="Inbox, 4 open"')

  // The 6th builder section (Connections) is exposed via the "More destinations" overflow menu trigger.
  expect(dockHtml).toContain('aria-label="More destinations"')
})

test("ToggleGroup emits radiogroup/radio/aria-checked without aria-pressed in single mode, and group/aria-pressed in multiple mode", () => {
  const singleHtml = renderToStaticMarkup(
    <ToggleGroup type="single" value="a" aria-label="Mode">
      <ToggleGroupItem value="a">Alpha</ToggleGroupItem>
      <ToggleGroupItem value="b">Beta</ToggleGroupItem>
    </ToggleGroup>,
  )
  expect(singleHtml).toContain('role="radiogroup"')
  expect(singleHtml).toContain('role="radio"')
  expect(singleHtml).toContain('aria-checked="true"')
  expect(singleHtml).toContain('aria-checked="false"')
  expect(singleHtml).not.toContain("aria-pressed=")

  const multiHtml = renderToStaticMarkup(
    <ToggleGroup type="multiple" value={["a"]} aria-label="Filters">
      <ToggleGroupItem value="a">Alpha</ToggleGroupItem>
      <ToggleGroupItem value="b">Beta</ToggleGroupItem>
    </ToggleGroup>,
  )
  expect(multiHtml).toContain('role="group"')
  expect(multiHtml).toContain('aria-pressed="true"')
  expect(multiHtml).toContain('aria-pressed="false"')
  expect(multiHtml).not.toContain('role="radio"')
  expect(multiHtml).not.toContain("aria-checked=")
})

test("ToggleGroup arrow-select guard survives Base UI's bubble-phase focus microtask and disarms after a loopFocus={false} boundary no-op", async () => {
  expect(isRadioArrowKey("ArrowRight", "horizontal")).toBe(true)
  expect(isRadioArrowKey("ArrowLeft", "horizontal")).toBe(true)
  expect(isRadioArrowKey("ArrowUp", "horizontal")).toBe(false)
  expect(isRadioArrowKey("ArrowDown", "horizontal")).toBe(false)
  expect(isRadioArrowKey("ArrowRight", "horizontal", { shiftKey: true })).toBe(false)
  expect(isRadioArrowKey("ArrowUp", "vertical")).toBe(true)
  expect(isRadioArrowKey("ArrowDown", "vertical")).toBe(true)
  expect(isRadioArrowKey("ArrowLeft", "vertical")).toBe(false)

  // 1. Capture arms guard, then Base UI CompositeRoot schedules focus() via one queueMicrotask()
  //    during bubble phase: consume() must see true when that focus microtask fires.
  const guard = createArrowSelectGuard()
  guard.arm()
  const consumedDuringFocus = await new Promise<boolean>((resolve) => {
    queueMicrotask(() => resolve(guard.consume()))
  })
  expect(consumedDuringFocus).toBe(true)
  expect(guard.consume()).toBe(false)

  // 2. Same-axis no-op at a loopFocus={false} boundary: capture arms guard, Base UI does not
  //    move focus, and the nested microtask disarms before any subsequent non-arrow focus event.
  guard.arm()
  await new Promise((resolve) => setTimeout(resolve, 0))
  expect(guard.consume()).toBe(false)

  // 3. Rapid consecutive arrow keys: an earlier press's cleanup never cancels a newer press.
  guard.arm()
  const secondConsumed = await new Promise<boolean>((resolve) => {
    queueMicrotask(() => {
      guard.arm()
      queueMicrotask(() => resolve(guard.consume()))
    })
  })
  expect(secondConsumed).toBe(true)
  expect(guard.consume()).toBe(false)
})

test("first-party Command palette renders accessible combobox, listbox, group, and option roles without cmdk", () => {
  const html = renderToStaticMarkup(
    <Command label="Choose a record" shouldFilter={false}>
      <CommandInput placeholder="Search…" value="" onValueChange={() => {}} />
      <CommandList>
        <CommandEmpty>No results.</CommandEmpty>
        <CommandGroup heading="Records">
          <CommandItem value="r1">Record One</CommandItem>
          <CommandItem value="r2">Record Two</CommandItem>
        </CommandGroup>
      </CommandList>
    </Command>,
  )
  expect(html).toContain('role="combobox"')
  expect(html).toContain('role="listbox"')
  expect(html).toContain('role="group"')
  expect(html).toContain('role="option"')
  expect(html).toContain('data-slot="command-item"')
  expect(html).toContain("Record One")
})
