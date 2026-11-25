import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { converter, parse } from "culori"
import { INDEX_CSS, SKINS, resolve, skinVars, themePalettes } from "../scripts/theme-palettes.ts"
import { MAX_CHROME_CHROMA, THEME_DATA_TOKEN } from "../src/lib/studio-theme.ts"

/**
 * DESIGN.md 19.6, the monochrome rule (owner, 2026-11-25; measured against Polar's dashboard and the
 * getopen.so demo): chrome is black, white and greys. Colour is for charts and visualisations, status
 * dots beside their word, avatars, the mascot, and data highlights; those tokens are named below and
 * nothing else is exempt.
 *
 * - The default skin (`luma`) is pure neutral: every chrome colour it declares, shadows, dock, island
 *   and kind tokens included, has R = G = B (no cool or warm cast; alpha aside).
 * - The opt-in skins (`polar`, `open`, `glass`) keep a cast (polar is Tailwind zinc, about C 0.005) but
 *   no hue: every chrome token's OKLCH chroma is below 0.02, which every accent fails by a wide margin
 *   (the old blue action was 0.20, a 7% violet glow 0.23).
 */
// The exemptions and the limit are the theme contract's, so a served theme is held to the same rule.
const DATA = THEME_DATA_TOKEN
const WHY = "chart and canvas visualisation, status beside a word, avatar, the mascot, or a data highlight"
const MAX_CHROMA = MAX_CHROME_CHROMA
const oklch = converter("oklch")
const css = readFileSync(INDEX_CSS, "utf8")
const palettes = themePalettes(css)

/** Every colour literal in a declared value (a colour, or a shadow listing several). */
function colours(value: string): string[] {
  return [...value.matchAll(/#[0-9a-fA-F]{3,8}\b|rgba?\([^)]*\)/g)].map((match) => match[0])
}

/** R, G and B of a colour literal, 0–255, alpha ignored. */
function channels(colour: string): readonly [number, number, number] {
  const rgb = converter("rgb")(parse(colour))
  if (rgb === undefined) throw new Error(`not a colour: ${colour}`)
  return [rgb.r, rgb.g, rgb.b].map((value) => Math.round(value * 255)) as unknown as readonly [number, number, number]
}

for (const scheme of ["light", "dark"] as const) {
  test(`luma/${scheme}: every chrome colour the default skin declares is a pure grey (R = G = B)`, () => {
    const vars = skinVars(css, "luma", scheme)
    const cast = Object.entries(vars).filter(([name]) => !DATA.test(name)).flatMap(([name, value]) => {
      const literal = /^(var|color-mix)\(/.test(value) ? [resolveOrNull(value, vars)].filter((v): v is string => v !== null) : colours(value)
      return literal.filter((colour) => { const [r, g, b] = channels(colour); return r !== g || g !== b }).map((colour) => `--${name}: ${colour}`)
    })
    expect(cast).toEqual([])
  })
}

function resolveOrNull(value: string, vars: Readonly<Record<string, string>>): string | null {
  try { return resolve(value, vars) } catch { return null }
}

for (const skin of SKINS) for (const scheme of ["light", "dark"] as const) {
  test(`${skin}/${scheme}: every chrome token has no hue (OKLCH C < ${MAX_CHROMA}); colour lives only in tokens for ${WHY}`, () => {
    const coloured = Object.entries(palettes[skin][scheme])
      .filter(([name]) => !DATA.test(name))
      .map(([name, value]) => ({ name, value, chroma: oklch(parse(value))?.c ?? 0 }))
      .filter(({ chroma }) => chroma >= MAX_CHROMA)
      .map(({ name, value, chroma }) => `--${name}: ${value} (C ${chroma.toFixed(3)})`)
    expect(coloured).toEqual([])
  })
}

test("the CTA, accent text, link and focus ring are neutral in every skin: near-black on light, near-white or grey on dark", () => {
  for (const skin of SKINS) for (const scheme of ["light", "dark"] as const) {
    const lightness = (name: string) => oklch(parse(palettes[skin][scheme][name]!))!.l
    for (const name of ["primary", "action"]) expect(scheme === "light" ? lightness(name) < 0.3 : lightness(name) > 0.85).toBe(true)
    expect(scheme === "light" ? lightness("link") < 0.3 : lightness("link") > 0.6).toBe(true)
  }
})

test("a category dot (a source system, a kind, a version) draws the neutral mark, never a hue", () => {
  const dots = readFileSync(new URL("../src/components/system/status-dot.tsx", import.meta.url), "utf8")
  expect(dots).not.toMatch(/bg-tone-/)
})
