import { expect, test } from "bun:test"
import {
  STUDIO_THEME_PRESETS,
  applyCustomTheme,
  exportStudioThemeJson,
  parseStudioTheme,
  resolveCustomThemeTokens,
  validateStudioThemeContrast,
} from "../src/lib/studio-theme.ts"

function fakeRoot(dark = false) {
  const styles = new Map<string, string>()
  const dataset: Record<string, string> = {}
  return {
    styles,
    dataset,
    element: {
      dataset,
      classList: { contains: (name: string) => name === "dark" && dark },
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

test("parseStudioTheme enforces WCAG AA contrast floors and rejects illegible custom themes", () => {
  const lowContrast = parseStudioTheme({
    version: 1,
    name: "Low Contrast",
    baseSkin: "polar",
    variants: {
      light: {
        background: "#ffffff",
        foreground: "#d4d4d8",
      },
    },
  })
  expect(lowContrast.ok).toBe(false)
  if (!lowContrast.ok) {
    expect(lowContrast.issues.some((i) => i.path.includes("contrast.light.foreground"))).toBe(true)
  }
})

test("shorthand accent and canvas expand with auto-contrasting foreground ink", () => {
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
  expect(tokens.ring).toBe("#1d4ed8")
})

test("applyCustomTheme writes variant tokens for light/dark and removes every inline property cleanly on reset", () => {
  const preset = STUDIO_THEME_PRESETS[1]!
  const lightRoot = fakeRoot(false)
  applyCustomTheme(preset, false, lightRoot.element)
  expect(lightRoot.dataset.customTheme).toBe(preset.name)
  expect(lightRoot.styles.get("--background")).toBe("#fcfcfd")
  expect(lightRoot.styles.get("--chart-1")).toBe("#111318")

  // Switching to dark variant updates tokens cleanly without leaving stale keys.
  applyCustomTheme(preset, true, lightRoot.element)
  expect(lightRoot.styles.get("--background")).toBe("#0f1117")
  expect(lightRoot.styles.get("--chart-1")).toBe("#3ccf8e")

  // Clearing removes every inline custom property and metadata attribute.
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
