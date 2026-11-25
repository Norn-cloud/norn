import { expect, test } from "bun:test"
import { applyAppearance } from "../src/lib/appearance.tsx"
import {
  STUDIO_THEME_PRESETS,
  applyCustomTheme,
  effectiveThemePalette,
  exportStudioThemeJson,
  parseStudioTheme,
  resolveCustomThemeTokens,
  validateStudioThemeContrast,
} from "../src/lib/studio-theme.ts"

function fakeRoot(initialDark = false) {
  const styles = new Map<string, string>()
  const dataset: Record<string, string> = {}
  const classes = new Set<string>(initialDark ? ["dark"] : [])
  return {
    styles,
    dataset,
    classes,
    element: {
      dataset,
      classList: {
        contains: (name: string) => classes.has(name),
        toggle: (name: string, force?: boolean) => {
          const next = force ?? !classes.has(name)
          if (next) classes.add(name)
          else classes.delete(name)
          return next
        },
      },
      style: {
        setProperty: (k: string, v: string) => { styles.set(k, v) },
        removeProperty: (k: string) => { styles.delete(k) },
      },
    } as unknown as HTMLElement,
  }
}

test("all curated StudioTheme JSON presets parse cleanly and pass WCAG AA in both light and dark schemes", () => {
  expect(STUDIO_THEME_PRESETS.length).toBeGreaterThanOrEqual(3)
  for (const preset of STUDIO_THEME_PRESETS) {
    const parsed = parseStudioTheme(JSON.stringify(preset))
    expect(parsed.ok, `${preset.name} should parse cleanly: ${JSON.stringify(parsed.issues)}`).toBe(true)
    expect(validateStudioThemeContrast(preset)).toEqual([])
  }
})

test("parseStudioTheme rejects unknown tokens, CSS injection values, and malformed JSON", () => {
  expect(parseStudioTheme("{not json").ok).toBe(false)

  const unknown = parseStudioTheme({
    version: 1,
    name: "Bad Token",
    colors: { "z-index": "#ffffff" },
  })
  expect(unknown.ok).toBe(false)
  if (!unknown.ok) expect(unknown.issues.some((i) => i.path === "colors.z-index")).toBe(true)

  const injection = parseStudioTheme({
    version: 1,
    name: "CSS Injection",
    colors: { background: "url(https://evil.example/x); position: fixed" },
  })
  expect(injection.ok).toBe(false)
  if (!injection.ok) expect(injection.issues.some((i) => i.path === "colors.background")).toBe(true)
})

test("parseStudioTheme enforces WCAG AA contrast across text, hover buttons, popovers, dock, and translucent shells", () => {
  const lowContrast = parseStudioTheme({
    version: 1,
    name: "Low Contrast",
    baseSkin: "polar",
    variants: { light: { background: "#ffffff", foreground: "#d4d4d8" } },
  })
  expect(lowContrast.ok).toBe(false)
  if (!lowContrast.ok) expect(lowContrast.issues.some((i) => i.path.includes("contrast.light.foreground"))).toBe(true)

  // Button[default] renders primary-foreground on action-hover: 1:1 hover text must be rejected.
  const badHover = parseStudioTheme({
    version: 1,
    name: "White Hover",
    baseSkin: "polar",
    appearance: "light",
    colors: { "action-hover": "#fafafa" },
  })
  expect(badHover.ok).toBe(false)
  if (!badHover.ok) expect(badHover.issues.some((i) => i.path.includes("action-hover"))).toBe(true)

  // Popover and floating dock pairs are validated too.
  const badPopover = parseStudioTheme({
    version: 1,
    name: "Invisible Popover",
    baseSkin: "polar",
    appearance: "dark",
    colors: { popover: "#1c1c20", "popover-foreground": "#2a2a30" },
  })
  expect(badPopover.ok).toBe(false)
  if (!badPopover.ok) expect(badPopover.issues.some((i) => i.path.includes("popover-foreground"))).toBe(true)

  const badDock = parseStudioTheme({
    version: 1,
    name: "Invisible Dock",
    baseSkin: "polar",
    appearance: "dark",
    colors: { "dock-surface": "#222226", "dock-foreground": "#2e2e34" },
  })
  expect(badDock.ok).toBe(false)
  if (!badDock.ok) expect(badDock.issues.some((i) => i.path.includes("dock-foreground"))).toBe(true)

  // Translucent dark shell on a light OS backdrop composites against white, exposing low-contrast white text.
  const translucentShell = parseStudioTheme({
    version: 1,
    name: "Translucent Light Shell",
    baseSkin: "polar",
    appearance: "light",
    colors: { shell: "rgb(0 0 0 / 0.1)", background: "rgb(0 0 0 / 0.05)", foreground: "#fafafa" },
  })
  expect(translucentShell.ok).toBe(false)

  // Muted surface pair: muted matching muted-foreground produces 1:1 text on muted badges and must be rejected.
  const badMuted = parseStudioTheme({
    version: 1,
    name: "Same Muted",
    baseSkin: "polar",
    appearance: "light",
    colors: { muted: "#585861" },
  })
  expect(badMuted.ok).toBe(false)
  if (!badMuted.ok) expect(badMuted.issues.some((i) => i.path.includes("muted"))).toBe(true)

  // Forced dark theme with white canvas and transparent shell/background composites over body's bg-canvas (#fff).
  const whiteCanvasDark = parseStudioTheme({
    version: 1,
    name: "White Canvas Dark",
    baseSkin: "polar",
    appearance: "dark",
    colors: { canvas: "#ffffff", shell: "rgb(0 0 0 / 0)", background: "rgb(0 0 0 / 0)" },
  })
  expect(whiteCanvasDark.ok).toBe(false)
  if (!whiteCanvasDark.ok) expect(whiteCanvasDark.issues.some((i) => i.path.includes("foreground"))).toBe(true)

  // Forced dark theme with white popover & dark popover-foreground still leaves foreground/muted-foreground unreadable inside popovers/dialogs.
  const whitePopoverDark = parseStudioTheme({
    version: 1,
    name: "White Popover Dark",
    baseSkin: "polar",
    appearance: "dark",
    colors: { popover: "#ffffff", "popover-foreground": "#111113" },
  })
  expect(whitePopoverDark.ok).toBe(false)
  if (!whitePopoverDark.ok) expect(whitePopoverDark.issues.some((i) => i.path.includes("on popover"))).toBe(true)

  // Forced dark theme with mid-grey popover and dim action/link fails action/link on popover AND destructive/10 alert contrast.
  const midGreyPopoverDark = parseStudioTheme({
    version: 1,
    name: "Mid Grey Popover Dark",
    baseSkin: "polar",
    appearance: "dark",
    colors: { popover: "#343434", action: "#858585", link: "#858585" },
  })
  expect(midGreyPopoverDark.ok).toBe(false)
  if (!midGreyPopoverDark.ok) {
    expect(midGreyPopoverDark.issues.some((i) => i.path.includes("action (on popover)"))).toBe(true)
    expect(midGreyPopoverDark.issues.some((i) => i.path.includes("link (on popover)"))).toBe(true)
    expect(midGreyPopoverDark.issues.some((i) => i.path.includes("destructive/10 over popover"))).toBe(true)
  }
})

test("forced appearance and omitted baseSkin use the exact same scheme and skin for validation and rendering", () => {
  const forcedDark = parseStudioTheme({
    version: 1,
    name: "Forced Dark",
    appearance: "dark",
    colors: { foreground: "#fafafa" },
  })
  expect(forcedDark.ok).toBe(true)
  if (!forcedDark.ok) return

  // Even when the user was on `glass` in a light OS, applying a forced dark theme without baseSkin
  // pins `data-skin="polar"` and enables `.dark` so rendering matches the validated palette.
  const root = fakeRoot(false)
  applyAppearance("glass", "sidebar", root.element, forcedDark.theme)
  expect(root.dataset.skin).toBe("polar")
  expect(root.classes.has("dark")).toBe(true)
  expect(root.dataset.forcedAppearance).toBe("dark")
  expect(root.styles.get("--foreground")).toBe("#fafafa")

  // Clearing the custom theme restores the OS preference and removes forcedAppearance + inline properties.
  applyAppearance("glass", "sidebar", root.element, null)
  expect(root.dataset.skin).toBe("glass")
  expect(root.classes.has("dark")).toBe(false)
  expect(root.dataset.forcedAppearance).toBeUndefined()
  expect(root.styles.size).toBe(0)
})

test("shorthand accent and canvas expand with auto-contrasting foreground ink and matching action-hover", () => {
  const result = parseStudioTheme({
    version: 1,
    name: "Ink Accent",
    baseSkin: "polar",
    appearance: "light",
    canvas: "#f5f5f5",
    accent: "#1d4ed8",
  })
  expect(result.ok).toBe(true)
  if (!result.ok) return
  const tokens = resolveCustomThemeTokens(result.theme, "light")
  expect(tokens.canvas).toBe("#f5f5f5")
  expect(tokens.shell).toBe("#f5f5f5")
  expect(tokens.primary).toBe("#1d4ed8")
  expect(tokens["primary-foreground"]).toBe("#fafafa")
  expect(tokens.action).toBe("#1d4ed8")
  expect(tokens["action-hover"]).toBe("#1d4ed8")
  expect(tokens.ring).toBe("#1d4ed8")
})

test("applyCustomTheme writes variant tokens for light/dark and removes every inline property cleanly on reset", () => {
  const preset = STUDIO_THEME_PRESETS[1]!
  const lightRoot = fakeRoot(false)
  applyCustomTheme(preset, false, lightRoot.element)
  expect(lightRoot.dataset.customTheme).toBe(preset.name)
  expect(lightRoot.styles.get("--background")).toBe("#fcfcfd")
  expect(lightRoot.styles.get("--chart-1")).toBe("#111318")

  applyCustomTheme(preset, true, lightRoot.element)
  expect(lightRoot.styles.get("--background")).toBe("#0f1117")
  expect(lightRoot.styles.get("--chart-1")).toBe("#3ccf8e")

  applyCustomTheme(null, true, lightRoot.element)
  expect(lightRoot.styles.size).toBe(0)
  expect(lightRoot.dataset.customTheme).toBeUndefined()
  expect(lightRoot.dataset.customThemeTokens).toBeUndefined()
})

test("exportStudioThemeJson produces valid WCAG-AA StudioTheme JSON for every built-in skin", () => {
  for (const skin of ["polar", "open", "luma", "glass"] as const) {
    const raw = exportStudioThemeJson(skin, null)
    const parsed = parseStudioTheme(raw)
    expect(parsed.ok, `${skin} export should validate: ${JSON.stringify(parsed.issues)}`).toBe(true)
  }
})

// #1705: every built-in skin, unchanged, passes the gate it applies to custom themes.
test("every built-in skin passes the rendered-pair contrast gate in light and dark", () => {
  for (const baseSkin of ["polar", "open", "luma", "glass"] as const) {
    expect(validateStudioThemeContrast({ version: 1, name: baseSkin, baseSkin }), baseSkin).toEqual([])
  }
})

// #1705: the gate reads the colours index.css paints, not a hand-kept copy.
test("the gate validates the rendered palette: dark Luma hover is index.css's #695ff0", () => {
  expect(effectiveThemePalette({ version: 1, name: "Luma", baseSkin: "luma" }, "dark")["action-hover"]).toBe("#695ff0")
  expect(effectiveThemePalette({ version: 1, name: "Polar", baseSkin: "polar" }, "dark")["dock-ring"]).toBe("#e4e4e7")
})

// #1705: pairs the old gate missed.
test("the gate catches white initials on a white dock-active and foregrounds lost on surface-subtle", () => {
  const dock = parseStudioTheme({ version: 1, name: "White dock", colors: { "dock-active": "#ffffff" } })
  expect(dock.ok).toBe(false)
  if (!dock.ok) expect(dock.issues.map((i) => i.path)).toContain("contrast.light.dock-foreground (on dock-active)")
  const subtle = parseStudioTheme({ version: 1, name: "Dim subtle", appearance: "light", colors: { "surface-subtle": "#6b6b73" } })
  expect(subtle.ok).toBe(false)
  if (!subtle.ok) {
    const paths = subtle.issues.map((i) => i.path)
    expect(paths).toContain("contrast.light.foreground (on surface-subtle)")
    expect(paths).toContain("contrast.light.muted-foreground (on surface-subtle)")
    expect(paths).toContain("contrast.light.secondary-foreground (on surface-subtle)")
  }
})

// #1705: a malformed colour is refused; a valid one is stored and applied as canonical CSS.
test("colours are parsed by culori and written as canonical CSS; mixed-syntax rgb() is refused", () => {
  const mixed = parseStudioTheme({ version: 1, name: "Mixed", colors: { foreground: "rgb(24, 24 27)" } })
  expect(mixed.ok).toBe(false)
  if (!mixed.ok) expect(mixed.issues.map((i) => i.path)).toEqual(["colors.foreground"])

  const modern = parseStudioTheme({ version: 1, name: "Modern", baseSkin: "polar", appearance: "light", colors: { foreground: " rgb(24 24 27) ", "surface-subtle": "oklch(0.97 0 0)", "dock-border": "rgb(255 255 255 / 8%)" } })
  expect(modern.ok, JSON.stringify(modern.issues)).toBe(true)
  if (!modern.ok) return
  expect(modern.theme.colors).toEqual({ foreground: "#18181b", "surface-subtle": "#f5f5f5", "dock-border": "rgba(255, 255, 255, 0.08)" })
  const { element, styles } = fakeRoot()
  applyCustomTheme(modern.theme, false, element)
  expect(styles.get("--foreground")).toBe("#18181b")
  expect(styles.get("--dock-border")).toBe("rgba(255, 255, 255, 0.08)")

  // A theme object that skipped parsing still only ever writes canonical, allowlisted colours.
  const { element: raw, styles: rawStyles } = fakeRoot()
  applyCustomTheme({ version: 1, name: "Raw", colors: { foreground: "rgb(24, 24 27)", background: "RED" } }, false, raw)
  expect(rawStyles.has("--foreground")).toBe(false)
  expect(rawStyles.get("--background")).toBe("#ff0000")
})

// #1724 (Fable r1): a CSS `none` channel is painted as 0, so the parsed colour has every channel.
test("parseColor reads a CSS none channel as 0, so every Color holds all of r, g, b and alpha", async () => {
  const { canonicalColor, parseColor } = await import("../src/lib/color.ts")
  expect(parseColor("rgb(none 0 0)")).toEqual({ mode: "rgb", r: 0, g: 0, b: 0, alpha: 1 })
  expect(canonicalColor("rgb(none 255 0)")).toBe("#00ff00")
  for (const raw of ["rgb(none none none / none)", "hsl(none 0% 50%)", "oklch(none none none)"]) {
    const color = parseColor(raw)
    expect(color).not.toBeNull()
    for (const channel of [color!.r, color!.g, color!.b, color!.alpha]) expect(Number.isFinite(channel)).toBe(true)
  }
})

// #1724 (CodeRabbit): a translucent hover is checked over the well it lies on, and muted text on it too.
test("the gate composites a translucent well-hover over the well and checks muted text on the hover", () => {
  const layered = parseStudioTheme({ version: 1, name: "Dark hover", baseSkin: "polar", appearance: "light", colors: { well: "#888888", "well-hover": "rgba(0, 0, 0, 0.2)", "muted-foreground": "#000000" } })
  expect(layered.ok).toBe(false)
  if (!layered.ok) {
    const paths = layered.issues.map((i) => i.path)
    // Over the white card the hover passes (11:1); over the #888888 well it is 3.4:1.
    expect(paths).toContain("contrast.light.foreground (on well-hover over well)")
    expect(paths).not.toContain("contrast.light.foreground (on well-hover)")
    expect(paths).not.toContain("contrast.light.foreground (on well)")
  }
  const muted = parseStudioTheme({ version: 1, name: "Grey hover", baseSkin: "polar", appearance: "light", colors: { "well-hover": "#999999" } })
  expect(muted.ok).toBe(false)
  if (!muted.ok) {
    const paths = muted.issues.map((i) => i.path)
    // Polar's #585861 muted ink is 2.5:1 on #999999; its foreground (6.2:1) passes.
    expect(paths).toContain("contrast.light.muted-foreground (on well-hover)")
    expect(paths).not.toContain("contrast.light.foreground (on well-hover)")
  }
})

// #1724 (CodeRabbit): culori throwing on malformed input is a refusal, and an explicit `/ none` alpha is transparent.
test("parseColor refuses input culori throws on and reads an explicit none alpha as 0, an omitted one as 1", async () => {
  const { parseColor } = await import("../src/lib/color.ts")
  for (const raw of ["color(", "color(srgb", "rgb("]) expect(parseColor(raw), raw).toBeNull()
  expect(parseColor("rgb(0 0 0 / none)")).toEqual({ mode: "rgb", r: 0, g: 0, b: 0, alpha: 0 })
  expect(parseColor("rgb(0 0 0)")!.alpha).toBe(1)
  expect(parseColor("oklch(0.5 0.1 200 / none)")!.alpha).toBe(0)
  const bad = parseStudioTheme({ version: 1, name: "Broken", colors: { foreground: "color(" } })
  expect(bad.ok).toBe(false)
  if (!bad.ok) expect(bad.issues.map((i) => i.path)).toEqual(["colors.foreground"])
})
