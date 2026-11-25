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
  const owners = new Set(["components/system/status-dot.tsx", "components/ui/badge.tsx"])
  const found = FILES.filter(({ file, text }) => !owners.has(file) && /(?<![\w-])(?:[^\s"'`]*:)?bg-dot-/.test(text)).map(({ file }) => file)
  expect(found).toEqual([])
})
