import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { INDEX_CSS, PALETTES_JSON, themePalettes } from "../scripts/theme-palettes.ts"

// #1705: the contrast gate's palettes are generated from index.css; regenerate with `bun scripts/theme-palettes.ts`.
test("theme-palettes.json equals the palettes index.css paints, for every skin, scheme and token", () => {
  expect(JSON.parse(readFileSync(PALETTES_JSON, "utf8"))).toEqual(themePalettes(readFileSync(INDEX_CSS, "utf8")))
})

test("the generator refuses a token it cannot resolve to a colour", () => {
  const css = readFileSync(INDEX_CSS, "utf8").replace("--foreground: #1b1b1d;", "--foreground: var(--nowhere);")
  expect(() => themePalettes(css)).toThrow("--nowhere")
})

// DESIGN.md section 3: the luma default holds without JS, so bare `:root` and `.dark` are luma, no rule
// needs `data-skin="luma"` to paint it, and polar is one opt-in skin among the others.
test("bare :root and .dark are the luma palette, so the default needs no data-skin attribute", () => {
  const css = readFileSync(INDEX_CSS, "utf8")
  expect(css).not.toContain('[data-skin="luma"]')
  const palettes = themePalettes(css)
  expect(palettes.luma.light.canvas).toBe("#f3f4f6")
  expect(palettes.luma.light.primary).toBe("#554ce8")
  expect(palettes.luma.dark.canvas).toBe("#0b0b0e")
  expect(palettes.luma.dark.primary).toBe("#6258ec")
  // Its islands are glass: a translucent material over a backdrop of its own.
  expect(palettes.luma.light.island).toMatch(/^rgba\(/)
  expect(palettes.luma.light.backdrop).not.toBe(palettes.luma.light.canvas)
  // Polar keeps its monochrome palette, now behind its own attribute, with opaque islands.
  expect(css).toContain(':root[data-skin="polar"] {')
  expect(palettes.polar.light.canvas).toBe("#f6f6f8")
  expect(palettes.polar.light.primary).toBe("#18181b")
  expect(palettes.polar.dark.canvas).toBe("#0a0a0b")
  expect(palettes.polar.dark.primary).toBe("#f4f4f5")
  expect(palettes.polar.light.island).toBe(palettes.polar.light.canvas)
})
