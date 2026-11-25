/**
 * Derives every built-in skin's light and dark palette from `src/index.css`, the one source of the
 * colours Studio paints, and writes them to `src/lib/theme-palettes.json` for the custom-theme
 * contrast gate. `test/theme-palettes.test.ts` fails when the JSON drifts from the CSS.
 *
 *   bun scripts/theme-palettes.ts
 */
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { canonicalColor, cssColor, fade, parseColor } from "../src/lib/color.ts"
import { STUDIO_THEME_TOKENS } from "../src/lib/studio-theme.ts"

export const SKINS = ["polar", "open", "luma", "glass"] as const
export type Scheme = "light" | "dark"
export type ThemePalettes = Record<(typeof SKINS)[number], Record<Scheme, Record<string, string>>>

const ROOT = join(import.meta.dir, "..")
export const INDEX_CSS = join(ROOT, "src", "index.css")
export const PALETTES_JSON = join(ROOT, "src", "lib", "theme-palettes.json")

/** The custom properties each top-level rule in `css` declares, by its selector. */
function rules(css: string): Map<string, Record<string, string>> {
  const out = new Map<string, Record<string, string>>()
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "")
  for (const match of stripped.matchAll(/^([^\s@{}][^{}]*?)\s*\{([^{}]*)\}/gm)) {
    const selector = match[1]!.trim()
    const declared = out.get(selector) ?? {}
    for (const decl of match[2]!.matchAll(/--([a-z0-9-]+)\s*:\s*([^;]+);/g)) declared[decl[1]!] = decl[2]!.trim()
    out.set(selector, declared)
  }
  return out
}

/** The rules that apply, in cascade order (each later one wins), for a skin and scheme. */
function cascade(skin: (typeof SKINS)[number], scheme: Scheme): readonly string[] {
  const base = scheme === "dark" ? [":root", ".dark"] : [":root"]
  if (skin === "open") return base
  const skinned = `:root[data-skin="${skin}"]`
  return scheme === "dark" ? [...base, skinned, `:root.dark[data-skin="${skin}"]`] : [...base, skinned]
}

/** Resolve one declared value to canonical CSS: `var()` references and `color-mix(…, X p%, transparent)`. */
function resolve(value: string, vars: Readonly<Record<string, string>>, seen: ReadonlySet<string> = new Set()): string {
  const reference = /^var\(--([a-z0-9-]+)\)$/.exec(value)
  if (reference) {
    const name = reference[1]!
    const next = vars[name]
    if (next === undefined || seen.has(name)) throw new Error(`index.css: --${name} is undefined or circular`)
    return resolve(next, vars, new Set([...seen, name]))
  }
  const mix = /^color-mix\(in [a-z0-9-]+,\s*(.+?)\s+(\d+(?:\.\d+)?)%,\s*transparent\)$/.exec(value)
  if (mix) {
    // Mixing with `transparent` keeps the colour and scales its alpha, in any interpolation space.
    const color = parseColor(resolve(mix[1]!, vars, seen))
    if (color === null) throw new Error(`index.css: ${value} does not mix a colour`)
    return cssColor(fade(color, Number(mix[2]) / 100))
  }
  const canonical = canonicalColor(value)
  if (canonical === null) throw new Error(`index.css: "${value}" is not a colour Studio can gate`)
  return canonical
}

/** Every allowlisted theme token's painted colour for every built-in skin and scheme. */
export function themePalettes(css: string): ThemePalettes {
  const declared = rules(css)
  const palettes = {} as ThemePalettes
  for (const skin of SKINS) {
    palettes[skin] = { light: {}, dark: {} }
    for (const scheme of ["light", "dark"] as const) {
      const vars: Record<string, string> = {}
      for (const selector of cascade(skin, scheme)) {
        const block = declared.get(selector)
        if (block === undefined) throw new Error(`index.css: no ${selector} rule`)
        Object.assign(vars, block)
      }
      for (const token of STUDIO_THEME_TOKENS) {
        const value = vars[token]
        if (value === undefined) throw new Error(`index.css: ${skin} ${scheme} never sets --${token}`)
        palettes[skin][scheme][token] = resolve(value, vars)
      }
    }
  }
  return palettes
}

if (import.meta.main) {
  writeFileSync(PALETTES_JSON, `${JSON.stringify(themePalettes(readFileSync(INDEX_CSS, "utf8")), null, 2)}\n`)
  console.log(`wrote ${PALETTES_JSON}`)
}
