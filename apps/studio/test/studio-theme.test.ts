import { expect, test } from "bun:test"
import { applyAppearance } from "../src/lib/appearance.tsx"
import {
  STUDIO_THEME_PRESETS,
  applyCustomTheme,
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
