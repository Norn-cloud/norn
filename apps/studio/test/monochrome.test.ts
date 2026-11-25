import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { converter, parse } from "culori"
import { INDEX_CSS, SKINS, themePalettes } from "../scripts/theme-palettes.ts"

/**
 * DESIGN.md 19.6, the monochrome rule (owner, 2026-11-25): chrome is black, white and greys in every
 * skin and scheme, like Polar's dashboard. Colour is for charts and visualisations, status dots
 * beside their word, avatars, the Norn mark and its mascot, and data highlights; those tokens are
 * named here and nothing else is exempt.
 *
 * Threshold: OKLCH chroma below 0.02. A neutral with a cool or warm cast (Tailwind's zinc and stone
 * sit at 0.004–0.012, the islands' mist at about 0.006) passes; any accent fails by a wide margin
 * (the old blue action was 0.20, the violet 0.23, a 7% violet glow still 0.23, since a translucent
 * colour keeps its hue over a neutral ground). Each colour is judged as written, alpha aside.
 */
const EXEMPT: Readonly<Record<string, string>> = {
  "chart-1": "chart series", "chart-2": "chart series", "chart-3": "chart series", "chart-4": "chart series", "chart-5": "chart series",
  "dot-live": "status dot beside its word", "dot-held": "status dot beside its word", "dot-bad": "status dot beside its word",
  success: "status text", warning: "status text", destructive: "status: the destructive confirm and error", "destructive-foreground": "ink on the destructive fill", "destructive-text": "status text",
  "avatar-1-bg": "avatar disc", "avatar-1-fg": "avatar disc", "avatar-2-bg": "avatar disc", "avatar-2-fg": "avatar disc", "avatar-3-bg": "avatar disc", "avatar-3-fg": "avatar disc",
  "avatar-4-bg": "avatar disc", "avatar-4-fg": "avatar disc", "avatar-5-bg": "avatar disc", "avatar-5-fg": "avatar disc", "avatar-6-bg": "avatar disc", "avatar-6-fg": "avatar disc",
  knot: "the Norn mark and the mascot",
}
const MAX_CHROMA = 0.02
const oklch = converter("oklch")
const palettes = themePalettes(readFileSync(INDEX_CSS, "utf8"))

for (const skin of SKINS) for (const scheme of ["light", "dark"] as const) {
  test(`${skin}/${scheme}: every chrome token is a neutral (OKLCH C < ${MAX_CHROMA}); colour lives only in the named data, status, avatar and mark tokens`, () => {
    const tokens = palettes[skin][scheme]
    const coloured = Object.entries(tokens)
      .filter(([name]) => EXEMPT[name] === undefined)
      .map(([name, value]) => ({ name, value, chroma: oklch(parse(value))?.c ?? 0 }))
      .filter(({ chroma }) => chroma >= MAX_CHROMA)
      .map(({ name, value, chroma }) => `--${name}: ${value} (C ${chroma.toFixed(3)})`)
    expect(coloured).toEqual([])
  })
}

test("the CTA, the accent text and the focus ring are neutral in every skin: near-black on light, near-white on dark", () => {
  for (const skin of SKINS) for (const scheme of ["light", "dark"] as const) {
    const lightness = (name: string) => oklch(parse(palettes[skin][scheme][name]!))!.l
    for (const name of ["primary", "action", "link"]) expect(scheme === "light" ? lightness(name) < 0.3 : lightness(name) > 0.85).toBe(true)
  }
})

test("a category dot (a source system, a kind) draws the neutral mark, never a hue", () => {
  const dots = readFileSync(new URL("../src/components/system/status-dot.tsx", import.meta.url), "utf8")
  expect(dots).not.toMatch(/bg-tone-/)
})
