// Themes as data: a typed `defineTheme` lowers to the `theme` definition the cell admits and serves,
// gated at lowering by the same rules Studio paints by and the cell admits by.
import { expect, test } from "bun:test"
import { TH, WORKSPACE_THEME_ID, defineTheme, lowerTheme } from "../src/builder/index.ts"

const graphite = defineTheme({
  name: "Graphite",
  baseSkin: "polar",
  colors: { "chart-1": "#0f726a" },
  variants: {
    light: { canvas: "#F3F4F6", foreground: "#111318", ring: "rgb(63, 63, 70)" },
    dark: { canvas: "#08090c", foreground: "#f1f5f9", border: "rgba(255, 255, 255, 0.080)" },
  },
})

test("a theme lowers to the workspace's theme definition, its colours written in canonical form", () => {
  const lowered = lowerTheme(graphite)
  expect(lowered).toEqual({
    ok: true,
    definition: {
      kind: "theme",
      id: WORKSPACE_THEME_ID,
      payload: {
        version: 1,
        name: "Graphite",
        baseSkin: "polar",
        colors: { "chart-1": "#0f726a" },
        variants: {
          light: { canvas: "#f3f4f6", foreground: "#111318", ring: "#3f3f46" },
          dark: { canvas: "#08090c", foreground: "#f1f5f9", border: "rgba(255, 255, 255, 0.08)" },
        },
      },
    },
  })
  // The payload is the one Studio paints: lowering it again changes nothing.
  if (lowered.ok) expect(TH.parseStudioTheme(lowered.definition.payload)).toEqual({ ok: true, theme: lowered.definition.payload, issues: [] })
})

test("an explicit definition id is kept, and one the cell would refuse is refused at lowering", () => {
  const named = lowerTheme(defineTheme({ id: "brand.dark", name: "Dark", appearance: "dark" }))
  expect(named.ok && named.definition.id).toBe("brand.dark")
  const bad = lowerTheme(defineTheme({ id: "Brand Dark", name: "Dark" }))
  expect(bad).toEqual({ ok: false, issues: [{ code: "theme.validation.schema", path: "id", message: expect.stringContaining("Brand Dark") }] })
})

test("a colour that only looks like one is refused at lowering, with the token it was for", () => {
  const lowered = lowerTheme(defineTheme({ name: "Typo", colors: { foreground: "#12", ring: "rgb(300, 0, 0)" }, accent: "rgba(0, 0, 0, 2)" }))
  expect(lowered.ok).toBe(false)
  if (!lowered.ok) {
    expect(lowered.issues.map(({ code, path }) => [code, path])).toEqual([
      ["theme.validation.invalid_color", "accent"],
      ["theme.validation.invalid_color", "colors.foreground"],
      ["theme.validation.invalid_color", "colors.ring"],
    ])
  }
})

test("a low-contrast theme is refused with the pair and the scheme named", () => {
  const lowered = lowerTheme(defineTheme({ name: "Faint", appearance: "light", colors: { foreground: "#d4d4d8" } }))
  expect(lowered.ok).toBe(false)
  if (!lowered.ok) {
    expect(new Set(lowered.issues.map((issue) => issue.code))).toEqual(new Set(["theme.validation.contrast"]))
    expect(lowered.issues.map((issue) => issue.path)).toContain("contrast.light.foreground (on canvas)")
  }
})

test("a hue on a chrome token is refused; colour on a data, status or avatar token is not", () => {
  const blue = lowerTheme(defineTheme({ name: "Blue", accent: "#1d4ed8" }))
  expect(blue.ok).toBe(false)
  if (!blue.ok) {
    const monochrome = blue.issues.filter((issue) => issue.code === "theme.validation.monochrome").map((issue) => issue.path)
    for (const scheme of ["light", "dark"]) for (const token of ["primary", "action", "ring", "link"]) expect(monochrome).toContain(`monochrome.${scheme}.${token}`)
  }
  const data = lowerTheme(defineTheme({ name: "Data", colors: { "chart-2": "#2563eb", "avatar-1-bg": "#fde68a", "avatar-1-fg": "#422006", "dot-live": "#16a34a" } }))
  expect(data.ok, JSON.stringify(!data.ok && data.issues)).toBe(true)
})

test("the token allowlist and the built-in skins are the shared contract's", () => {
  expect(TH.STUDIO_THEME_TOKENS).toContain("canvas")
  expect(new Set(TH.STUDIO_THEME_TOKENS).size).toBe(TH.STUDIO_THEME_TOKENS.length)
  expect(TH.THEME_SKINS).toEqual(["polar", "open", "luma", "glass"])
  for (const skin of TH.THEME_SKINS) for (const scheme of ["light", "dark"] as const) {
    expect(Object.keys(TH.BASE_PALETTES[skin][scheme]).sort()).toEqual([...TH.STUDIO_THEME_TOKENS].sort())
    // Every built-in skin passes its own gates.
    expect(TH.validateStudioThemeContrast({ version: 1, name: skin, baseSkin: skin, appearance: scheme })).toEqual([])
  }
})

test("canonical colour forms parse, round-trip and refuse what is out of range", () => {
  expect(TH.canonicalThemeColor("#ABC")).toBe("#aabbcc")
  expect(TH.canonicalThemeColor("#aabbcc80")).toBe("rgba(170, 187, 204, 0.5)")
  expect(TH.canonicalThemeColor("rgb(1, 2, 3)")).toBe("#010203")
  expect(TH.canonicalThemeColor("rgba(1, 2, 3, 1)")).toBe("#010203")
  for (const raw of ["red", "#ab", "rgb(256, 0, 0)", "rgba(0, 0, 0, 1.5)", "oklch(0.5 0 0)", "rgb(0 0 0)", "", 12]) expect(TH.canonicalThemeColor(raw), String(raw)).toBeNull()
  // Composited as a browser paints: half black over white is #808080, and contrast is WCAG 2.x.
  const white = TH.parseThemeColor("#ffffff")!
  expect(TH.cssColor(TH.over(TH.parseThemeColor("rgba(0, 0, 0, 0.5)")!, white))).toBe("#808080")
  expect(TH.contrastOn(TH.parseThemeColor("#000000")!, white)).toBeCloseTo(21, 9)
  expect(TH.oklchChroma(TH.parseThemeColor("#777777")!)).toBeLessThan(1e-6)
})

test("tokens, skins and colours are typed: a typo does not compile", () => {
  defineTheme({
    name: "Typed",
    // @ts-expect-error: not a theme token
    colors: { foregroud: "#000000" },
  })
  defineTheme({
    name: "Typed",
    // @ts-expect-error: geometry is never a theme token
    variants: { light: { "z-index": "#000000" } },
  })
  // @ts-expect-error: not a built-in skin
  defineTheme({ name: "Typed", baseSkin: "neon" })
  // @ts-expect-error: a theme colour is written in canonical form, not as a name
  defineTheme({ name: "Typed", accent: "red" })
  // @ts-expect-error: a theme has a name
  defineTheme({ baseSkin: "luma" })
  // @ts-expect-error: appearance is light or dark
  defineTheme({ name: "Typed", appearance: "dim" })
  expect(true).toBe(true)
})
