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
