import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

// Design v4 contrast & surface bar, read directly from the theme tokens (light :root, then .dark):
// - Every text / chip / link / button foreground pair reaches WCAG AA (4.5:1) in both themes.
// - Non-text canvas edges reach WCAG 1.4.11 (3:1 against the canvas surface).
// - Two-surface shell (--shell != --background) and tinted panels (--card != --background) hold in both themes.
const css = readFileSync(join(import.meta.dir, "../src/index.css"), "utf8")
const values = (name: string) => [...css.matchAll(new RegExp(`--${name}: (#[0-9a-fA-F]{6})`, "g"))].map((match) => match[1]!)
const rgbOf = (hex: string) => [1, 3, 5].map((at) => Number.parseInt(hex.slice(at, at + 2), 16)) as [number, number, number]
const mix = (fg: string, bg: string, alpha: number) => {
  const f = rgbOf(fg)
  const b = rgbOf(bg)
  return "#" + [0, 1, 2].map((i) => Math.round(f[i]! * alpha + b[i]! * (1 - alpha)).toString(16).padStart(2, "0")).join("")
}
const luminance = (hex: string) => {
  const [r, g, b] = rgbOf(hex).map((v) => v / 255).map((c) => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!
}
const ratio = (a: string, b: string) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return (hi! + 0.05) / (lo! + 0.05)
}

const SURFACES = ["shell", "background", "card", "surface-subtle", "muted", "popover"] as const
const TONES = ["gray", "green", "amber", "red", "blue", "violet", "teal", "pink", "orange"] as const
const KINDS = ["source", "mapping", "object", "transform", "question", "workflow", "guarantee", "policy"] as const

for (const [theme, at] of [["light", 0], ["dark", 1]] as const) {
  const token = (name: string) => {
    const hex = values(name)[at]
    expect(hex, `${theme} --${name}`).toBeDefined()
    return hex!
  }

  test(`${theme}: canvas edges reach 3:1 on the canvas, and edge-label pills 4.5:1`, () => {
    expect(ratio(token("edge"), token("canvas-surface"))).toBeGreaterThanOrEqual(3)
    expect(ratio(token("muted-foreground"), token("card"))).toBeGreaterThanOrEqual(4.5)
  })

  test(`${theme}: two-surface shell and tinted panels stay distinct from the page`, () => {
    expect(token("shell")).not.toBe(token("background"))
    expect(token("card")).not.toBe(token("background"))
    expect(token("surface-subtle")).not.toBe(token("background"))
  })

  test(`${theme}: body, muted, primary button, accent, link and semantic tokens pass WCAG AA (4.5:1)`, () => {
    for (const fg of ["foreground", "muted-foreground"] as const) {
      for (const bg of SURFACES) {
        expect(ratio(token(fg), token(bg)), `${theme} ${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5)
      }
    }
    expect(ratio(token("primary-foreground"), token("primary")), `${theme} primary button`).toBeGreaterThanOrEqual(4.5)
    expect(ratio(token("action-foreground"), token("action-hover")), `${theme} primary button hover`).toBeGreaterThanOrEqual(4.5)
    for (const bg of ["shell", "background", "card"] as const) {
      expect(ratio(token("action"), token(bg)), `${theme} action on ${bg}`).toBeGreaterThanOrEqual(4.5)
    }
    for (const bg of ["background", "card"] as const) {
      expect(ratio(token("link"), token(bg)), `${theme} link on ${bg}`).toBeGreaterThanOrEqual(4.5)
    }
    expect(ratio(token("destructive-foreground"), token("destructive")), `${theme} destructive badge`).toBeGreaterThanOrEqual(4.5)
    expect(ratio(token("destructive-text"), token("card")), `${theme} destructive text`).toBeGreaterThanOrEqual(4.5)
    const badgeAlpha = theme === "light" ? 0.12 : 0.16
    for (const sem of ["success", "warning"] as const) {
      expect(ratio(token(sem), token("card")), `${theme} ${sem} on card`).toBeGreaterThanOrEqual(4.5)
      expect(ratio(token(sem), mix(token(sem), token("card"), badgeAlpha)), `${theme} ${sem} on badge`).toBeGreaterThanOrEqual(4.5)
    }
  })

  test(`${theme}: every tone chip and kind badge passes WCAG AA (4.5:1) on card and on its tinted fill`, () => {
    const chipAlpha = theme === "light" ? 0.10 : 0.15
    for (const tone of TONES) {
      const hex = token(`tone-${tone}`)
      expect(ratio(hex, token("card")), `${theme} tone-${tone} on card`).toBeGreaterThanOrEqual(4.5)
      expect(ratio(hex, mix(hex, token("card"), chipAlpha)), `${theme} tone-${tone} on chip`).toBeGreaterThanOrEqual(4.5)
    }
    for (const kind of KINDS) {
      const hex = token(`kind-${kind}`)
      expect(ratio(hex, token("card")), `${theme} kind-${kind} on card`).toBeGreaterThanOrEqual(4.5)
      expect(ratio(hex, mix(hex, token("card"), 0.10)), `${theme} kind-${kind} on tinted fill`).toBeGreaterThanOrEqual(4.5)
    }
  })
}
