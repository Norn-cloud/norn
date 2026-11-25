import { expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { join, relative } from "node:path"
import { colorsNamed, converter, parse } from "culori"
import { INDEX_CSS, SKINS, skinVars } from "../scripts/theme-palettes.ts"

/**
 * DESIGN.md 19.6, the monochrome rule (owner, 2026-11-25; measured against Polar's dashboard and the
 * getopen.so demo): chrome is black, white and greys. Colour is for charts and visualisations, status
 * dots beside their word, avatars, the mascot, and data highlights; those tokens are named below and
 * nothing else is exempt.
 *
 * - The default skin (`luma`) is pure neutral: every chrome colour it declares, shadows, dock, island
 *   and kind tokens included, has R = G = B (no cool or warm cast; alpha aside).
 * - The opt-in skins (`polar`, `open`, `glass`) keep a cast (polar is Tailwind zinc, about C 0.005) but
 *   no hue: every colour any chrome token paints has OKLCH chroma below 0.02, which every accent fails
 *   by a wide margin (the old blue action was 0.20, a 7% violet glow 0.23).
 * - Every custom property a skin declares is read, not only the theme tokens, and every colour in it
 *   counts: hex, rgb/hsl/hwb/lab/lch/oklab/oklch/color(), named colours, through var() (and its
 *   fallback) and color-mix(). A value the lint cannot resolve fails; nothing passes by default.
 * - Source (`src/**`) paints chrome only through tokens: no Tailwind palette colour class, and no
 *   arbitrary utility with a coloured literal. A data colour is a data token's utility (`text-success`,
 *   `bg-tone-blue`, `ring-warning`), so the exemption is the token's name, never a file list.
 */
const DATA = /^(chart-\d|dot-(live|held|bad)|success|warning|destructive(-foreground|-text)?|avatar-\d-(bg|fg)|mascot-body|tone-[a-z]+|tint-[a-z]+|edge(-active)?|node-[a-z]+|canvas-[a-z]+)$/
const WHY = "chart and canvas visualisation, status beside a word, avatar, the mascot, or a data highlight"
const MAX_CHROMA = 0.02
const oklch = converter("oklch")
const rgb = converter("rgb")
const css = readFileSync(INDEX_CSS, "utf8")
const NAMED = new Set(Object.keys(colorsNamed))
const COLOUR_FUNCTIONS = new Set(["rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color"])
/** Words that paint no colour of their own: `currentcolor` is the text colour, itself a gated token. */
const NOT_A_COLOUR = new Set(["transparent", "currentcolor", "inherit", "initial", "unset", "none"])

/** The text inside the parenthesis that opens at `open`, and the index after its closing one. */
function group(text: string, open: number): { readonly inner: string; readonly end: number } | null {
  let depth = 0
  for (let at = open; at < text.length; at += 1) {
    if (text[at] === "(") depth += 1
    else if (text[at] === ")" && --depth === 0) return { inner: text.slice(open + 1, at), end: at + 1 }
  }
  return null
}

/** A function's arguments, split at top-level commas. */
function args(inner: string): string[] {
  const out: string[] = []
  let depth = 0
  let start = 0
  for (let at = 0; at < inner.length; at += 1) {
    if (inner[at] === "(") depth += 1
    else if (inner[at] === ")") depth -= 1
    else if (inner[at] === "," && depth === 0) { out.push(inner.slice(start, at)); start = at + 1 }
  }
  out.push(inner.slice(start))
  return out.map((part) => part.trim())
}

interface Paint { readonly colours: string[]; readonly unresolved: string[] }

/**
 * Every colour a value paints. `vars` resolves `var()` (and its fallback); with `vars` null (source
 * utilities), a `var()` is a token, gated by the CSS rules, and paints nothing here. A relative
 * colour counts only as `oklch(from X l c-or-less h)`, whose chroma is at most X's.
 */
function paint(value: string, vars: Readonly<Record<string, string>> | null, seen: ReadonlySet<string> = new Set()): Paint {
  const colours: string[] = []
  const unresolved: string[] = []
  const take = (inner: Paint) => { colours.push(...inner.colours); unresolved.push(...inner.unresolved) }
  let at = 0
  while (at < value.length) {
    const rest = value.slice(at)
    const quoted = /^(["'])(?:[^"'\\]|\\.)*?\1/.exec(rest)
    if (quoted) { at += quoted[0].length; continue }
    const hex = /^#[0-9a-fA-F]{3,8}(?![\w-])/.exec(rest)
    if (hex) { colours.push(hex[0]); at += hex[0].length; continue }
    const number = /^-?(?:\d+\.?\d*|\.\d+)[a-zA-Z%]*/.exec(rest)
    if (number) { at += number[0].length; continue }
    const word = /^-?[a-zA-Z_][\w-]*/.exec(rest)
    if (!word) { at += 1; continue }
    const name = word[0].toLowerCase()
    const open = at + word[0].length
    if (value[open] !== "(") {
      if (NAMED.has(name) && !NOT_A_COLOUR.has(name)) colours.push(name)
      at = open
      continue
    }
    const fn = group(value, open)
    if (fn === null) { unresolved.push(value.slice(at)); break }
    const whole = value.slice(at, fn.end)
    at = fn.end
    if (name === "var") {
      if (vars === null) continue
      const [reference = "", ...fallback] = args(fn.inner)
      const token = reference.replace(/^--/, "")
      const declared = vars[token]
      if (declared !== undefined && !seen.has(token)) take(paint(declared, vars, new Set([...seen, token])))
      else if (fallback.length > 0) take(paint(fallback.join(","), vars, seen))
      else unresolved.push(whole)
      continue
    }
    if (name === "color-mix") {
      // A mix of colours with no hue has none; any hue in an argument is reported as that argument.
      for (const part of args(fn.inner).slice(1)) take(paint(part.replace(/(?:^|\s)\d+(?:\.\d+)?%(?=\s|$)/g, " "), vars, seen))
      continue
    }
    if (COLOUR_FUNCTIONS.has(name)) {
      const relative = /^from\s+(.+?)\s+(\S+)\s+(c|0|calc\(\s*c\s*\*\s*(\d*\.?\d+)\s*\))\s+h(?:\s*\/.*)?$/.exec(fn.inner.trim())
      if (relative && name === "oklch" && (relative[4] === undefined || Number(relative[4]) <= 1)) { take(paint(relative[1]!, vars, seen)); continue }
      if (/^from\s/.test(fn.inner.trim()) || /var\(/.test(fn.inner) || parse(whole) === undefined) { unresolved.push(whole); continue }
      colours.push(whole)
      continue
    }
    // Any other function (calc, a gradient): its arguments may paint.
    take(paint(fn.inner, vars, seen))
  }
  return { colours, unresolved }
}

const chroma = (colour: string) => oklch(parse(colour))?.c ?? 0
const grey = (colour: string) => {
  const { r, g, b } = rgb(parse(colour))!
  const [red, green, blue] = [r, g, b].map((channel) => Math.round(channel * 255))
  return red === green && green === blue
}

/** `--name: what` for every chrome token of a skin and scheme that paints a colour failing `ok`, or that cannot be resolved. */
function offenders(skin: (typeof SKINS)[number], scheme: "light" | "dark", ok: (colour: string) => boolean): string[] {
  const vars = skinVars(css, skin, scheme)
  return Object.entries(vars).filter(([name]) => !DATA.test(name)).flatMap(([name, value]) => {
    const { colours, unresolved } = paint(value, vars, new Set([name]))
    return [
      ...colours.filter((colour) => !ok(colour)).map((colour) => `--${name}: ${colour} (C ${chroma(colour).toFixed(3)})`),
      ...unresolved.map((what) => `--${name}: cannot resolve ${what}`),
    ]
  })
}

for (const scheme of ["light", "dark"] as const) {
  test(`luma/${scheme}: every colour a chrome token of the default skin paints is a pure grey (R = G = B)`, () => {
    expect(offenders("luma", scheme, grey)).toEqual([])
  })
}

for (const skin of SKINS) for (const scheme of ["light", "dark"] as const) {
  test(`${skin}/${scheme}: every colour any chrome token paints has no hue (OKLCH C < ${MAX_CHROMA}); colour lives only in tokens for ${WHY}`, () => {
    expect(offenders(skin, scheme, (colour) => chroma(colour) < MAX_CHROMA)).toEqual([])
  })
}

test("the lint reads every colour syntax and fails closed on what it cannot resolve", () => {
  const vars = { ink: "#111", blue: "oklch(0.5 0.2 260)", glow: "0 0 4px rgb(44 52 94 / 0.2)" }
  const hue = (value: string) => paint(value, vars).colours.filter((colour) => chroma(colour) >= MAX_CHROMA)
  for (const value of ["hsl(220 80% 50%)", "hsla(220, 80%, 50%, 0.5)", "lch(50 60 260)", "lab(50 20 -40)", "oklab(0.5 0.1 -0.1)", "hwb(220 10% 10%)", "color(display-p3 0.1 0.2 0.9)", "rebeccapurple", "0 1px 2px royalblue", "var(--glow)", "var(--missing, #2563eb)", "color-mix(in oklab, var(--blue) 30%, transparent)", "linear-gradient(red, blue)"]) {
    expect(hue(value).length).toBeGreaterThan(0)
  }
  for (const value of ["#777", "rgb(0 0 0 / 0.1)", "color-mix(in oklch, var(--ink) 40%, white)", "oklch(from var(--ink) 0.9 calc(c * 0.4) h)", "inset 0 1px 0 transparent", "currentColor"]) expect(hue(value)).toEqual([])
  for (const value of ["var(--missing)", "oklch(from var(--blue) l 0.2 250)", "rgb(var(--ink) / 0.5)", "color-mix(in oklab, var(--missing) 10%, transparent)"]) expect(paint(value, vars).unresolved.length).toBeGreaterThan(0)
})

test("the CTA, accent text, link and focus ring are neutral in every skin: near-black on light, near-white or grey on dark", () => {
  for (const skin of SKINS) for (const scheme of ["light", "dark"] as const) {
    const vars = skinVars(css, skin, scheme)
    const lightness = (name: string) => {
      const [colour] = paint(`var(--${name})`, vars).colours
      return oklch(parse(colour!))!.l
    }
    for (const name of ["primary", "action"]) expect(scheme === "light" ? lightness(name) < 0.3 : lightness(name) > 0.85).toBe(true)
    expect(scheme === "light" ? lightness("link") < 0.3 : lightness("link") > 0.6).toBe(true)
  }
})

test("a category dot (a source system, a kind, a version) draws the neutral mark, never a hue", () => {
  const dots = readFileSync(new URL("../src/components/system/status-dot.tsx", import.meta.url), "utf8")
  expect(dots).not.toMatch(/bg-tone-/)
})

// ---------------------------------------------------------------- source

const SRC = join(import.meta.dir, "../src")
function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) return entry.name === "checker" || entry.name === "engine" ? [] : sources(path)
    return /\.tsx?$/.test(entry.name) && !entry.name.endsWith(".d.ts") ? [path] : []
  })
}
const LITERAL = /"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`/g
const PALETTE = /^-?(?:bg|text|border(?:-[trblxyse])?|ring(?:-offset)?|inset-ring|outline|fill|stroke|from|via|to|decoration|shadow|inset-shadow|accent|caret|divide|placeholder)-(?:red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|slate|gray|zinc|neutral|stone|mauve|olive|mist|taupe)-\d{2,3}(?:\/\S+)?!?$/

/** The utility of a class (what follows its last variant `:` outside brackets and parentheses). */
function utility(token: string): string {
  let depth = 0
  let start = 0
  for (let at = 0; at < token.length; at += 1) {
    if (token[at] === "[" || token[at] === "(") depth += 1
    else if (token[at] === "]" || token[at] === ")") depth -= 1
    else if (token[at] === ":" && depth === 0) start = at + 1
  }
  return token.slice(start)
}

/** `why` for a class that paints a colour outside the tokens: a palette class, or an arbitrary value with a hue or an unresolvable colour. */
function colouredClass(token: string): string | null {
  const name = utility(token)
  if (PALETTE.test(name)) return "a Tailwind palette colour"
  const arbitrary = /^-?[a-z][a-z0-9-]*-\[(.+)\](?:\/\S+)?!?$/.exec(name)
  if (arbitrary === null) return null
  const { colours, unresolved } = paint(arbitrary[1]!.replaceAll("_", " "), null)
  const hue = colours.find((colour) => chroma(colour) >= MAX_CHROMA)
  if (hue !== undefined) return `a coloured literal ${hue}`
  return unresolved.length > 0 ? `an unresolvable colour ${unresolved[0]}` : null
}

test("source paints chrome only through tokens: no Tailwind palette colour class and no coloured arbitrary utility", () => {
  const found = sources(SRC).flatMap((path) => {
    const text = readFileSync(path, "utf8")
    return [...text.matchAll(LITERAL)].flatMap((match) => match[0].slice(1, -1).split(/\s+/).flatMap((token) => {
      const why = colouredClass(token)
      return why === null ? [] : [`${relative(SRC, path)}:${text.slice(0, match.index).split("\n").length}  ${token}  (${why})`]
    }))
  })
  expect(found).toEqual([])
})

test("the source scan sees through variants and arbitrary values, and lets token utilities through", () => {
  for (const token of ["bg-emerald-500/10", "dark:text-amber-400", "hover:ring-blue-600", "bg-[#2563eb]", "shadow-[0_0_8px_rgb(44_52_94/0.3)]", "text-[oklch(0.6_0.2_250)]", "bg-[rgb(var(--x))]", "[&>[data-slot=x]:is(a,b):hover]:border-sky-200"]) {
    expect(colouredClass(token)).not.toBeNull()
  }
  for (const token of ["bg-success/10", "text-warning", "ring-tone-amber", "bg-white", "text-black", "bg-[color-mix(in_oklch,var(--muted),var(--foreground)_5%)]", "bg-[oklch(from_var(--primary)_0.93_calc(c*0.4)_h)]", "shadow-[0_1px_2px_rgb(0_0_0/0.1)]", "data-[tone=red]:bg-muted", "grid-cols-[1fr_auto]"]) {
    expect(colouredClass(token)).toBeNull()
  }
})
