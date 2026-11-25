import { expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { join, relative } from "node:path"

/**
 * DESIGN.md 16.2, gate 6 (the type lock) and section 14 (logical properties), as a lint over every
 * first-party source file in `src/`: the five steps of section 8.1 are the only sizes, 400 and 500
 * the only weights (500 only where section 8.2 allows it), no uppercase or tracked chrome, and no
 * physical inline spacing. Canvas edge labels and minimap text live in `index.css`, not here.
 */
const SRC = join(import.meta.dir, "../src")

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name)
    if (entry.isDirectory()) return entry.name === "checker" || entry.name === "engine" ? [] : sources(path)
    return /\.tsx?$/.test(entry.name) && !entry.name.endsWith(".d.ts") ? [path] : []
  })
}

const FILES = sources(SRC).map((path) => ({ file: relative(SRC, path), text: readFileSync(path, "utf8") }))
const LITERAL = /"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`/g

/** Every string literal in `text` with the line it starts on. */
function literals(text: string): { readonly line: number; readonly body: string }[] {
  return [...text.matchAll(LITERAL)].map((match) => ({ line: text.slice(0, match.index).split("\n").length, body: match[0].slice(1, -1) }))
}

/** `file:line  token` for every literal in which `bad` finds a token, unless `allowed` clears that literal. */
function offenders(bad: RegExp, allowed: (body: string) => boolean = () => false): string[] {
  return FILES.flatMap(({ file, text }) => literals(text).flatMap(({ line, body }) => {
    if (allowed(body)) return []
    return [...body.matchAll(new RegExp(bad.source, "g"))].map((match) => `${file}:${line}  ${match[0]}`)
  }))
}

test("only the five steps set a size: no Tailwind sizes and no arbitrary text-[…] sizes", () => {
  expect(offenders(/(?<![\w-])(?:[^\s"'`]*:)?text-(?:xs|sm|base|lg|[2-9]?xl)(?![\w-])/)).toEqual([])
  expect(offenders(/(?<![\w-])(?:[^\s"'`]*:)?text-\[[0-9.]+(?:rem|px|em|pt)\]/)).toEqual([])
})

test("400 and 500 are the only weights", () => {
  expect(offenders(/(?<![\w-])(?:[^\s"'`]*:)?font-(?:semibold|bold|extrabold|black|light|thin|extralight)(?![\w-])/)).toEqual([])
})

test("500 sits only on a title, a heading, the primary action or an active state", () => {
  // A bare `font-medium` must share its literal with a step-3 or step-4 title, or be the primary
  // action's solid fill. A state variant (`data-active:font-medium`) is the active pill or segment.
  const heading = /(?<![\w:-])text-(?:title|heading)(?![\w-])|(?<![\w:-])bg-primary(?![\w/-])/
  expect(offenders(/(?<![\w:-])font-medium(?![\w-])/, (body) => heading.test(body))).toEqual([])
})

test("no uppercase or tracked chrome", () => {
  expect(offenders(/(?<![\w:-])uppercase(?![\w-])/)).toEqual([])
  expect(offenders(/(?<![\w-])tracking-(?:wide|wider|widest)(?![\w-])/)).toEqual([])
})

test("inline spacing, borders, radii and alignment are logical (DESIGN.md 14)", () => {
  const value = String.raw`(?:\d+(?:\.\d+)?|px|full|auto|\d+/\d+|\[[^\]\s]+\]|\([^)\s]+\))(?![\w-])`
  const physical = new RegExp(String.raw`(?<![\w-])(?:[^\s"'\x60]*:)?-?(?:(?:ml|mr|pl|pr|left|right)-${value}|border-[lr](?:-\S+)?(?![\w-])|rounded-(?:[lr]|tl|tr|bl|br)(?:-\S+)?(?![\w-])|text-(?:left|right)(?![\w-]))`)
  // Centring with `left-1/2 -translate-x-1/2` is the same in both directions.
  expect(offenders(physical, (body) => /left-1\/2/.test(body) && /-translate-x-1\/2/.test(body))).toEqual([])
})

test("focus is the 2px --ring outline, never a translucent ring that misses 3:1 (DESIGN.md 16.2, gate 5)", () => {
  expect(offenders(/(?<![\w-])(?:[^\s"'`]*:)?(?:focus-visible|focus):ring-ring\/\d+/)).toEqual([])
})

test("a status dot fill lives only in the dot primitives, so it always sits beside its word", () => {
  // The primitives' types then require the word (`Dot`'s children or name; test/system-primitives.test.tsx).
  const owners = new Set(["components/system/status-dot.tsx", "components/ui/badge.tsx"])
  const found = FILES.filter(({ file, text }) => !owners.has(file) && /(?<![\w-])(?:[^\s"'`]*:)?bg-dot-/.test(text)).map(({ file }) => file)
  expect(found).toEqual([])
})

/** The banned sparkle "AI" code points: ✨ U+2728, ✦ U+2726, ✧ U+2727, 🪄 U+1FA84. */
const SPARKLE_POINTS = [0x2728, 0x2726, 0x2727, 0x1fa84]
const SPARKLE_GLYPH = new RegExp(SPARKLE_POINTS.flatMap((point) => {
  const hex = point.toString(16)
  // JS \u{…} and \u…, HTML &#x…; and &#…;, and the CSS `content` escape \….
  const escapes = [`\\\\u\\{0*${hex}\\}`, `&#x0*${hex};`, `&#0*${point};`, `\\\\0*${hex}(?![0-9a-f])`]
  if (point <= 0xffff) escapes.push(`\\\\u${hex}`)
  else {
    const high = (Math.floor((point - 0x10000) / 0x400) + 0xd800).toString(16)
    const low = (((point - 0x10000) % 0x400) + 0xdc00).toString(16)
    escapes.push(`\\\\u${high}\\\\u${low}`)
  }
  return [String.fromCodePoint(point), ...escapes]
}).join("|") + String.raw`|\bAiGlyph\b|>\s*AI\s*<\/text>`, "giu")
/** A lucide icon export name for a sparkle, wand or stars icon. */
const SPARKLE_ICON = /Sparkle|Wand|^Stars(?:Icon)?$/
/** The same icons by their kebab-case file or dynamic name (`star` and `star-half` are fine). */
const SPARKLE_SLUG = /^(?:[a-z0-9-]+-)?sparkles?(?:-[a-z0-9-]+)?$|^(?:[a-z0-9-]+-)?wand(?:-[a-z0-9-]+)*$|^stars$/

/** Every sparkle "AI" glyph or icon a source file uses: named, aliased, namespaced, re-exported, deep-imported or dynamic. */
function sparkleHits(text: string): string[] {
  const lucide = String.raw`["'](lucide-react(?:\/[^"']*)?)["']`
  const hits: string[] = []
  // import { Sparkles as Shine } / export { Sparkles } / export { Sparkles as default } from "lucide-react…"
  for (const match of text.matchAll(new RegExp(String.raw`(?:import|export)\s+(?:type\s+)?(?:[\w$]+\s*,\s*)?\{([^}]*)\}\s*from\s*${lucide}`, "g"))) {
    for (const part of match[1]!.split(",")) {
      const name = part.trim().replace(/^type\s+/, "").split(/\s+as\s+/)[0]!
      if (SPARKLE_ICON.test(name)) hits.push(name)
    }
  }
  // import * as Icons from "lucide-react" … Icons.Sparkles / Icons["Sparkles"]; export * re-exports every icon.
  for (const match of text.matchAll(new RegExp(String.raw`import\s+(?:[\w$]+\s*,\s*)?\*\s+as\s+([\w$]+)\s+from\s*${lucide}`, "g"))) {
    const alias = match[1]!.replace(/\$/g, "\\$")
    for (const use of text.matchAll(new RegExp(String.raw`(?<![\w$])${alias}\s*(?:\.\s*([\w$]+)|\[\s*["'\x60]([\w$]+)["'\x60]\s*\])`, "g"))) {
      const name = use[1] ?? use[2]!
      if (SPARKLE_ICON.test(name)) hits.push(`${match[1]}.${name}`)
    }
  }
  for (const match of text.matchAll(new RegExp(String.raw`export\s+\*\s+(?:as\s+[\w$]+\s+)?from\s*${lucide}`, "g"))) hits.push(match[0])
  // A deep import names the icon's file: lucide-react/dist/esm/icons/sparkles(.js).
  for (const match of text.matchAll(new RegExp(lucide, "g"))) {
    const file = match[1]!.split("/").at(-1)!.replace(/\.js$/, "")
    if (match[1] !== "lucide-react" && SPARKLE_SLUG.test(file)) hits.push(match[1]!)
  }
  // lucide-react/dynamic draws an icon by its kebab-case name: any literal naming a sparkle icon.
  if (/["']lucide-react\/dynamic["']/.test(text)) {
    for (const match of text.matchAll(LITERAL)) if (SPARKLE_SLUG.test(match[0].slice(1, -1))) hits.push(match[0])
  }
  hits.push(...[...text.matchAll(SPARKLE_GLYPH)].map((match) => match[0]))
  return hits
}

test("no sparkle \"AI\" glyph: the assistant is the mascot or its name in words (DESIGN.md 19.2)", () => {
  // Owner, 2026-11-25: the sparkle "AI" icon is banned. No lucide sparkle, wand or stars icon (named,
  // aliased, namespaced, re-exported, deep-imported or dynamic), no ✨ ✦ ✧ 🪄 (or their escapes),
  // and no hand-drawn "AI" lettering glyph, in any source or stylesheet. No allowlist.
  const styles = readdirSync(SRC, { recursive: true, encoding: "utf8" }).filter((file) => file.endsWith(".css")).map((file) => ({ file, text: readFileSync(join(SRC, file), "utf8") }))
  const found = [...FILES, ...styles].flatMap(({ file, text }) => sparkleHits(text).map((hit) => `${file}  ${hit}`))
  expect(found).toEqual([])
})

test("the sparkle ban sees every way in, and not a plain star", () => {
  const banned = [
    `import { Sparkles } from "lucide-react"`,
    `import { Wand2 as Magic, Check } from "lucide-react"`,
    `import * as Icons from "lucide-react"\nconst A = <Icons.Sparkles />`,
    `import * as L from "lucide-react"\nconst A = L["WandSparkles"]`,
    `export { Sparkle } from "lucide-react"`,
    `export { Stars as Shine } from "lucide-react"`,
    `export * from "lucide-react"`,
    `import Shine from "lucide-react/dist/esm/icons/sparkles.js"`,
    `import { DynamicIcon } from "lucide-react/dynamic"\nconst A = <DynamicIcon name="wand-sparkles" />`,
    `const a = "✨ Ask"`, `const a = "\\u{2728}"`, `const a = "\\u2728"`, `const a = "&#x2728;"`, `const a = "&#10024;"`,
    `const a = "✦"`, `const a = "✧"`, `const a = "🪄"`, `const a = "\\u{1FA84}"`, `const a = "\\uD83E\\uDE84"`, `content: "&#X2728;"`, `.ask::before { content: "\\2728" }`,
  ]
  for (const text of banned) expect(sparkleHits(text)).not.toEqual([])
  for (const text of [`import { Star, StarHalf } from "lucide-react"`, `import Star from "lucide-react/dist/esm/icons/star.js"`, `import StarHalf from "lucide-react/dist/esm/icons/star-half"`, `import * as Icons from "lucide-react"\nconst A = <Icons.Star />`, `import { DynamicIcon } from "lucide-react/dynamic"\nconst A = <DynamicIcon name="star" />`, `const a = "★ rated"`]) {
    expect(sparkleHits(text)).toEqual([])
  }
})
