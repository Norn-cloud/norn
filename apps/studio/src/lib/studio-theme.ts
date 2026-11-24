import type { Skin } from "./appearance.tsx"
import { canonicalColor, contrastOn, fade, over, parseColor, type Color } from "./color.ts"
import PALETTES from "./theme-palettes.json"

/**
 * Semantic CSS custom properties a custom StudioTheme JSON may override.
 * Strictly allowlisted so custom themes restyle visual tokens only — never
 * layout geometry, z-index, or arbitrary CSS injection.
 */
export const STUDIO_THEME_TOKENS = [
  "shell",
  "canvas",
  "background",
  "foreground",
  "card",
  "card-foreground",
  "well",
  "well-border",
  "well-hover",
  "popover",
  "popover-foreground",
  "primary",
  "primary-foreground",
  "action",
  "action-hover",
  "action-foreground",
  "secondary",
  "secondary-foreground",
  "muted",
  "muted-foreground",
  "accent",
  "accent-foreground",
  "surface-subtle",
  "border",
  "input",
  "ring",
  "link",
  "sidebar",
  "sidebar-foreground",
  "sidebar-primary",
  "sidebar-primary-foreground",
  "sidebar-accent",
  "sidebar-accent-foreground",
  "sidebar-border",
  "sidebar-ring",
  "panel-border",
  "table-border",
  "dock-surface",
  "dock-border",
  "dock-well",
  "dock-foreground",
  "dock-muted",
  "dock-active",
  "dock-ring",
  "dock-indicator",
  "knot",
  "destructive",
  "destructive-foreground",
  "destructive-text",
  "success",
  "warning",
  "chart-1",
  "chart-2",
  "chart-3",
  "chart-4",
  "chart-5",
] as const

export type StudioThemeToken = (typeof STUDIO_THEME_TOKENS)[number]
export type StudioThemeTokenMap = Partial<Record<StudioThemeToken, string>>

/**
 * VS Code / T3 Code-style declarative JSON theme definition for Norn Studio.
 * Large structural regions stay monochrome by default; tenants or users can
 * override any allowlisted semantic token per light/dark scheme, gated by WCAG AA.
 */
export interface StudioTheme {
  readonly version: 1
  readonly name: string
  readonly baseSkin?: Skin
  readonly appearance?: "light" | "dark"
  /** Shorthand override for `--action`, `--action-hover`, `--primary`, `--ring`, and `--link` (with auto-contrasting foreground). */
  readonly accent?: string
  /** Shorthand override for `--canvas` and `--shell`. */
  readonly canvas?: string
  readonly colors?: StudioThemeTokenMap
  readonly variants?: {
    readonly light?: StudioThemeTokenMap
    readonly dark?: StudioThemeTokenMap
  }
}

export interface StudioThemeValidationIssue {
  readonly path: string
  readonly message: string
}

export type StudioThemeParseResult =
  | { readonly ok: true; readonly theme: StudioTheme; readonly issues: readonly [] }
  | { readonly ok: false; readonly issues: readonly StudioThemeValidationIssue[] }

const ALLOWED_TOKENS = new Set<string>(STUDIO_THEME_TOKENS)
const VALID_SKINS = new Set<string>(["polar", "open", "luma", "glass"])
const CUSTOM_THEME_KEY = "norn.studio.customTheme"
const APPLIED_TOKENS_ATTR = "customThemeTokens"

/** Effective built-in base skin used identically for validation and rendering. */
export const effectiveBaseSkin = (theme: Pick<StudioTheme, "baseSkin">): Skin => theme.baseSkin ?? "polar"

/**
 * Every built-in skin's painted palette, generated from `index.css` by `scripts/theme-palettes.ts`
 * (`test/theme-palettes.test.ts` fails on drift), so the gate validates the colours Studio renders.
 */
const BASE_PALETTES = PALETTES as unknown as Record<Skin, Record<"light" | "dark", Record<StudioThemeToken, string>>>

/** Curated starter JSON themes users can apply, inspect, customize, or export. */
export const STUDIO_THEME_PRESETS: readonly StudioTheme[] = [
  {
    version: 1,
    name: "Polar Monochrome",
    baseSkin: "polar",
    variants: {
      light: {
        canvas: "#f6f6f8",
        background: "#ffffff",
        card: "#ffffff",
        foreground: "#18181b",
        "muted-foreground": "#585861",
        primary: "#18181b",
        "primary-foreground": "#fafafa",
        action: "#18181b",
        "action-hover": "#2c2c32",
        "action-foreground": "#fafafa",
        ring: "#52525b",
      },
      dark: {
        canvas: "#0a0a0b",
        background: "#111113",
        card: "#161618",
        foreground: "#fafafa",
        "muted-foreground": "#9d9da6",
        primary: "#f4f4f5",
        "primary-foreground": "#141416",
        action: "#f4f4f5",
        "action-hover": "#e4e4e7",
        "action-foreground": "#141416",
        ring: "#a1a1aa",
      },
    },
  },
  {
    version: 1,
    name: "Graphite Terminal",
    baseSkin: "polar",
    variants: {
      light: {
        shell: "#f3f4f6",
        canvas: "#f3f4f6",
        background: "#fcfcfd",
        card: "#ffffff",
        foreground: "#111318",
        "muted-foreground": "#4f5561",
        primary: "#111318",
        "primary-foreground": "#f8fafc",
        action: "#111318",
        "action-hover": "#232833",
        "action-foreground": "#f8fafc",
        ring: "#3f4756",
        "chart-1": "#111318",
        "chart-2": "#0f726a",
      },
      dark: {
        shell: "#08090c",
        canvas: "#08090c",
        background: "#0f1117",
        card: "#141720",
        well: "#1a1e29",
        foreground: "#f1f5f9",
        "muted-foreground": "#94a3b8",
        primary: "#f1f5f9",
        "primary-foreground": "#0f1117",
        action: "#f1f5f9",
        "action-hover": "#dbe2ec",
        "action-foreground": "#0f1117",
        border: "#232838",
        ring: "#94a3b8",
        "chart-1": "#3ccf8e",
        "chart-2": "#60a5fa",
      },
    },
  },
  {
    version: 1,
    name: "Paper Editorial",
    baseSkin: "polar",
    variants: {
      light: {
        shell: "#f4f2ed",
        canvas: "#f4f2ed",
        background: "#fcfbf8",
        card: "#fcfbf8",
        well: "#f2efe9",
        foreground: "#1c1917",
        "muted-foreground": "#57534e",
        primary: "#1c1917",
        "primary-foreground": "#fafaf9",
        action: "#1c1917",
        "action-hover": "#2f2a26",
        "action-foreground": "#fafaf9",
        border: "#e5e1d8",
        ring: "#57534e",
      },
      dark: {
        shell: "#121110",
        canvas: "#121110",
        background: "#191715",
        card: "#201d1a",
        well: "#27231f",
        foreground: "#f5f2eb",
        "muted-foreground": "#a8a29e",
        primary: "#f5f2eb",
        "primary-foreground": "#191715",
        action: "#f5f2eb",
        "action-hover": "#e7e2d7",
        "action-foreground": "#191715",
        border: "#302b26",
        ring: "#a8a29e",
      },
    },
  },
]

const OPAQUE_LIGHT = parseColor("#ffffff")!
const OPAQUE_DARK = parseColor("#000000")!
const DARK_INK = parseColor("#141416")!
const LIGHT_INK = parseColor("#fafafa")!

/** Pick crisp dark or light foreground ink for a shorthand `accent` override. */
function autoInkFor(color: Color): string {
  return contrastOn(LIGHT_INK, color) >= contrastOn(DARK_INK, color) ? "#fafafa" : "#141416"
}

/** A theme colour in canonical CSS (`#rrggbb` or `rgba(…)`), or null when it isn't a CSS colour. */
function themeColor(value: unknown): string | null {
  return typeof value === "string" ? canonicalColor(value) : null
}

function validateTokenMap(
  input: unknown,
  pathPrefix: string,
  issues: StudioThemeValidationIssue[],
): StudioThemeTokenMap {
  if (input === undefined) return {}
  if (typeof input !== "object" || input === null || Array.isArray(input)) {
    issues.push({ path: pathPrefix, message: "Expected an object mapping semantic token names to CSS colors." })
    return {}
  }
  const clean: Record<string, string> = {}
  for (const [rawKey, rawVal] of Object.entries(input)) {
    const key = rawKey.startsWith("--") ? rawKey.slice(2) : rawKey
    if (!ALLOWED_TOKENS.has(key)) {
      issues.push({ path: `${pathPrefix}.${rawKey}`, message: `Unknown or disallowed theme token "${rawKey}".` })
      continue
    }
    const color = themeColor(rawVal)
    if (color === null) {
      issues.push({ path: `${pathPrefix}.${key}`, message: `Invalid color value for "${key}". Use a CSS color such as #rrggbb, rgb(…), hsl(…) or oklch(…).` })
      continue
    }
    clean[key] = color
  }
  return clean
}

/** Compute the effective CSS custom property overrides a StudioTheme writes for one light/dark scheme. */
export function resolveCustomThemeTokens(theme: StudioTheme, scheme: "light" | "dark"): Record<string, string> {
  const overrides: Record<string, string> = {}
  if (theme.canvas) {
    overrides.canvas = theme.canvas
    overrides.shell = theme.canvas
  }
  if (theme.accent) {
    const parsed = parseColor(theme.accent)
    const ink = parsed ? autoInkFor(parsed) : "#ffffff"
    overrides.primary = theme.accent
    overrides["primary-foreground"] = ink
    overrides.action = theme.accent
    overrides["action-hover"] = theme.accent
    overrides["action-foreground"] = ink
    overrides.ring = theme.accent
    overrides.link = theme.accent
  }
  if (theme.colors) Object.assign(overrides, theme.colors)
  const variant = theme.variants?.[scheme]
  if (variant) Object.assign(overrides, variant)
  return overrides
}

/**
 * One surface as Studio paints it: its layers from the bottom up, each a token at an optional
 * alpha (Tailwind's `/p` modifier, per scheme when `dark:` changes it), over the scheme's canvas.
 */
type Alpha = number | { readonly light: number; readonly dark: number }
type Surface = readonly (StudioThemeToken | readonly [StudioThemeToken, Alpha])[]

const SHELL: Surface = ["canvas", "shell"]
const BG: Surface = [...SHELL, "background"]
const CARD: Surface = [...BG, "card"]
const POPOVER: Surface = [...BG, "popover"]
const SIDEBAR: Surface = [...SHELL, "sidebar"]
const DOCK: Surface = [...BG, "dock-surface"]

/**
 * The ink/surface pairs Studio renders (from the components' `bg-*`/`text-*` pairs), each with its
 * WCAG floor: 4.5:1 for text, 3:1 for focus rings and status icons. A partial-alpha fill sits
 * between its host and the full fill, so gating both ends covers it.
 */
const PAIRS: readonly (readonly [StudioThemeToken, string, Surface, number])[] = [
  ["foreground", "background", BG, 4.5],
  ["foreground", "card", CARD, 4.5],
  ["foreground", "well", [...CARD, "well"], 4.5],
  ["foreground", "well-hover", [...CARD, "well-hover"], 4.5],
  ["foreground", "muted", [...CARD, "muted"], 4.5],
  ["foreground", "popover", POPOVER, 4.5],
  ["foreground", "secondary", [...BG, "secondary"], 4.5],
  ["foreground", "accent", [...BG, "accent"], 4.5],
  ["foreground", "surface-subtle", [...BG, "surface-subtle"], 4.5],
  ["foreground", "surface-subtle over card", [...CARD, "surface-subtle"], 4.5],
  ["foreground", "sidebar-accent", [...SIDEBAR, "sidebar-accent"], 4.5],
  ["card-foreground", "card", CARD, 4.5],
  ["popover-foreground", "popover", POPOVER, 4.5],
  ["muted-foreground", "background", BG, 4.5],
  ["muted-foreground", "card", CARD, 4.5],
  ["muted-foreground", "well", [...CARD, "well"], 4.5],
  ["muted-foreground", "muted", [...CARD, "muted"], 4.5],
  ["muted-foreground", "popover", POPOVER, 4.5],
  ["muted-foreground", "accent", [...BG, "accent"], 4.5],
  ["muted-foreground", "surface-subtle", [...BG, "surface-subtle"], 4.5],
  ["muted-foreground", "sidebar", SIDEBAR, 4.5],
  ["muted-foreground", "sidebar-accent", [...SIDEBAR, "sidebar-accent"], 4.5],
  ["secondary-foreground", "secondary", [...BG, "secondary"], 4.5],
  ["secondary-foreground", "surface-subtle", [...BG, "surface-subtle"], 4.5],
  ["accent-foreground", "accent", [...BG, "accent"], 4.5],
  ["sidebar-foreground", "sidebar", SIDEBAR, 4.5],
  ["sidebar-foreground", "sidebar-accent", [...SIDEBAR, "sidebar-accent"], 4.5],
  ["sidebar-accent-foreground", "sidebar-accent", [...SIDEBAR, "sidebar-accent"], 4.5],
  ["primary-foreground", "primary", [...BG, "primary"], 4.5],
  ["primary-foreground", "action-hover", [...BG, "action-hover"], 4.5],
  ["action", "background", BG, 4.5],
  ["action", "card", CARD, 4.5],
  ["action", "popover", POPOVER, 4.5],
  ["action", "sidebar", SIDEBAR, 4.5],
  // The rail's active and hover items: `bg-action/12 hover:bg-action/15 dark:bg-action/18`.
  ["action", "action/15 over sidebar", [...SIDEBAR, ["action", { light: 0.15, dark: 0.18 }]], 4.5],
  ["link", "background", BG, 4.5],
  ["link", "card", CARD, 4.5],
  ["link", "popover", POPOVER, 4.5],
  // The assistant's done/failed status icons (a check or a cross beside the step's words).
  ["background", "success status icon", [...BG, "success"], 3],
  ["background", "destructive status icon", [...BG, "destructive"], 3],
  ["destructive-foreground", "destructive", [...BG, "destructive"], 4.5],
  ["destructive", "destructive/10 over popover", [...POPOVER, ["destructive", 0.1]], 4.5],
  ["destructive-text", "background", BG, 4.5],
  ["destructive-text", "card", CARD, 4.5],
  ["destructive-text", "popover", POPOVER, 4.5],
  ["destructive-text", "destructive/10 over background", [...BG, ["destructive", 0.1]], 4.5],
  ["destructive-text", "destructive/10 over card", [...CARD, ["destructive", 0.1]], 4.5],
  ["destructive-text", "destructive/10 over popover", [...POPOVER, ["destructive", 0.1]], 4.5],
  ["dock-foreground", "dock-surface", DOCK, 4.5],
  ["dock-foreground", "dock-well", [...DOCK, "dock-well"], 4.5],
  ["dock-foreground", "dock-active", [...DOCK, "dock-active"], 4.5],
  ["dock-muted", "dock-surface", DOCK, 4.5],
  ["dock-muted", "dock-well", [...DOCK, "dock-well"], 4.5],
  ["ring", "background", BG, 3],
  ["dock-ring", "dock-surface", DOCK, 3],
]

/** The effective palette a theme paints for one scheme: the base skin's rendered palette under its overrides. */
export function effectiveThemePalette(theme: StudioTheme, scheme: "light" | "dark"): Readonly<Record<StudioThemeToken, string>> {
  return { ...BASE_PALETTES[effectiveBaseSkin(theme)][scheme], ...resolveCustomThemeTokens(theme, scheme) }
}

/** Verify WCAG AA contrast for every ink/surface pair Studio renders, in each scheme the theme applies to. */
export function validateStudioThemeContrast(theme: StudioTheme): readonly StudioThemeValidationIssue[] {
  const issues: StudioThemeValidationIssue[] = []
  const schemes: readonly ("light" | "dark")[] = theme.appearance ? [theme.appearance] : ["light", "dark"]
  for (const scheme of schemes) {
    const palette = effectiveThemePalette(theme, scheme)
    const color = (token: StudioThemeToken): Color => parseColor(palette[token])!
    // `body` paints `bg-canvas` over the browser's opaque page, so every layer composites from there.
    const paint = (surface: Surface): Color => surface.reduce<Color>((below, layer) => {
      const [token, alpha] = typeof layer === "string" ? [layer, 1] : layer
      return over(fade(color(token), typeof alpha === "number" ? alpha : alpha[scheme]), below)
    }, scheme === "dark" ? OPAQUE_DARK : OPAQUE_LIGHT)
    for (const [ink, label, surface, floor] of PAIRS) {
      const ratio = contrastOn(color(ink), paint(surface))
      if (ratio < floor) {
        issues.push({
          path: `contrast.${scheme}.${ink} (on ${label})`,
          message: `${scheme} ${ink} (on ${label}) contrast is ${ratio.toFixed(2)}:1 (WCAG AA requires >= ${floor.toFixed(1)}:1).`,
        })
      }
    }
  }
  return issues
}

/** Parse, sanitize, and WCAG-AA-validate a StudioTheme JSON string or object. */
export function parseStudioTheme(raw: string | unknown): StudioThemeParseResult {
  let parsed: unknown = raw
  if (typeof raw === "string") {
    try {
      parsed = JSON.parse(raw)
    } catch (error) {
      return {
        ok: false,
        issues: [{ path: "$", message: `Invalid JSON: ${error instanceof Error ? error.message : String(error)}` }],
      }
    }
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { ok: false, issues: [{ path: "$", message: "Theme must be a JSON object." }] }
  }
  const obj = parsed as Record<string, unknown>
  const issues: StudioThemeValidationIssue[] = []

  if (obj.version !== undefined && obj.version !== 1) {
    issues.push({ path: "version", message: "Only theme version 1 is supported." })
  }
  const name = typeof obj.name === "string" ? obj.name.trim() : ""
  if (name === "" || name.length > 64) {
    issues.push({ path: "name", message: "Theme name must be a non-empty string (max 64 characters)." })
  }
  let baseSkin: Skin | undefined
  if (obj.baseSkin !== undefined) {
    if (typeof obj.baseSkin !== "string" || !VALID_SKINS.has(obj.baseSkin)) {
      issues.push({ path: "baseSkin", message: 'baseSkin must be one of "polar", "open", "luma", or "glass".' })
    } else {
      baseSkin = obj.baseSkin as Skin
    }
  }
  let appearance: "light" | "dark" | undefined
  if (obj.appearance !== undefined) {
    if (obj.appearance !== "light" && obj.appearance !== "dark") {
      issues.push({ path: "appearance", message: 'appearance must be "light" or "dark".' })
    } else {
      appearance = obj.appearance
    }
  }
  let accent: string | undefined
  if (obj.accent !== undefined) {
    const color = themeColor(obj.accent)
    if (color === null) issues.push({ path: "accent", message: "accent must be a CSS color such as #rrggbb, rgb(…) or oklch(…)." })
    else accent = color
  }
  let canvas: string | undefined
  if (obj.canvas !== undefined) {
    const color = themeColor(obj.canvas)
    if (color === null) issues.push({ path: "canvas", message: "canvas must be a CSS color such as #rrggbb, rgb(…) or oklch(…)." })
    else canvas = color
  }

  const colors = validateTokenMap(obj.colors, "colors", issues)
  let variants: StudioTheme["variants"]
  if (obj.variants !== undefined) {
    if (typeof obj.variants !== "object" || obj.variants === null || Array.isArray(obj.variants)) {
      issues.push({ path: "variants", message: "variants must be an object with optional light and dark maps." })
    } else {
      const v = obj.variants as Record<string, unknown>
      const light = validateTokenMap(v.light, "variants.light", issues)
      const dark = validateTokenMap(v.dark, "variants.dark", issues)
      variants = {
        ...(Object.keys(light).length > 0 ? { light } : {}),
        ...(Object.keys(dark).length > 0 ? { dark } : {}),
      }
    }
  }

  if (issues.length > 0) return { ok: false, issues }

  const theme: StudioTheme = {
    version: 1,
    name,
    ...(baseSkin ? { baseSkin } : {}),
    ...(appearance ? { appearance } : {}),
    ...(accent ? { accent } : {}),
    ...(canvas ? { canvas } : {}),
    ...(Object.keys(colors).length > 0 ? { colors } : {}),
    ...(variants && (variants.light || variants.dark) ? { variants } : {}),
  }

  const contrastIssues = validateStudioThemeContrast(theme)
  if (contrastIssues.length > 0) return { ok: false, issues: contrastIssues }
  return { ok: true, theme, issues: [] }
}

/** Apply or clear a validated StudioTheme's CSS custom properties on the document root without leaking stale tokens. */
export function applyCustomTheme(
  theme: StudioTheme | null,
  isDark: boolean,
  root: HTMLElement | undefined = globalThis.document?.documentElement,
) {
  if (!root) return
  const previous = root.dataset[APPLIED_TOKENS_ATTR]?.split(",").filter(Boolean) ?? []
  for (const token of previous) {
    root.style.removeProperty(`--${token}`)
  }
  if (!theme) {
    delete root.dataset[APPLIED_TOKENS_ATTR]
    delete root.dataset.customTheme
    return
  }
  const scheme: "light" | "dark" = theme.appearance ?? (isDark ? "dark" : "light")
  const tokens = resolveCustomThemeTokens(theme, scheme)
  // Only allowlisted tokens, each written as the canonical CSS of its parsed colour, ever reach the page.
  const keys = Object.keys(tokens).filter((key) => ALLOWED_TOKENS.has(key) && canonicalColor(tokens[key]!) !== null)
  for (const key of keys) {
    root.style.setProperty(`--${key}`, canonicalColor(tokens[key]!)!)
  }
  root.dataset[APPLIED_TOKENS_ATTR] = keys.join(",")
  root.dataset.customTheme = theme.name
}

/** Read a persisted custom StudioTheme from localStorage; malformed or low-contrast entries are ignored safely. */
export function storedCustomTheme(): StudioTheme | null {
  try {
    const raw = globalThis.localStorage?.getItem(CUSTOM_THEME_KEY)
    if (!raw) return null
    const result = parseStudioTheme(raw)
    return result.ok ? result.theme : null
  } catch {
    return null
  }
}

export function persistCustomTheme(theme: StudioTheme | null): void {
  try {
    if (theme === null) globalThis.localStorage?.removeItem(CUSTOM_THEME_KEY)
    else globalThis.localStorage?.setItem(CUSTOM_THEME_KEY, JSON.stringify(theme))
  } catch {
    /* storage blocked: skip persistence */
  }
}

/** Serialize the active custom theme (or a starter export matching the active built-in skin) as formatted JSON. */
export function exportStudioThemeJson(skin: Skin, customTheme: StudioTheme | null): string {
  if (customTheme) return JSON.stringify(customTheme, null, 2)
  const preset: StudioTheme = {
    version: 1,
    name: `Custom ${skin[0]!.toUpperCase()}${skin.slice(1)}`,
    baseSkin: skin,
    variants: {
      light: {
        canvas: BASE_PALETTES[skin].light.canvas,
        background: BASE_PALETTES[skin].light.background,
        foreground: BASE_PALETTES[skin].light.foreground,
        primary: BASE_PALETTES[skin].light.primary,
        "primary-foreground": BASE_PALETTES[skin].light["primary-foreground"],
        action: BASE_PALETTES[skin].light.action,
        "action-hover": BASE_PALETTES[skin].light["action-hover"],
        ring: BASE_PALETTES[skin].light.ring,
      },
      dark: {
        canvas: BASE_PALETTES[skin].dark.canvas,
        background: BASE_PALETTES[skin].dark.background,
        foreground: BASE_PALETTES[skin].dark.foreground,
        primary: BASE_PALETTES[skin].dark.primary,
        "primary-foreground": BASE_PALETTES[skin].dark["primary-foreground"],
        action: BASE_PALETTES[skin].dark.action,
        "action-hover": BASE_PALETTES[skin].dark["action-hover"],
        ring: BASE_PALETTES[skin].dark.ring,
      },
    },
  }
  return JSON.stringify(preset, null, 2)
}
