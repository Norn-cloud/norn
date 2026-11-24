import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Chip, type Tone } from "../src/components/chip.tsx"
import { ChoiceColumn } from "../src/components/decision/choice-columns.tsx"
import { badgeVariants } from "../src/components/ui/badge.tsx"
import { buttonVariants } from "../src/components/ui/button.tsx"
import { Tabs, TabsList, TabsTrigger } from "../src/components/ui/tabs.tsx"
import { KINDS, STATUS, type Kind, type Status } from "../src/lib/kinds.ts"

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

function parseCssBlock(selector: ":root" | ".dark"): Readonly<Record<string, string>> {
  const pattern = selector === ":root" ? /:root\s*\{([^}]+)\}/ : /\.dark\s*\{([^}]+)\}/
  const body = pattern.exec(css)?.[1]
  if (!body) throw new Error(`missing ${selector} block in index.css`)
  const raw: Record<string, string> = {}
  for (const [, name, value] of body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)) {
    raw[name!] = value!.trim()
  }
  const resolved: Record<string, string> = {}
  const resolve = (name: string, depth = 0): string => {
    if (resolved[name] !== undefined) return resolved[name]!
    if (depth > 8) throw new Error(`cyclic CSS token --${name}`)
    const value = raw[name]
    if (value === undefined) throw new Error(`unknown CSS token --${name} in ${selector}`)
    const alias = /^var\(--([a-z0-9-]+)\)$/.exec(value)
    const out = alias ? resolve(alias[1]!, depth + 1) : value
    resolved[name] = out
    return out
  }
  for (const name of Object.keys(raw)) resolve(name)
  return resolved
}

const THEME_TOKENS: Readonly<Record<Theme, Readonly<Record<string, string>>>> = {
  light: parseCssBlock(":root"),
  dark: parseCssBlock(".dark"),
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

const SELECTED_VARIANTS = new Set(["data-active", "aria-pressed", "data-[status=active]", "data-current"])
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
const tabsSource = readSrc("components/ui/tabs.tsx")
const recordsGridSource = readSrc("grid/records-grid.tsx")
const decisionTreeSource = readSrc("decision-tree.tsx")

for (const theme of ["light", "dark"] as const) {
  test(`${theme}: surfaces stay distinct, text tokens pass 4.5:1, and edges/focus ring pass 3:1`, () => {
    expect(tokenHex(theme, "shell")).not.toBe(tokenHex(theme, "background"))
    expect(tokenHex(theme, "card")).not.toBe(tokenHex(theme, "background"))
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

  test(`${theme}: every Button variant passes 4.5:1 at rest and on hover across real hosting surfaces, with dedicated focus outline and flat disabled state`, () => {
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

  test(`${theme}: every rendered Chip tone and Badge variant passes 4.5:1 using its actual declared fill and hover classes`, () => {
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

  test(`${theme}: ChoiceColumn stays opaque on enabled unselected options and passes 4.5:1 across resting, selected, unselected and hover states`, () => {
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

  test(`${theme}: Records filter count and DecisionTree numbers/options/Agent's pick pass 4.5:1 without opacity dimming`, () => {
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

  test(`${theme}: selected Tabs, Perspective switch, rail links and sidebar rows keep dedicated focus outlines and >= 4.5:1 active text`, () => {
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
    const railButtonClass = /const railButton = "([^"]+)"/.exec(shellSource)?.[1]
    const focusRowClass = /const focusRow = "([^"]+)"/.exec(shellSource)?.[1]
    const perspectiveClass = /function PerspectiveSwitch[\s\S]*?aria-pressed=\{perspective === id\}[\s\S]*?className="([^"]+)"/.exec(shellSource)?.[1]
    expect(railButtonClass).toBeDefined()
    expect(focusRowClass).toBeDefined()
    expect(perspectiveClass).toBeDefined()
    for (const [name, cls] of [["railButton", railButtonClass!], ["focusRow", focusRowClass!], ["PerspectiveSwitch", perspectiveClass!]] as const) {
      expect(cls, `${name} must use focus-visible outline, not ring`).not.toMatch(/focus-visible:(?:\[[^\]]+\]:)?ring-/)
      expect(cls, `${name} outline width`).toContain("outline-2")
      expect(cls, `${name} outline style`).toContain("outline-solid")
      expect(cls, `${name} outline color`).toContain("outline-ring")
      expect(cls, `${name} outline offset`).toMatch(/outline-offset-/)
    }
    // PerspectiveSwitch active & inactive text contrast on its track.
    const trackHex = tokenHex(theme, theme === "dark" ? "shell" : "surface-subtle")
    expect(resolveClasses(perspectiveClass!, theme, "selected", trackHex).ratio, `${theme} PerspectiveSwitch selected`).toBeGreaterThanOrEqual(4.5)
    expect(resolveClasses(perspectiveClass!, theme, "rest", trackHex).ratio, `${theme} PerspectiveSwitch resting`).toBeGreaterThanOrEqual(4.5)
  })
}
