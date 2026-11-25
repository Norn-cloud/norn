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

// DESIGN.md section 3: the polar default holds without JS, so bare `:root` and `.dark` are polar and no
// rule needs `data-skin="polar"` to paint it.
test("bare :root and .dark are the polar palette, so the default needs no data-skin attribute", () => {
  const css = readFileSync(INDEX_CSS, "utf8")
  expect(css).not.toContain('[data-skin="polar"]')
  const polar = themePalettes(css).polar
  expect(polar.light.canvas).toBe("#f6f6f8")
  expect(polar.light.primary).toBe("#18181b")
  expect(polar.dark.canvas).toBe("#0a0a0b")
  expect(polar.dark.primary).toBe("#f4f4f5")
})
