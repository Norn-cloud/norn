import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Chip, type Tone } from "../src/components/system/chip.tsx"
import { ChoiceColumn } from "../src/components/decision/choice-columns.tsx"
import { badgeVariants } from "../src/components/ui/badge.tsx"
import { buttonVariants } from "../src/components/ui/button.tsx"
import { Tabs, TabsList, TabsTrigger } from "../src/components/ui/tabs.tsx"
import { KINDS, STATUS, type Kind, type Status } from "../src/lib/kinds.ts"
import { resolve as resolveToken, skinVars, themePalettes } from "../scripts/theme-palettes.ts"
import { contrastOn, cssColor, fade, over, parseColor, type Color } from "../src/lib/color.ts"

type Skin = "polar" | "open" | "luma" | "glass"

/**
 * Design v4 contrast, surface and focus-indicator invariants, derived directly from the live
 * theme tokens (`index.css`) and real component class lists / rendered markup across resting,
 * hover, selected, focus and disabled states.
 */
type Theme = "light" | "dark"
type InteractionState = "rest" | "hover" | "selected" | "selected-hover"

const SRC = join(import.meta.dir, "../src")
const readSrc = (rel: string) => readFileSync(join(SRC, rel), "utf8")
const css = readSrc("index.css")

/** A skin's tokens for one scheme, `var()` aliases resolved, from the same cascade the palettes use. */
function skinTokens(skin: Skin, theme: Theme): Readonly<Record<string, string>> {
  const raw = skinVars(css, skin, theme)
  const resolved: Record<string, string> = {}
  const resolve = (name: string, depth = 0): string => {
    if (resolved[name] !== undefined) return resolved[name]!
    if (depth > 8) throw new Error(`cyclic CSS token --${name}`)
    const value = raw[name]
    if (value === undefined) throw new Error(`unknown CSS token --${name} in ${skin}/${theme}`)
    const alias = /^var\(--([a-z0-9-]+)\)$/.exec(value)
    const out = alias ? resolve(alias[1]!, depth + 1) : value
    resolved[name] = out
    return out
  }
  for (const name of Object.keys(raw)) resolve(name)
  return resolved
}

/**
 * The two opaque first-class skins every component class list is measured in: `polar`, the default
 * (bare `:root` and `.dark`), and `open`.
 */
const OPAQUE_SKINS = ["polar", "open"] as const
const SKIN_TOKENS = Object.fromEntries(OPAQUE_SKINS.map((skin) => [skin, { light: skinTokens(skin, "light"), dark: skinTokens(skin, "dark") }])) as
  Readonly<Record<(typeof OPAQUE_SKINS)[number], Readonly<Record<Theme, Readonly<Record<string, string>>>>>>
let THEME_TOKENS: Readonly<Record<Theme, Readonly<Record<string, string>>>> = SKIN_TOKENS.polar

/**
 * Every skin's tokens as the colours a browser paints over the page: a translucent value (glass's
 * surfaces, a `color-mix(…, transparent)` dot) is composited over `--canvas` at 8 bits a channel, as
 * DESIGN.md 4.2 measures. Luma and glass are measured this way where a class list is checked in all four skins.
 */
const ALL_SKINS = ["polar", "open", "luma", "glass"] as const
function flatTokens(skin: Skin, theme: Theme): Readonly<Record<string, string>> {
  const raw = skinVars(css, skin, theme)
  const canvas = parseColor(resolveToken(raw["canvas"]!, raw))
  if (canvas === null || canvas.alpha < 1) throw new Error(`${skin}/${theme} --canvas is not an opaque colour`)
  const flat: Record<string, string> = {}
  for (const name of Object.keys(raw)) {
    let value: string
    try { value = resolveToken(raw[name]!, raw) } catch { continue }
    const color = parseColor(value)
    if (color !== null) flat[name] = cssColor(color.alpha < 1 ? over(color, canvas) : color)
  }
  return flat
}
/** Polar and open as the opaque tests read them; luma and glass flattened over their canvas. */
const FLAT_TOKENS: Readonly<Record<Skin, Readonly<Record<Theme, Readonly<Record<string, string>>>>>> = {
  ...SKIN_TOKENS,
  luma: { light: flatTokens("luma", "light"), dark: flatTokens("luma", "dark") },
  glass: { light: flatTokens("glass", "light"), dark: flatTokens("glass", "dark") },
}

/**
 * The skins every component class list is measured in: `luma`, the default (its translucent islands
 * flattened over the canvas; the islands themselves are measured over the backdrop below), and the
 * opaque `polar` and `open`.
 */
const COMPONENT_SKINS = ["luma", "polar", "open"] as const
const COMPONENT_TOKENS: Readonly<Record<(typeof COMPONENT_SKINS)[number], Readonly<Record<Theme, Readonly<Record<string, string>>>>>> = {
  luma: FLAT_TOKENS.luma, polar: SKIN_TOKENS.polar, open: SKIN_TOKENS.open,
}

const COLOR_MAP: Readonly<Record<string, string>> = (() => {
  const block = /@theme\s+inline\s*\{([^}]+)\}/.exec(css)?.[1] ?? ""
  const map: Record<string, string> = {}
  for (const [, color, token] of block.matchAll(/--color-([a-z0-9-]+):\s*var\(--([a-z0-9-]+)\);/g)) {
    map[color!] = token!
  }
  return map
})()

const rgbOf = (hex: string): [number, number, number] => {
  if (!/^#[0-9a-fA-F]{6}$/.test(hex)) throw new Error(`expected 6-digit hex, got ${hex}`)
  return [1, 3, 5].map((at) => Number.parseInt(hex.slice(at, at + 2), 16)) as [number, number, number]
}

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

function tokenHex(theme: Theme, name: string): string {
  const value = THEME_TOKENS[theme][name]
  if (!value || !/^#[0-9a-fA-F]{6}$/.test(value)) throw new Error(`${theme} --${name} is not a hex color: ${value}`)
  return value
}

function colorTokenHex(theme: Theme, colorName: string): string | undefined {
  if (colorName === "white") return "#ffffff"
  if (colorName === "black") return "#000000"
  const mapped = COLOR_MAP[colorName] ?? colorName
  const value = THEME_TOKENS[theme][mapped]
  return value && /^#[0-9a-fA-F]{6}$/.test(value) ? value : undefined
}

interface ColorSpec {
  readonly hex: string
  readonly alpha: number
  readonly raw: string
}

function parseColorUtility(theme: Theme, utility: string, prefix: "bg" | "text"): ColorSpec | "transparent" | undefined {
  if (!utility.startsWith(`${prefix}-`)) return undefined
  const body = utility.slice(prefix.length + 1)
  if (body === "transparent") return "transparent"
  if (body === "inherit" || body === "current") return undefined
  // Arbitrary var(--token)
  const varMatch = /^\[var\(--([a-z0-9-]+)\)\]$/.exec(body)
  if (varMatch) return { hex: tokenHex(theme, varMatch[1]!), alpha: 1, raw: utility }
  // Arbitrary color-mix(in_oklab,var(--token)_N%,transparent)
  const mixMatch = /^\[color-mix\(in_oklab,var\(--([a-z0-9-]+)\)_(\d+)%,transparent\)\]$/.exec(body)
  if (mixMatch) return { hex: tokenHex(theme, mixMatch[1]!), alpha: Number(mixMatch[2]) / 100, raw: utility }
  // Standard color or color/alpha (skip non-color utilities like text-sm, text-xs, text-left, bg-clip-padding, bg-gradient-*)
  const [name, alphaPart] = body.split("/")
  if (!name) return undefined
  const hex = colorTokenHex(theme, name)
  if (!hex) return undefined
  const alpha = alphaPart === undefined ? 1 : Number(alphaPart) / 100
  if (Number.isNaN(alpha)) return undefined
  return { hex, alpha, raw: utility }
}

interface ResolvedPair {
  readonly fg: string
  readonly bg: string
  readonly ratio: number
  readonly opacity: number
}

const SELECTED_VARIANTS = new Set(["data-active", "aria-pressed", "data-[status=active]", "data-current", "aria-[current=page]"])
const HOVER_VARIANTS = new Set(["hover", "[a]:hover"])

/**
 * Evaluate a real Tailwind class list string in a given theme and interaction state over `hostBgHex`.
 * Handles base, `dark:`, `hover:` / `[a]:hover:`, selected variants (`data-active:`, `aria-pressed:`,
 * `data-[status=active]:`, `data-current:`), combinations thereof, translucent `/alpha` fills, and `opacity-*`.
 */
function resolveClasses(
  classList: string,
  theme: Theme,
  state: InteractionState,
  hostBgHex: string,
  defaultFgHex = tokenHex(theme, "foreground"),
): ResolvedPair {
  const isDark = theme === "dark"
  const isHover = state === "hover" || state === "selected-hover"
  const isSelected = state === "selected" || state === "selected-hover"

  // Order layers by cascade precedence: base < dark < selected < dark:selected < hover < dark:hover < selected:hover < dark:selected:hover
  const scoreOf = (variants: readonly string[]): number | null => {
    let dark = false
    let hover = false
    let selected = false
    for (const v of variants) {
      if (v === "dark") dark = true
      else if (HOVER_VARIANTS.has(v)) hover = true
      else if (SELECTED_VARIANTS.has(v)) selected = true
      else return null // ignore focus-visible, disabled, aria-invalid, group-data-*, responsive, etc.
    }
    if (dark && !isDark) return null
    if (hover && !isHover) return null
    if (selected && !isSelected) return null
    return (hover ? 4 : 0) + (selected ? 2 : 0) + (dark ? 1 : 0)
  }

  let bestBg: { score: number; value: ColorSpec | "transparent" } | undefined
  let bestFg: { score: number; value: ColorSpec } | undefined
  let bestOpacity: { score: number; value: number } | undefined

  for (const token of classList.split(/\s+/).filter(Boolean)) {
    // Split variant prefixes without splitting inside [...] brackets.
    const parts: string[] = []
    let depth = 0
    let start = 0
    for (let i = 0; i < token.length; i += 1) {
      const ch = token[i]
      if (ch === "[") depth += 1
      else if (ch === "]") depth -= 1
      else if (ch === ":" && depth === 0) {
        parts.push(token.slice(start, i))
        start = i + 1
      }
    }
    const utility = token.slice(start)
    // Combine `[a]` + `hover` back into `[a]:hover`
    const variants: string[] = []
    for (let i = 0; i < parts.length; i += 1) {
      if (parts[i] === "[a]" && parts[i + 1] === "hover") {
        variants.push("[a]:hover")
        i += 1
      } else {
        variants.push(parts[i]!)
      }
    }
    const score = scoreOf(variants)
    if (score === null) continue

    const bg = parseColorUtility(theme, utility, "bg")
    if (bg !== undefined && (!bestBg || score >= bestBg.score)) bestBg = { score, value: bg }

    const fg = parseColorUtility(theme, utility, "text")
    if (fg !== undefined && fg !== "transparent" && (!bestFg || score >= bestFg.score)) bestFg = { score, value: fg }

    const op = /^opacity-(\d+)$/.exec(utility)
    if (op && (!bestOpacity || score >= bestOpacity.score)) bestOpacity = { score, value: Number(op[1]) / 100 }
  }

  const opacity = bestOpacity?.value ?? 1
  const rawBg = !bestBg || bestBg.value === "transparent"
    ? hostBgHex
    : mix(bestBg.value.hex, hostBgHex, bestBg.value.alpha)
  const bg = opacity < 1 ? mix(rawBg, hostBgHex, opacity) : rawBg
  const rawFg = bestFg ? mix(bestFg.value.hex, bg, bestFg.value.alpha) : defaultFgHex
  const fg = opacity < 1 ? mix(rawFg, bg, opacity) : rawFg
  return { fg, bg, ratio: ratio(fg, bg), opacity }
}

function extractClass(html: string, tagPattern: RegExp): string {
  const match = tagPattern.exec(html)
  expect(match?.[1], `expected class matching ${tagPattern}`).toBeDefined()
  return match![1]!
}

const SURFACES = ["shell", "background", "card", "surface-subtle", "muted", "popover", "accent"] as const
const HOST_SURFACES = ["background", "card", "popover", "surface-subtle"] as const
const TONES: readonly Tone[] = ["gray", "green", "amber", "red", "blue", "violet", "teal", "pink", "orange"]
const KIND_KEYS = Object.keys(KINDS) as Kind[]
const STATUS_KEYS = Object.keys(STATUS) as Status[]
const BUTTON_VARIANTS = ["default", "outline", "secondary", "ghost", "destructive", "link"] as const
const BADGE_VARIANTS = ["default", "secondary", "destructive", "outline", "success", "warning", "info", "ghost", "link"] as const

const shellSource = readSrc("shell.tsx")
const shellNavSource = readSrc("shell-nav.tsx")
const tabsSource = readSrc("components/ui/tabs.tsx")
const recordsGridSource = readSrc("grid/records-grid.tsx")
const decisionTreeSource = readSrc("decision-tree.tsx")

for (const skin of COMPONENT_SKINS) for (const theme of ["light", "dark"] as const) {
  test(`${skin}/${theme}: surfaces stay distinct, text tokens pass 4.5:1, and edges/focus ring pass 3:1`, () => {
    THEME_TOKENS = COMPONENT_TOKENS[skin]
    expect(tokenHex(theme, "shell")).not.toBe(tokenHex(theme, "background"))
    expect(tokenHex(theme, "well")).not.toBe(tokenHex(theme, "background"))
    if (theme === "dark") expect(tokenHex(theme, "card")).not.toBe(tokenHex(theme, "background"))
    expect(tokenHex(theme, "surface-subtle")).not.toBe(tokenHex(theme, "background"))

    expect(ratio(tokenHex(theme, "edge"), tokenHex(theme, "canvas-surface")), `${theme} edge`).toBeGreaterThanOrEqual(3)
    expect(ratio(tokenHex(theme, "edge-active"), tokenHex(theme, "canvas-surface")), `${theme} edge-active`).toBeGreaterThanOrEqual(3)

    for (const bg of SURFACES) {
      expect(ratio(tokenHex(theme, "foreground"), tokenHex(theme, bg)), `${theme} foreground on ${bg}`).toBeGreaterThanOrEqual(4.5)
      expect(ratio(tokenHex(theme, "muted-foreground"), tokenHex(theme, bg)), `${theme} muted-foreground on ${bg}`).toBeGreaterThanOrEqual(4.5)
    }
    for (const bg of ["shell", "background", "card", "surface-subtle", "popover"] as const) {
      expect(ratio(tokenHex(theme, "ring"), tokenHex(theme, bg)), `${theme} focus ring on ${bg}`).toBeGreaterThanOrEqual(3)
    }
    for (const bg of ["background", "card"] as const) {
      expect(ratio(tokenHex(theme, "link"), tokenHex(theme, bg)), `${theme} link on ${bg}`).toBeGreaterThanOrEqual(4.5)
    }
  })

  test(`${skin}/${theme}: every Button variant passes 4.5:1 at rest and on hover across real hosting surfaces, with dedicated focus outline and flat disabled state`, () => {
    THEME_TOKENS = COMPONENT_TOKENS[skin]
    for (const variant of BUTTON_VARIANTS) {
      const classes = buttonVariants({ variant })
      for (const host of HOST_SURFACES) {
        const hostHex = tokenHex(theme, host)
        const rest = resolveClasses(classes, theme, "rest", hostHex)
        const hover = resolveClasses(classes, theme, "hover", hostHex)
        expect(rest.ratio, `${theme} Button[${variant}] rest on ${host} (${rest.fg} on ${rest.bg})`).toBeGreaterThanOrEqual(4.5)
        expect(hover.ratio, `${theme} Button[${variant}] hover on ${host} (${hover.fg} on ${hover.bg})`).toBeGreaterThanOrEqual(4.5)
      }
      // Dedicated keyboard focus outline (cannot be overridden by border/shadow/ring styles) and flat disabled state.
      expect(classes).toContain("focus-visible:outline-2")
      expect(classes).toContain("focus-visible:outline-solid")
      expect(classes).toContain("focus-visible:outline-offset-2")
      expect(classes).toContain(variant === "destructive" ? "focus-visible:outline-destructive" : "focus-visible:outline-ring")
      expect(classes).toContain("disabled:opacity-")
      expect(classes).toContain("disabled:shadow-none")
    }
    // Destructive focus outline itself reaches >= 3:1 against card and popover.
    for (const host of ["background", "card", "popover"] as const) {
      expect(ratio(tokenHex(theme, "destructive"), tokenHex(theme, host)), `${theme} destructive focus outline on ${host}`).toBeGreaterThanOrEqual(3)
    }
  })

  test(`${skin}/${theme}: ChoiceColumn stays opaque on enabled unselected options and passes 4.5:1 across resting, selected, unselected and hover states`, () => {
    THEME_TOKENS = COMPONENT_TOKENS[skin]
    const options = [
      { id: "a", label: "Every record", detail: "all rows", marks: ["recommended" as const] },
      { id: "b", label: "Filtered", detail: "subset", marks: ["agent" as const] },
    ]
    const hostHex = tokenHex(theme, "background")
    for (const selected of [undefined, "b"] as const) {
      const html = renderToStaticMarkup(createElement(ChoiceColumn, { title: "Only where", options, selected, onSelect: () => {} }))
      const buttons = [...html.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)]
      expect(buttons).toHaveLength(2)
      for (const [, attrs, inner] of buttons) {
        const btnClass = /class="([^"]+)"/.exec(attrs!)?.[1] ?? ""
        // Enabled choices never dim via opacity.
        expect(btnClass, `${theme} ChoiceColumn button must stay opaque`).not.toMatch(/\bopacity-\d+\b/)
        // Dedicated focus outline cannot be overridden by selection ring.
        expect(btnClass).toContain("focus-visible:outline-2")
        expect(btnClass).toContain("focus-visible:outline-solid")
        expect(btnClass).toContain("focus-visible:outline-ring")

        for (const state of ["rest", "hover"] as const) {
          const surface = resolveClasses(btnClass, theme, state, hostHex)
          const spans = [...inner!.matchAll(/<span\s+class="([^"]+)">([^<]+)<\/span>/g)]
          expect(spans.length).toBeGreaterThanOrEqual(3)
          for (const [, spanClass, text] of spans) {
            const pair = resolveClasses(spanClass!, theme, "rest", surface.bg)
            expect(pair.opacity).toBe(1)
            expect(pair.ratio, `${theme} ChoiceColumn "${text}" (selected=${String(selected)}, ${state})`).toBeGreaterThanOrEqual(4.5)
          }
        }
      }
    }
  })

  test(`${skin}/${theme}: Records filter count and DecisionTree numbers/options/Agent's pick pass 4.5:1 without opacity dimming`, () => {
    THEME_TOKENS = COMPONENT_TOKENS[skin]
    // 1. Records filter count badge (resting on background and hovered inside ghost button on accent).
    const filterCountClass = /Filter\{filterCount > 0 && <span className="([^"]+)"/.exec(recordsGridSource)?.[1]
    expect(filterCountClass, "filter count span class in records-grid.tsx").toBeDefined()
    for (const host of ["background", "accent"] as const) {
      const pair = resolveClasses(filterCountClass!, theme, "rest", tokenHex(theme, host))
      expect(pair.ratio, `${theme} Records filter count on ${host} (${filterCountClass})`).toBeGreaterThanOrEqual(4.5)
    }

    // 2. DecisionTree selected & unselected option numbers.
    const numberMatch = /const OptionNumber[\s\S]*?on \? "([^"]+)" : "([^"]+)"/.exec(decisionTreeSource)
    expect(numberMatch, "OptionNumber classes in decision-tree.tsx").toBeTruthy()
    for (const [label, cls] of [["selected", numberMatch![1]!], ["unselected", numberMatch![2]!]] as const) {
      const pair = resolveClasses(cls, theme, "rest", tokenHex(theme, "card"))
      expect(pair.ratio, `${theme} DecisionTree OptionNumber (${label})`).toBeGreaterThanOrEqual(4.5)
    }

    // 3. DecisionTree Agent's pick pill on both card and unselected surface-subtle.
    const agentPickClass = /\{agentPick && <span className="([^"]+)"/.exec(decisionTreeSource)?.[1]
    expect(agentPickClass, "Agent's pick span class in decision-tree.tsx").toBeDefined()
    for (const host of ["card", "surface-subtle"] as const) {
      const pair = resolveClasses(agentPickClass!, theme, "rest", tokenHex(theme, host))
      expect(pair.ratio, `${theme} DecisionTree Agent's pick on ${host}`).toBeGreaterThanOrEqual(4.5)
    }

    // 4. DecisionTree option button stays opaque when unselected and uses dedicated focus-visible outline.
    const optionButton = /aria-pressed=\{active\}[\s\S]*?className=\{cn\(([\s\S]*?)\)\}>/.exec(decisionTreeSource)?.[1]
    expect(optionButton, "DecisionTree option button className").toBeDefined()
    expect(optionButton).not.toMatch(/\bopacity-\d+\b/)
    expect(optionButton).toContain("focus-visible:outline-2")
    expect(optionButton).toContain("focus-visible:outline-solid")
    expect(optionButton).toContain("focus-visible:outline-ring")
  })

  test(`${skin}/${theme}: selected Tabs, applet rail buttons and menu rows keep dedicated focus outlines and >= 4.5:1 active text`, () => {
    THEME_TOKENS = COMPONENT_TOKENS[skin]
    // 1. Rendered TabsTrigger (both default and line variants).
    for (const variant of ["default", "line"] as const) {
      const html = renderToStaticMarkup(
        createElement(Tabs, { defaultValue: "overview" },
          createElement(TabsList, { variant },
            createElement(TabsTrigger, { value: "overview" }, "Overview"),
            createElement(TabsTrigger, { value: "lineage" }, "Lineage"),
          )),
      )
      const triggerClass = extractClass(html, /data-slot="tabs-trigger"[^>]*class="([^"]+)"/)
      expect(triggerClass).toContain("focus-visible:outline-2")
      expect(triggerClass).toContain("focus-visible:outline-solid")
      expect(triggerClass).toContain("focus-visible:outline-ring")
      expect(triggerClass).not.toContain("focus-visible:outline-none")
      expect(triggerClass).not.toMatch(/\bfocus-visible:ring-/)
      // Resolve the group variant that matches the rendered TabsList.
      const hostHex = tokenHex(theme, "background")
      expect(html).toContain(`data-variant="${variant}"`)
      const effectiveClass = triggerClass.replaceAll(`group-data-[variant=${variant}]/tabs-list:`, "")
      const active = resolveClasses(effectiveClass, theme, "selected", hostHex)
      expect(active.bg).toBe(variant === "default" && theme === "dark" ? tokenHex(theme, "accent") : hostHex)
      expect(active.ratio, `${theme} TabsTrigger[${variant}] selected`).toBeGreaterThanOrEqual(4.5)
    }
    expect(tabsSource).toContain("group-data-[variant=line]/tabs-list:focus-visible:-outline-offset-2")

    // 2. Shell controls whose selection/border uses ring-* must use dedicated focus-visible outline.
    const railButtonClass = /const railButton = "([^"]+)"/.exec(shellNavSource)?.[1]
    const focusRowClass = /const focusRow = "([^"]+)"/.exec(shellNavSource)?.[1]
    expect(railButtonClass).toBeDefined()
    expect(focusRowClass).toBeDefined()
    for (const [name, cls] of [["railButton", railButtonClass!], ["focusRow", focusRowClass!]] as const) {
      expect(cls, `${name} must use focus-visible outline, not ring`).not.toMatch(/focus-visible:(?:\[[^\]]+\]:)?ring-/)
      expect(cls, `${name} outline width`).toContain("outline-2")
      expect(cls, `${name} outline style`).toContain("outline-solid")
      expect(cls, `${name} outline color`).toContain("outline-ring")
      expect(cls, `${name} outline offset`).toMatch(/outline-offset-/)
    }
    // The rail replaced the perspective switch: there is no second control for the same choice.
    expect(shellSource).not.toMatch(/function PerspectiveSwitch/)
  })
}

for (const skin of ALL_SKINS) for (const theme of ["light", "dark"] as const) {
  test(`${skin}/${theme}: every rendered Chip tone and Badge variant passes 4.5:1 using its actual declared fill and hover classes`, () => {
    THEME_TOKENS = FLAT_TOKENS[skin]
    for (const tone of TONES) {
      const html = renderToStaticMarkup(createElement(Chip, { tone, children: "Label" }))
      const classes = extractClass(html, /data-slot="chip"[^>]*class="([^"]+)"/)
      for (const host of ["background", "card", "surface-subtle"] as const) {
        const pair = resolveClasses(classes, theme, "rest", tokenHex(theme, host))
        expect(pair.ratio, `${theme} Chip[${tone}] on ${host} (${classes})`).toBeGreaterThanOrEqual(4.5)
      }
    }
    for (const variant of BADGE_VARIANTS) {
      const classes = badgeVariants({ variant })
      for (const host of ["background", "card"] as const) {
        const hostHex = tokenHex(theme, host)
        const rest = resolveClasses(classes, theme, "rest", hostHex)
        const hover = resolveClasses(classes, theme, "hover", hostHex)
        expect(rest.ratio, `${theme} Badge[${variant}] rest on ${host}`).toBeGreaterThanOrEqual(4.5)
        expect(hover.ratio, `${theme} Badge[${variant}] hover on ${host}`).toBeGreaterThanOrEqual(4.5)
      }
    }
    for (const kind of KIND_KEYS) {
      const classes = `${KINDS[kind].text} ${KINDS[kind].fill}`
      for (const host of ["background", "card"] as const) {
        const hostHex = tokenHex(theme, host)
        expect(resolveClasses(KINDS[kind].text, theme, "rest", hostHex).ratio, `${theme} KINDS[${kind}] text on ${host}`).toBeGreaterThanOrEqual(4.5)
        expect(resolveClasses(classes, theme, "rest", hostHex).ratio, `${theme} KINDS[${kind}] tinted on ${host}`).toBeGreaterThanOrEqual(4.5)
      }
    }
    for (const status of STATUS_KEYS) {
      for (const host of ["background", "card"] as const) {
        expect(resolveClasses(STATUS[status].text, theme, "rest", tokenHex(theme, host)).ratio, `${theme} STATUS[${status}] on ${host}`).toBeGreaterThanOrEqual(4.5)
      }
    }
  })

  test(`${skin}/${theme}: the records toolbar's "Unapplied changes" dot, which has no word, keeps 3:1 on its button at rest and on hover`, () => {
    THEME_TOKENS = FLAT_TOKENS[skin]
    const dot = /\{draftDirty && <span className="([^"]+)" aria-label="Unapplied changes"/.exec(recordsGridSource)?.[1]
    expect(dot, "Unapplied changes dot in records-grid.tsx").toBeDefined()
    const fill = dot!.split(/\s+/).map((token) => parseColorUtility(theme, token, "bg")).find((spec) => spec !== undefined && spec !== "transparent")
    expect(fill, `the dot's fill (${dot})`).toBeDefined()
    const button = buttonVariants({ variant: "ghost" })
    for (const host of ["background", "card"] as const) {
      for (const state of ["rest", "hover"] as const) {
        const surface = resolveClasses(button, theme, state, tokenHex(theme, host)).bg
        const ink = mix((fill as ColorSpec).hex, surface, (fill as ColorSpec).alpha)
        expect(ratio(ink, surface), `${skin}/${theme} dot ${dot} on the ghost button (${state}) over ${host}`).toBeGreaterThanOrEqual(3)
      }
    }
  })
}

/** Resolve a hex or `rgb(r g b / a)` color value over a backing hex color. */
function resolveSurfaceColor(value: string, backdropHex: string): string {
  if (/^#[0-9a-fA-F]{6}$/.test(value)) return value
  const rgbAlpha = /^rgb\(\s*(\d+)\s+(\d+)\s+(\d+)\s*\/\s*([\d.]+)\s*\)$/.exec(value)
  if (rgbAlpha) {
    const fg = "#" + [1, 2, 3].map((i) => Number(rgbAlpha[i]!).toString(16).padStart(2, "0")).join("")
    return mix(fg, backdropHex, Number(rgbAlpha[4]!))
  }
  throw new Error(`unsupported color format in skin tokens: ${value}`)
}

const SKIN_SELECTORS: readonly { readonly skin: "open" | "polar" | "luma" | "glass"; readonly theme: Theme }[] = [
  { skin: "open", theme: "light" },
  { skin: "open", theme: "dark" },
  { skin: "polar", theme: "light" },
  { skin: "polar", theme: "dark" },
  { skin: "luma", theme: "light" },
  { skin: "luma", theme: "dark" },
  { skin: "glass", theme: "light" },
  { skin: "glass", theme: "dark" },
]

for (const { skin, theme } of SKIN_SELECTORS) {
  test(`skin=${skin} (${theme}): composited surfaces, foreground, muted-foreground, primary button, and focus ring pass WCAG AA`, () => {
    const merged: Readonly<Record<string, string>> = skinVars(css, skin, theme)
    const resolveVar = (name: string, depth = 0): string => {
      if (depth > 8) throw new Error(`cyclic skin token --${name}`)
      const raw = merged[name]
      if (!raw) throw new Error(`missing token --${name} in ${skin}/${theme}`)
      const alias = /^var\(--([a-z0-9-]+)\)$/.exec(raw)
      return alias ? resolveVar(alias[1]!, depth + 1) : raw
    }

    const shellHex = resolveSurfaceColor(resolveVar("shell"), theme === "dark" ? "#0b0b0e" : "#ffffff")
    const bgSurfaceHex = resolveSurfaceColor(resolveVar("background"), shellHex)
    if (theme === "dark") {
      expect(luminance(shellHex), `${skin}/${theme} shell must stay dark`).toBeLessThan(0.08)
      expect(luminance(bgSurfaceHex), `${skin}/${theme} background must stay dark`).toBeLessThan(0.08)
    }
    const fgHex = resolveSurfaceColor(resolveVar("foreground"), shellHex)
    const mutedFgHex = resolveSurfaceColor(resolveVar("muted-foreground"), shellHex)
    const primaryBgHex = resolveSurfaceColor(resolveVar("primary"), shellHex)
    const primaryFgHex = resolveSurfaceColor(resolveVar("primary-foreground"), primaryBgHex)
    const ringHex = resolveSurfaceColor(resolveVar("ring"), shellHex)

    if (skin === "polar") {
      const [pr, pg, pb] = rgbOf(primaryBgHex)
      expect(Math.max(pr, pg, pb) - Math.min(pr, pg, pb), `polar/${theme} primary must stay monochrome, not violet`).toBeLessThanOrEqual(8)
    }

    expect(ratio(primaryFgHex, primaryBgHex), `${skin}/${theme} primary button`).toBeGreaterThanOrEqual(4.5)

    for (const surface of ["shell", "background", "card", "surface-subtle", "muted", "popover", "accent"] as const) {
      const bgHex = resolveSurfaceColor(resolveVar(surface), shellHex)
      expect(ratio(fgHex, bgHex), `${skin}/${theme} foreground on ${surface} (${fgHex} on ${bgHex})`).toBeGreaterThanOrEqual(4.5)
      expect(ratio(mutedFgHex, bgHex), `${skin}/${theme} muted-foreground on ${surface} (${mutedFgHex} on ${bgHex})`).toBeGreaterThanOrEqual(4.5)
    }

    for (const surface of ["shell", "background", "card", "popover"] as const) {
      const bgHex = resolveSurfaceColor(resolveVar(surface), shellHex)
      expect(ratio(ringHex, bgHex), `${skin}/${theme} focus ring on ${surface} (${ringHex} on ${bgHex})`).toBeGreaterThanOrEqual(3)
    }

    const dockSurfaceHex = resolveSurfaceColor(resolveVar("dock-surface"), shellHex)
    const dockRingHex = resolveSurfaceColor(resolveVar("dock-ring"), dockSurfaceHex)
    const dockIndicatorHex = resolveSurfaceColor(resolveVar("dock-indicator"), dockSurfaceHex)
    expect(ratio(dockRingHex, dockSurfaceHex), `${skin}/${theme} dock focus ring (${dockRingHex} on ${dockSurfaceHex})`).toBeGreaterThanOrEqual(3)
    expect(ratio(dockIndicatorHex, dockSurfaceHex), `${skin}/${theme} dock unread indicator (${dockIndicatorHex} on ${dockSurfaceHex})`).toBeGreaterThanOrEqual(3)
  })
}

test("shell never duplicates data-skin inside the document body, keeping :root.dark[data-skin=...] authoritative", () => {
  expect(shellSource).not.toMatch(/data-skin=\{/)
})

/**
 * DESIGN.md 4.6 and 4.7: the islands' measured pairs in every skin and scheme, composited the same way
 * as section 4.2 (8 bits per layer), over the backdrop at its worst case: flat, under either ambient
 * glow at its peak, and under both. The nav islands (rail and submenu) are `--sidebar` over that
 * backdrop, with the hover wash (`--sidebar-accent` at 50%) and the pill (`--sidebar-accent`) on it;
 * the main island is `--island`, with a card and a well inside it. The phone sheet is opaque `--shell`.
 * The reduced-transparency fallback paints `--shell` and `--canvas`, measured by the canvas pairs.
 */
const PALETTES = themePalettes(css)
function backdrops(color: (token: string) => Color): readonly (readonly [string, Color])[] {
  const flat = color("backdrop")
  const glow1 = over(color("backdrop-glow-1"), flat)
  return [["backdrop", flat], ["glow 1", glow1], ["glow 2", over(color("backdrop-glow-2"), flat)], ["both glows", over(color("backdrop-glow-2"), glow1)]]
}
for (const skin of ["luma", "polar", "open", "glass"] as const) for (const theme of ["light", "dark"] as const) {
  const palette = PALETTES[skin][theme]
  const color = (token: string): Color => {
    const parsed = parseColor(palette[token as keyof typeof palette]!)
    expect(parsed, `${skin}/${theme} --${token}`).not.toBeNull()
    return parsed!
  }
  test(`skin=${skin} (${theme}): nav islands' ground, 50% hover wash and selection pill pass DESIGN.md 4.6 over the worst-case backdrop (text 4.5:1, ring and dot 3:1)`, () => {
    const grounds = [...backdrops(color).map(([where, backdrop]) => [`nav island over ${where}`, over(color("sidebar"), backdrop)] as const), ["phone sheet", over(color("shell"), color("canvas"))] as const]
    for (const [where, ground] of grounds) {
      const hover = over(fade(color("sidebar-accent"), 0.5), ground)
      const pill = over(color("sidebar-accent"), ground)
      const at = (label: string) => `${skin}/${theme} ${where}: ${label}`
      // Resting rows, group headers, counts and the applet icons: muted ink on the ground and on the hover wash.
      expect(contrastOn(color("muted-foreground"), ground), at("muted on ground")).toBeGreaterThanOrEqual(4.5)
      expect(contrastOn(color("muted-foreground"), hover), at("muted on hover")).toBeGreaterThanOrEqual(4.5)
      // The current row and the current applet: label, icon and count all take the pill's ink.
      expect(contrastOn(color("sidebar-accent-foreground"), pill), at("pill ink on pill")).toBeGreaterThanOrEqual(4.5)
      // The applet's name in the submenu header, the workspace and the principal.
      expect(contrastOn(color("sidebar-foreground"), ground), at("names on ground")).toBeGreaterThanOrEqual(4.5)
      expect(contrastOn(color("sidebar-foreground"), hover), at("names on hover")).toBeGreaterThanOrEqual(4.5)
      expect(contrastOn(color("foreground"), ground), at("applet name on ground")).toBeGreaterThanOrEqual(4.5)
      // The focus ring and the attention dot (also `--ring`): 3:1 on the ground, the wash and the pill.
      for (const [label, surface] of [["ground", ground], ["hover", hover], ["pill", pill]] as const) {
        expect(contrastOn(color("ring"), surface), at(`ring/dot on ${label}`)).toBeGreaterThanOrEqual(3)
      }
    }
  })
  test(`skin=${skin} (${theme}): the main island's text, links, ring, cards and wells pass DESIGN.md 4.2 over the worst-case backdrop`, () => {
    for (const [where, backdrop] of backdrops(color)) {
      const island = over(color("island"), backdrop)
      const card = over(color("card"), island)
      const well = over(color("well"), card)
      const hover = over(color("well-hover"), well)
      const at = (label: string) => `${skin}/${theme} main island over ${where}: ${label}`
      for (const [name, surface] of [["island", island], ["card", card], ["well", well], ["well hover", hover]] as const) {
        for (const ink of ["foreground", "muted-foreground", "action", "link"] as const) {
          expect(contrastOn(color(ink), surface), at(`${ink} on ${name}`)).toBeGreaterThanOrEqual(4.5)
        }
        expect(contrastOn(color("ring"), surface), at(`ring on ${name}`)).toBeGreaterThanOrEqual(3)
      }
      // The top bar's icon buttons and the search field's text are muted ink on the island itself.
      expect(contrastOn(color("subtle-foreground"), island), at("decorative glyphs on island")).toBeGreaterThanOrEqual(3)
    }
  })
}

test("the islands' glass falls back to opaque surfaces under reduced transparency, without backdrop-filter, and in forced colours", () => {
  const block = (query: string) => {
    const at = css.indexOf(query)
    expect(at, query).toBeGreaterThan(-1)
    return css.slice(at, css.indexOf("\n  }\n", at))
  }
  for (const query of ["@media (prefers-reduced-transparency: reduce)", "@supports not ((backdrop-filter: blur(1px)) or (-webkit-backdrop-filter: blur(1px)))"]) {
    const rules = block(query)
    expect(rules, query).toContain('[data-island="nav"] { background-color: var(--shell); }')
    expect(rules, query).toContain('[data-island="main"] { background-color: var(--canvas); }')
  }
  expect(block("@media (prefers-reduced-transparency: reduce)")).toMatch(/backdrop-filter: none/)
  expect(block("@media (forced-colors: active)")).toMatch(/backdrop-filter: none[\s\S]*border-color: CanvasText/)
  // The glass is CSS only: no WebGL or refraction library paints it.
  expect(shellSource + shellNavSource).not.toMatch(/webgl|WebGL|liquid-glass|refraction/)
})

test("menu rows and applet buttons: a 50% wash on hover with no text change, the neutral pill with its own ink at 400 when current, and no ring, bar or accent", () => {
  const row = /const row = "([^"]+)"/.exec(shellNavSource)?.[1] ?? ""
  const rail = /const railButton = "([^"]+)"/.exec(shellNavSource)?.[1] ?? ""
  const groupHeading = /const groupHeading = "([^"]+)"/.exec(shellNavSource)?.[1] ?? ""
  for (const [name, cls, current] of [["row", row, "data-current"], ["railButton", rail, "aria-[current=page]"]] as const) {
    expect(cls, name).toContain("hover:bg-sidebar-accent/50")
    expect(cls, `${name} keeps its text colour on hover`).not.toMatch(/hover:text-/)
    expect(cls, name).toContain(`${current}:bg-sidebar-accent`)
    expect(cls, name).toContain(`${current}:text-sidebar-accent-foreground`)
    const whenCurrent = cls.split(/\s+/).filter((token) => token.startsWith(`${current}:`)).map((token) => token.slice(current.length + 1))
    expect(whenCurrent.sort(), `${name}: the current row is only the pill and its ink (no ring, bar, shadow, weight or accent)`).toEqual(["bg-sidebar-accent", "text-sidebar-accent-foreground"])
    expect(cls, `${name} size`).toContain(name === "row" ? "h-[32px]" : "size-[40px]")
    expect(cls, `${name} phone target`).toContain(name === "row" ? "max-sm:min-h-[44px]" : "max-sm:size-[44px]")
    expect(cls, `${name} weight`).not.toMatch(/font-(medium|semibold|bold)/)
  }
  // A pressed control (the assistant, and the applet whose menu the phone sheet shows) takes the wash, never the pill.
  const whenPressed = rail.split(/\s+/).filter((token) => token.startsWith("aria-pressed:")).map((token) => token.slice("aria-pressed:".length))
  expect(whenPressed).toEqual(["bg-sidebar-accent/50"])
  expect(shellNavSource).toMatch(/aria-label=\{title\} aria-pressed=\{open\} onClick=\{toggle\} className=\{railButton\}/)
  expect(row).toContain("text-body")
  expect(groupHeading).toContain("text-meta")
  expect(groupHeading).toContain("h-[32px]")
  expect(groupHeading).not.toMatch(/uppercase|tracking-|font-semibold/)
  for (const skin of OPAQUE_SKINS) for (const theme of ["light", "dark"] as const) {
    THEME_TOKENS = SKIN_TOKENS[skin]
    const ground = tokenHex(theme, "sidebar")
    for (const state of ["rest", "hover", "selected"] as const) {
      const pair = resolveClasses(row, theme, state, ground, tokenHex(theme, "muted-foreground"))
      expect(pair.ratio, `${skin}/${theme} row ${state} (${pair.fg} on ${pair.bg})`).toBeGreaterThanOrEqual(4.5)
    }
  }
})

test("cn keeps a type-scale step beside a text colour, leading and tracking in either order, and a later size replaces an earlier one", async () => {
  const { cn, TYPE_SCALE } = await import("../src/lib/utils.ts")
  expect(cn("text-meta text-muted-foreground")).toBe("text-meta text-muted-foreground")
  expect(cn("font-heading text-heading font-medium text-foreground", "[overflow-wrap:break-word]")).toContain("text-heading")
  for (const step of TYPE_SCALE) {
    const size = `text-${step}`
    // Colour, leading and tracking are other properties: they survive beside a step, before or after it.
    for (const other of ["text-foreground", "text-muted-foreground", "leading-snug", "leading-tight", "leading-[1.2]", "tracking-tight", "tracking-[-0.01em]"]) {
      expect(cn(other, size), `${other} then ${size}`).toBe(`${other} ${size}`)
      expect(cn(size, other), `${size} then ${other}`).toBe(`${size} ${other}`)
    }
    // Sizes conflict both ways: a later step replaces an earlier Tailwind size, and the reverse.
    expect(cn("text-sm", size)).toBe(size)
    expect(cn(size, "text-sm")).toBe("text-sm")
  }
  expect(cn("text-body", "text-meta")).toBe("text-meta")
  expect(cn("text-figure", "text-title")).toBe("text-title")
  expect(cn("text-muted-foreground", "text-foreground")).toBe("text-foreground")
  // The live case: the start node's label keeps its leading beside the step.
  expect(cn("font-medium leading-snug", "text-body")).toBe("font-medium leading-snug text-body")
})
