import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

// The canvas's contrast bar, read from the theme itself (light :root, then .dark): edges are non-text graphics
// (WCAG 1.4.11, 3:1 against the canvas); edge-label pills are text (1.4.3, 4.5:1 against the pill).
const css = readFileSync(join(import.meta.dir, "../src/index.css"), "utf8")
const values = (name: string) => [...css.matchAll(new RegExp(`--${name}: (#[0-9a-fA-F]{6})`, "g"))].map((match) => match[1]!)
const luminance = (hex: string) => {
  const [r, g, b] = [1, 3, 5].map((at) => Number.parseInt(hex.slice(at, at + 2), 16) / 255).map((c) => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!
}
const ratio = (a: string, b: string) => { const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x); return (hi! + 0.05) / (lo! + 0.05) }

for (const [theme, at] of [["light", 0], ["dark", 1]] as const) {
  test(`${theme}: canvas edges reach 3:1 on the canvas, and edge-label pills 4.5:1`, () => {
    expect(ratio(values("edge")[at]!, values("canvas-surface")[at]!)).toBeGreaterThanOrEqual(3)
    expect(ratio(values("muted-foreground")[at]!, values("card")[at]!)).toBeGreaterThanOrEqual(4.5)
  })
}
