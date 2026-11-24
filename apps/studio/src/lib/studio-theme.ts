import type { Skin } from "./appearance.tsx"

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
  /** Shorthand override for `--action`, `--primary`, `--ring`, and `--link` (with auto-contrasting foreground when omitted). */
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

/** Base token palettes for WCAG AA validation when a custom theme overrides a subset of tokens. */
const BASE_PALETTES: Record<Skin, Record<"light" | "dark", Record<string, string>>> = {
  polar: {
    light: {
      shell: "#f6f6f8", canvas: "#f6f6f8", background: "#ffffff", foreground: "#18181b",
      card: "#ffffff", "card-foreground": "#18181b", well: "#f7f7f9",
      primary: "#18181b", "primary-foreground": "#fafafa",
      action: "#18181b", "action-hover": "#2c2c32", "action-foreground": "#fafafa",
      muted: "#f4f4f6", "muted-foreground": "#585861",
      ring: "#52525b", link: "#18181b", border: "#e5e5ea",
      destructive: "#b83228", "destructive-foreground": "#ffffff",
      success: "#1f7a4d", warning: "#8a5600", knot: "#5b52f0",
    },
    dark: {
      shell: "#0a0a0b", canvas: "#0a0a0b", background: "#111113", foreground: "#fafafa",
      card: "#161618", "card-foreground": "#fafafa", well: "#1b1b1e",
      primary: "#f4f4f5", "primary-foreground": "#141416",
      action: "#f4f4f5", "action-hover": "#e4e4e7", "action-foreground": "#141416",
      muted: "#202024", "muted-foreground": "#9d9da6",
      ring: "#a1a1aa", link: "#e4e4e7", border: "#26262b",
      destructive: "#eb7168", "destructive-foreground": "#0b0b0e",
      success: "#3ccf8e", warning: "#f0b04a", knot: "#7c74ff",
    },
  },
  open: {
    light: {
      shell: "#f6f6f7", canvas: "#ffffff", background: "#ffffff", foreground: "#1b1b1d",
      card: "#ffffff", "card-foreground": "#1b1b1d", well: "#f5f5f5",
      primary: "#2e60e0", "primary-foreground": "#ffffff",
      action: "#2e60e0", "action-hover": "#2451c7", "action-foreground": "#ffffff",
      muted: "#f5f5f5", "muted-foreground": "#5c5c63",
      ring: "#2e60e0", link: "#2451c7", border: "#ebebeb",
      destructive: "#b83228", "destructive-foreground": "#ffffff",
      success: "#1f7a4d", warning: "#8a5600", knot: "#5b52f0",
    },
    dark: {
      shell: "#09090a", canvas: "#0d0d0e", background: "#0d0d0e", foreground: "#ececee",
      card: "#151517", "card-foreground": "#ececee", well: "#1c1c1f",
      primary: "#2e60e0", "primary-foreground": "#ffffff",
      action: "#6b98ff", "action-hover": "#2451c7", "action-foreground": "#ffffff",
      muted: "#1c1c1f", "muted-foreground": "#9b9ba3",
      ring: "#5b8cff", link: "#7da4ff", border: "#232326",
      destructive: "#eb7168", "destructive-foreground": "#0b0b0e",
      success: "#3ccf8e", warning: "#f0b04a", knot: "#7c74ff",
    },
  },
  luma: {
    light: {
      shell: "#f3f4f6", canvas: "#f3f4f6", background: "#ffffff", foreground: "#1b1b1d",
      card: "#fafafb", "card-foreground": "#1b1b1d", well: "#fafafb",
      primary: "#554ce8", "primary-foreground": "#ffffff",
      action: "#554ce8", "action-hover": "#4940dc", "action-foreground": "#ffffff",
      muted: "#f1f2f5", "muted-foreground": "#5c5c63",
      ring: "#554ce8", link: "#4f46e5", border: "#e4e5ea",
      destructive: "#b83228", "destructive-foreground": "#ffffff",
      success: "#1f7a4d", warning: "#8a5600", knot: "#5b52f0",
    },
    dark: {
      shell: "#0b0b0e", canvas: "#0b0b0e", background: "#111115", foreground: "#ececee",
      card: "#17171d", "card-foreground": "#ececee", well: "#17171d",
      primary: "#6258ec", "primary-foreground": "#ffffff",
      action: "#7c74ff", "action-hover": "#695ff0", "action-foreground": "#ffffff",
      muted: "#1d1d24", "muted-foreground": "#9b9ba3",
      ring: "#7c74ff", link: "#938cff", border: "#262730",
      destructive: "#eb7168", "destructive-foreground": "#0b0b0e",
      success: "#3ccf8e", warning: "#f0b04a", knot: "#7c74ff",
    },
  },
  glass: {
    light: {
      shell: "#eef1f8", canvas: "#eef1f8", background: "#fbfcff", foreground: "#1b1b1d",
      card: "#f9faff", "card-foreground": "#1b1b1d", well: "#f4f6fc",
      primary: "#2e60e0", "primary-foreground": "#ffffff",
      action: "#2e60e0", "action-hover": "#2451c7", "action-foreground": "#ffffff",
      muted: "#ecf0fa", "muted-foreground": "#5c5c63",
      ring: "#2e60e0", link: "#2451c7", border: "#d8e0f2",
      destructive: "#b83228", "destructive-foreground": "#ffffff",
      success: "#1f7a4d", warning: "#8a5600", knot: "#5b52f0",
    },
    dark: {
      shell: "#0b0d17", canvas: "#0b0d17", background: "#121522", foreground: "#ececee",
      card: "#181c2c", "card-foreground": "#ececee", well: "#1e2336",
      primary: "#2e60e0", "primary-foreground": "#ffffff",
      action: "#6b98ff", "action-hover": "#2451c7", "action-foreground": "#ffffff",
      muted: "#1e2236", "muted-foreground": "#9b9ba3",
      ring: "#5b8cff", link: "#7da4ff", border: "#2b314a",
      destructive: "#eb7168", "destructive-foreground": "#0b0b0e",
      success: "#3ccf8e", warning: "#f0b04a", knot: "#7c74ff",
    },
  },
}

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
        "action-foreground": "#191715",
        border: "#302b26",
        ring: "#a8a29e",
      },
    },
  },
]

const HEX_SHORT = /^#([0-9a-fA-F]{3})$/
const HEX_FULL = /^#([0-9a-fA-F]{6})$/
const RGB_FUNC = /^rgba?\(\s*(\d{1,3})\s*[,\s]\s*(\d{1,3})\s*[,\s]\s*(\d{1,3})(?:\s*[,/]\s*([01](?:\.\d+)?|\.\d+))?\s*\)$/
const SAFE_COLOR_CHARS = /^[#a-zA-Z0-9(),.%/\s-]+$/

interface Rgba {
  readonly r: number
  readonly g: number
  readonly b: number
  readonly a: number
}

/** Parse hex (`#rgb` or `#rrggbb`) or `rgb(...)` / `rgba(...)` into sRGB channels. */
export function parseColorRgba(raw: string): Rgba | null {
  const value = raw.trim()
  const short = HEX_SHORT.exec(value)
  if (short) {
    const [r, g, b] = short[1]!.split("").map((ch) => Number.parseInt(ch + ch, 16))
    return { r: r!, g: g!, b: b!, a: 1 }
  }
  const full = HEX_FULL.exec(value)
  if (full) {
    const hex = full[1]!
    return {
      r: Number.parseInt(hex.slice(0, 2), 16),
      g: Number.parseInt(hex.slice(2, 4), 16),
      b: Number.parseInt(hex.slice(4, 6), 16),
      a: 1,
    }
  }
  const rgb = RGB_FUNC.exec(value)
  if (rgb) {
    const r = Number(rgb[1])
    const g = Number(rgb[2])
    const b = Number(rgb[3])
    const a = rgb[4] !== undefined ? Number(rgb[4]) : 1
    if (r > 255 || g > 255 || b > 255 || Number.isNaN(a) || a < 0 || a > 1) return null
    return { r, g, b, a }
  }
  return null
}

function compositeOver(fg: Rgba, bg: Rgba): Rgba {
  if (fg.a >= 1) return fg
  const inv = 1 - fg.a
  return {
    r: Math.round(fg.r * fg.a + bg.r * inv),
    g: Math.round(fg.g * fg.a + bg.g * inv),
    b: Math.round(fg.b * fg.a + bg.b * inv),
    a: 1,
  }
}

function relativeLuminance({ r, g, b }: Rgba): number {
  const toLin = (v: number) => {
    const s = v / 255
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * toLin(r) + 0.7152 * toLin(g) + 0.0722 * toLin(b)
}

/** WCAG 2.1 contrast ratio between two opaque or alpha-composited colors. */
export function colorContrastRatio(fg: Rgba, bg: Rgba): number {
  const flatFg = compositeOver(fg, bg)
  const l1 = relativeLuminance(flatFg)
  const l2 = relativeLuminance(bg)
  const [hi, lo] = l1 >= l2 ? [l1, l2] : [l2, l1]
  return (hi + 0.05) / (lo + 0.05)
}

/** Pick crisp dark or light foreground ink for a shorthand `accent` override. */
function autoInkFor(color: Rgba): string {
  const darkInk: Rgba = { r: 20, g: 20, b: 22, a: 1 }
  const lightInk: Rgba = { r: 250, g: 250, b: 250, a: 1 }
  return colorContrastRatio(lightInk, color) >= colorContrastRatio(darkInk, color) ? "#fafafa" : "#141416"
}

/** Validate a single CSS color token value — strictly hex or rgb()/rgba() to guarantee WCAG AA verifiability and zero CSS injection. */
function isSafeThemeColor(value: unknown): value is string {
  if (typeof value !== "string") return false
  const trimmed = value.trim()
  if (trimmed === "" || trimmed.length > 64 || !SAFE_COLOR_CHARS.test(trimmed)) return false
  return parseColorRgba(trimmed) !== null
}

function validateTokenMap(
  input: unknown,
  pathPrefix: string,
  issues: StudioThemeValidationIssue[],
): StudioThemeTokenMap {
  if (input === undefined) return {}
  if (typeof input !== "object" || input === null || Array.isArray(input)) {
    issues.push({ path: pathPrefix, message: "Expected an object mapping semantic token names to hex or rgb() colors." })
    return {}
  }
  const clean: Record<string, string> = {}
  for (const [rawKey, rawVal] of Object.entries(input)) {
    const key = rawKey.startsWith("--") ? rawKey.slice(2) : rawKey
    if (!ALLOWED_TOKENS.has(key)) {
      issues.push({ path: `${pathPrefix}.${rawKey}`, message: `Unknown or disallowed theme token "${rawKey}".` })
      continue
    }
    if (!isSafeThemeColor(rawVal)) {
      issues.push({ path: `${pathPrefix}.${key}`, message: `Invalid color value for "${key}". Use #rrggbb, #rgb, or rgb(...).` })
      continue
    }
    clean[key] = rawVal.trim()
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
    const parsed = parseColorRgba(theme.accent)
    const ink = parsed ? autoInkFor(parsed) : "#ffffff"
    overrides.primary = theme.accent
    overrides["primary-foreground"] = ink
    overrides.action = theme.accent
    overrides["action-foreground"] = ink
    overrides.ring = theme.accent
    overrides.link = theme.accent
  }
  if (theme.colors) Object.assign(overrides, theme.colors)
  const variant = theme.variants?.[scheme]
  if (variant) Object.assign(overrides, variant)
  return overrides
}

/** Verify WCAG AA contrast ratios for the scheme(s) a StudioTheme targets. */
export function validateStudioThemeContrast(theme: StudioTheme): readonly StudioThemeValidationIssue[] {
  const issues: StudioThemeValidationIssue[] = []
  const baseSkin: Skin = theme.baseSkin ?? "polar"
  const schemes: readonly ("light" | "dark")[] = theme.appearance ? [theme.appearance] : ["light", "dark"]

  for (const scheme of schemes) {
    const effective = { ...BASE_PALETTES[baseSkin][scheme], ...resolveCustomThemeTokens(theme, scheme) }
    const shell = parseColorRgba(effective.shell ?? "#ffffff")!
    const bg = compositeOver(parseColorRgba(effective.background ?? "#ffffff")!, shell)
    const card = compositeOver(parseColorRgba(effective.card ?? effective.background ?? "#ffffff")!, bg)
    const checks: [string, string, Rgba, number][] = [
      ["foreground", effective.foreground!, bg, 4.5],
      ["card-foreground", effective["card-foreground"] ?? effective.foreground!, card, 4.5],
      ["muted-foreground", effective["muted-foreground"]!, bg, 4.5],
      ["muted-foreground (on card)", effective["muted-foreground"]!, card, 4.5],
      ["primary-foreground (on primary)", effective["primary-foreground"]!, compositeOver(parseColorRgba(effective.primary!)!, bg), 4.5],
      ["action (on background)", effective.action!, bg, 4.5],
      ["ring (on background)", effective.ring!, bg, 3.0],
    ]
    if (resolveCustomThemeTokens(theme, scheme)["action-foreground"] !== undefined) {
      checks.push(["action-foreground (on action)", effective["action-foreground"]!, compositeOver(parseColorRgba(effective.action!)!, bg), 4.5])
    }
    for (const [label, fgRaw, host, floor] of checks) {
      const fg = parseColorRgba(fgRaw)
      if (!fg) continue
      const ratio = colorContrastRatio(fg, host)
      if (ratio < floor) {
        issues.push({
          path: `contrast.${scheme}.${label}`,
          message: `${scheme} ${label} contrast is ${ratio.toFixed(2)}:1 (WCAG AA requires >= ${floor.toFixed(1)}:1).`,
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
    if (!isSafeThemeColor(obj.accent)) {
      issues.push({ path: "accent", message: "accent must be a valid hex or rgb() color." })
    } else {
      accent = obj.accent.trim()
    }
  }
  let canvas: string | undefined
  if (obj.canvas !== undefined) {
    if (!isSafeThemeColor(obj.canvas)) {
      issues.push({ path: "canvas", message: "canvas must be a valid hex or rgb() color." })
    } else {
      canvas = obj.canvas.trim()
    }
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
  const keys = Object.keys(tokens)
  for (const key of keys) {
    root.style.setProperty(`--${key}`, tokens[key]!)
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
        ring: BASE_PALETTES[skin].light.ring,
      },
      dark: {
        canvas: BASE_PALETTES[skin].dark.canvas,
        background: BASE_PALETTES[skin].dark.background,
        foreground: BASE_PALETTES[skin].dark.foreground,
        primary: BASE_PALETTES[skin].dark.primary,
        "primary-foreground": BASE_PALETTES[skin].dark["primary-foreground"],
        action: BASE_PALETTES[skin].dark.action,
        ring: BASE_PALETTES[skin].dark.ring,
      },
    },
  }
  return JSON.stringify(preset, null, 2)
}
