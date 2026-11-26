/**
 * Themes as data (#1705 follow-up, owner 2026-11-25): a Studio theme is a `theme` definition the
 * cell admits and serves, so an agent changes the workspace's look with no redeploy. This module is
 * the one TypeScript source of the theme contract: the token allowlist, the payload shape, the
 * validators Studio runs at render and the `defineTheme` builder runs at lowering, and the colour
 * maths they share. It imports nothing outside this directory (the builder runs in Studio's
 * browser bundle too), so its colours are the canonical forms a payload stores: `#rgb`, `#rgba`,
 * `#rrggbb`, `#rrggbbaa`, `rgb(r, g, b)` and `rgba(r, g, b, a)`. Studio's editor accepts any CSS
 * colour and writes it in canonical form before it reaches these validators.
 *
 * The server's checker admits a theme by the same rules (schema, known tokens, parseable colours,
 * WCAG contrast for every pair below in light and dark, monochrome chrome, a known base skin);
 * `fixtures/themes/expected.json` holds the golden answers both sides must give.
 */
import PALETTES from "./theme-palettes.ts"

/**
 * Semantic CSS custom properties a theme may override. Strictly allowlisted so a theme restyles
 * visual tokens only: never layout geometry, z-index, or arbitrary CSS.
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
  // DESIGN.md 3: the colour tokens the spec adds join the allowlist; geometry tokens never do.
  "subtle-foreground",
  "line",
  "action-dot",
  "action-soft",
  "dot-live",
  "dot-held",
  "dot-bad",
  "dot-quiet",
  "dot-accent",
  "avatar-1-bg",
  "avatar-1-fg",
  "avatar-2-bg",
  "avatar-2-fg",
  "avatar-3-bg",
  "avatar-3-fg",
  "avatar-4-bg",
  "avatar-4-fg",
  "avatar-5-bg",
  "avatar-5-fg",
  "avatar-6-bg",
  "avatar-6-fg",
  // DESIGN.md 4.7: the three-island shell's backdrop, its two ambient glows and the main island's material.
  "backdrop",
  "backdrop-glow-1",
  "backdrop-glow-2",
  "island",
] as const

export type StudioThemeToken = (typeof STUDIO_THEME_TOKENS)[number]
export type StudioThemeTokenMap = Partial<Record<StudioThemeToken, string>>

/** The built-in skins a theme may start from. */
export const THEME_SKINS = ["polar", "open", "luma", "glass"] as const
export type ThemeSkin = (typeof THEME_SKINS)[number]
export type ThemeScheme = "light" | "dark"

/**
 * A Studio theme: a name, an optional base skin, and per-scheme token overrides (VS Code style).
 * Large structural regions stay monochrome; every override is gated by WCAG AA contrast.
 */
export interface StudioTheme {
  readonly version: 1
  readonly name: string
  readonly baseSkin?: ThemeSkin
  readonly appearance?: ThemeScheme
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

/**
 * Why a theme is refused, in the checker's code registry form (`<area>.<category>.<detail>`). The
 * server's checker answers with the same codes, so a refusal reads the same wherever it was found.
 */
export const THEME_ISSUE = {
  /** Not a JSON object, or a field with the wrong type or value (version, name, appearance, variants). */
  schema: "theme.validation.schema",
  /** `baseSkin` is not a built-in skin. */
  skin: "theme.validation.unknown_skin",
  /** A token outside the allowlist. */
  token: "theme.validation.unknown_token",
  /** A value that is not a colour. */
  color: "theme.validation.invalid_color",
  /** An ink/surface pair below its WCAG floor. */
  contrast: "theme.validation.contrast",
  /** A chrome token with a hue (OKLCH chroma above the monochrome limit). */
  monochrome: "theme.validation.monochrome",
} as const
export type ThemeIssueCode = (typeof THEME_ISSUE)[keyof typeof THEME_ISSUE]

export interface StudioThemeValidationIssue {
  readonly code: ThemeIssueCode
  readonly path: string
  readonly message: string
}

export type StudioThemeParseResult =
  | { readonly ok: true; readonly theme: StudioTheme; readonly issues: readonly [] }
  | { readonly ok: false; readonly issues: readonly StudioThemeValidationIssue[] }

// ---------------------------------------------------------------------------------------------
// Colour: sRGB channels 0–1 with alpha 0–1, composited and measured as a browser paints them.
// ---------------------------------------------------------------------------------------------

/** An sRGB colour, channels 0–1, with alpha 0–1. */
export interface Color {
  readonly mode: "rgb"
  readonly r: number
  readonly g: number
  readonly b: number
  readonly alpha: number
}

const HEX = /^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i
const CHANNEL = "(\\d{1,3})"
const ALPHA = "(0|1|0?\\.\\d+|1\\.0+)"
const RGB = new RegExp(`^rgba?\\(\\s*${CHANNEL}\\s*,\\s*${CHANNEL}\\s*,\\s*${CHANNEL}\\s*(?:,\\s*${ALPHA}\\s*)?\\)$`, "i")

/**
 * Parse one theme colour in canonical form (`#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`,
 * `rgb(r, g, b)` or `rgba(r, g, b, a)`, channels 0–255 and alpha 0–1), or null when it isn't one.
 */
export function parseThemeColor(raw: string): Color | null {
  const value = raw.trim()
  const hex = HEX.exec(value)
  if (hex) {
    const digits = hex[1]!
    const wide = digits.length > 4 ? digits : [...digits].map((digit) => digit + digit).join("")
    const byte = (at: number) => Number.parseInt(wide.slice(at, at + 2), 16) / 255
    return { mode: "rgb", r: byte(0), g: byte(2), b: byte(4), alpha: wide.length === 8 ? byte(6) : 1 }
  }
  const rgb = RGB.exec(value)
  if (rgb === null) return null
  const channels = [rgb[1], rgb[2], rgb[3]].map(Number)
  if (channels.some((channel) => channel > 255)) return null
  const [r, g, b] = channels.map((channel) => channel / 255) as [number, number, number]
  return { mode: "rgb", r, g, b, alpha: rgb[4] === undefined ? 1 : Number(rgb[4]) }
}

const clamp01 = (value: number) => Math.max(0, Math.min(1, value || 0))
const byte = (value: number) => Math.round(clamp01(value) * 255)

/** The canonical CSS for a colour: `#rrggbb` when opaque, else `rgba(r, g, b, a)` with a two-decimal alpha. */
export function cssColor(color: Color): string {
  if (color.alpha >= 1) return `#${((1 << 24) | (byte(color.r) << 16) | (byte(color.g) << 8) | byte(color.b)).toString(16).slice(1)}`
  return `rgba(${byte(color.r)}, ${byte(color.g)}, ${byte(color.b)}, ${Math.round(clamp01(color.alpha) * 100) / 100})`
}

/** A theme colour in canonical CSS, or null when it isn't a canonical-form colour. */
export function canonicalThemeColor(raw: unknown): string | null {
  if (typeof raw !== "string") return null
  const color = parseThemeColor(raw)
  return color === null ? null : cssColor(color)
}

/** A channel (0–1) rounded to the nearest of the 256 values a browser paints. */
const to8 = (channel: number) => Math.round(channel * 255) / 255

/**
 * `top` painted over `bottom` (CSS `normal` blending, source-over), with each channel rounded to 8
 * bits, as a browser paints every layer and as DESIGN.md section 4.2 measures composited surfaces.
 */
export function over(top: Color, bottom: Color): Color {
  const alpha = top.alpha + bottom.alpha * (1 - top.alpha)
  const channel = (s: number, b: number) => alpha === 0
    ? 0
    : Math.max(0, Math.min(1, (top.alpha * (1 - bottom.alpha) * s + top.alpha * bottom.alpha * s + (1 - top.alpha) * bottom.alpha * b) / alpha))
  return { mode: "rgb", r: to8(channel(top.r, bottom.r)), g: to8(channel(top.g, bottom.g)), b: to8(channel(top.b, bottom.b)), alpha }
}

/** A colour with its alpha scaled, as `color-mix(in …, c p%, transparent)` and Tailwind's `/p` modifier paint it. */
export function fade(color: Color, ratio: number): Color {
  return { ...color, alpha: color.alpha * ratio }
}

const linear = (c: number) => {
  const abs = Math.abs(c)
  return abs <= 0.04045 ? c / 12.92 : (Math.sign(c) || 1) * ((abs + 0.055) / 1.055) ** 2.4
}
const luminance = (color: Color) => 0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)

/** WCAG 2.x contrast of `ink` painted over the opaque `surface`. */
export function contrastOn(ink: Color, surface: Color): number {
  const [a, b] = [luminance(over(ink, surface)), luminance(surface)]
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)
}

/** OKLCH chroma of a colour's RGB (alpha ignored), as CSS Color 4 converts sRGB to OKLab. */
export function oklchChroma(color: Color): number {
  const [r, g, b] = [linear(color.r), linear(color.g), linear(color.b)]
  const l = Math.cbrt(0.412221469470763 * r + 0.5363325372617348 * g + 0.0514459932675022 * b)
  const m = Math.cbrt(0.2119034958178252 * r + 0.6806995506452344 * g + 0.1073969535369406 * b)
  const s = Math.cbrt(0.0883024591900564 * r + 0.2817188391361215 * g + 0.6299787016738222 * b)
  const A = 1.9779985324311684 * l - 2.4285922420485799 * m + 0.450593709617411 * s
  const B = 0.0259040424655478 * l + 0.7827717124575296 * m - 0.8086757549230774 * s
  return Math.sqrt(A * A + B * B)
}

// ---------------------------------------------------------------------------------------------
// The theme: its effective palette per scheme, and the gates.
// ---------------------------------------------------------------------------------------------

const ALLOWED_TOKENS: ReadonlySet<string> = new Set(STUDIO_THEME_TOKENS)
const VALID_SKINS: ReadonlySet<string> = new Set(THEME_SKINS)

/** Whether `token` is on the allowlist. */
export const isThemeToken = (token: string): token is StudioThemeToken => ALLOWED_TOKENS.has(token)

/** Effective built-in base skin used identically for validation and rendering. */
export const effectiveBaseSkin = (theme: Pick<StudioTheme, "baseSkin">): ThemeSkin => theme.baseSkin ?? "luma"

/**
 * Every built-in skin's painted palette, generated from Studio's `index.css` by
 * `apps/studio/scripts/theme-palettes.ts` (Studio's `test/theme-palettes.test.ts` fails on drift),
 * so the gate validates the colours Studio renders.
 */
export const BASE_PALETTES: Readonly<Record<ThemeSkin, Readonly<Record<ThemeScheme, Readonly<Record<StudioThemeToken, string>>>>>> = PALETTES

const OPAQUE_LIGHT = parseThemeColor("#ffffff")!
const OPAQUE_DARK = parseThemeColor("#000000")!
const BLACK = parseThemeColor("#000000")!
const DARK_INK = parseThemeColor("#141416")!
const LIGHT_INK = parseThemeColor("#fafafa")!

/** Pick crisp dark or light foreground ink for a shorthand `accent` override. */
function autoInkFor(color: Color): string {
  return contrastOn(LIGHT_INK, color) >= contrastOn(DARK_INK, color) ? "#fafafa" : "#141416"
}

/** Compute the effective CSS custom property overrides a StudioTheme writes for one light/dark scheme. */
export function resolveCustomThemeTokens(theme: StudioTheme, scheme: ThemeScheme): Record<string, string> {
  const overrides: Record<string, string> = {}
  if (theme.canvas) {
    overrides.canvas = theme.canvas
    overrides.shell = theme.canvas
  }
  if (theme.accent) {
    const parsed = parseThemeColor(theme.accent)
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
  // A theme that paints its own ground but not the islands gets opaque islands in that ground, on a
  // backdrop one step darker, so its canvas is what the reader sees (DESIGN.md 4.7).
  const ground = parseThemeColor(overrides.canvas ?? "")
  if (ground !== null) {
    overrides.island ??= overrides.canvas!
    overrides.sidebar ??= overrides.shell ?? overrides.canvas!
    overrides.backdrop ??= cssColor(over(fade(BLACK, scheme === "dark" ? 0.4 : 0.05), { ...ground, alpha: 1 }))
  }
  return overrides
}

/**
 * One surface as Studio paints it: its layers from the bottom up, each a token at an optional
 * alpha (Tailwind's `/p` modifier, per scheme when `dark:` changes it), over the scheme's canvas.
 */
type Alpha = number | { readonly light: number; readonly dark: number }
type Surface = readonly (StudioThemeToken | readonly [StudioThemeToken, Alpha])[]

const CANVAS: Surface = ["canvas"]
const SHELL: Surface = ["canvas", "shell"]
const BG: Surface = [...SHELL, "background"]
const CARD: Surface = [...BG, "card"]
const POPOVER: Surface = [...BG, "popover"]
/**
 * DESIGN.md 4.7: the backdrop the islands float on, at each of its extremes: flat, under either
 * ambient glow at its peak, and under both. Every island pair is measured over all four.
 */
const BACKDROPS: readonly (readonly [string, Surface])[] = [
  ["backdrop", ["backdrop"]],
  ["backdrop under glow 1", ["backdrop", "backdrop-glow-1"]],
  ["backdrop under glow 2", ["backdrop", "backdrop-glow-2"]],
  ["backdrop under both glows", ["backdrop", "backdrop-glow-1", "backdrop-glow-2"]],
]
// The phone sheet is opaque `--shell` in every skin, so its rows are measured on that ground too.
const SHEET: Surface = [...CANVAS, "shell"]
const SHEET_HOVER: Surface = [...SHEET, ["sidebar-accent", 0.5]]
const SHEET_PILL: Surface = [...SHEET, "sidebar-accent"]
const DOCK: Surface = [...BG, "dock-surface"]
/** A decorative hairline only has to stay visible against its surface. */
const HAIRLINE = 1.1
/** A status dot beside its word only has to stay a visible mark, not carry the state alone. */
const MARK = 1.5

/**
 * DESIGN.md 4.6 and 4.7: what sits on the islands over one backdrop. On the main island: page text,
 * links, accent text and the ring, and the same on a card and a well inside it. On the nav islands
 * (the applet rail and the submenu): resting rows, the names, the 50% hover wash, the selection pill
 * and the ring, which is also the attention dot.
 */
function islandPairs(where: string, backdrop: Surface): readonly (readonly [StudioThemeToken, string, Surface, number])[] {
  const island: Surface = [...backdrop, "island"]
  const card: Surface = [...island, "card"]
  const well: Surface = [...card, "well"]
  const nav: Surface = [...backdrop, "sidebar"]
  const hover: Surface = [...nav, ["sidebar-accent", 0.5]]
  const pill: Surface = [...nav, "sidebar-accent"]
  return [
    ["foreground", `main island over ${where}`, island, 4.5],
    ["muted-foreground", `main island over ${where}`, island, 4.5],
    ["action", `main island over ${where}`, island, 4.5],
    ["link", `main island over ${where}`, island, 4.5],
    ["ring", `main island over ${where}`, island, 3],
    ["foreground", `card on the main island over ${where}`, card, 4.5],
    ["muted-foreground", `card on the main island over ${where}`, card, 4.5],
    ["action", `card on the main island over ${where}`, card, 4.5],
    ["ring", `card on the main island over ${where}`, card, 3],
    ["muted-foreground", `well on the main island over ${where}`, well, 4.5],
    // The status line's dot sits beside its word, so it is kept visible, not held to 3:1 (DESIGN.md 4.3).
    ["dot-bad", `main island over ${where}`, island, MARK],
    ["dot-held", `main island over ${where}`, island, MARK],
    ["muted-foreground", `nav island over ${where}`, nav, 4.5],
    ["muted-foreground", `nav island hover wash over ${where}`, hover, 4.5],
    ["sidebar-foreground", `nav island over ${where}`, nav, 4.5],
    ["sidebar-foreground", `nav island hover wash over ${where}`, hover, 4.5],
    ["sidebar-accent-foreground", `nav island pill over ${where}`, pill, 4.5],
    ["ring", `nav island over ${where}`, nav, 3],
    ["ring", `nav island hover wash over ${where}`, hover, 3],
    ["ring", `nav island pill over ${where}`, pill, 3],
  ]
}

/**
 * The ink/surface pairs Studio renders (from the components' `bg-*`/`text-*` pairs), each with its
 * WCAG floor: 4.5:1 for text, 3:1 for focus rings and status icons. A partial-alpha fill sits
 * between its host and the full fill, so gating both ends covers it.
 */
const PAIRS: readonly (readonly [StudioThemeToken, string, Surface, number])[] = [
  // The content column is the unframed canvas (DESIGN.md 9.1): page heads, prose and links sit on it.
  ["foreground", "canvas", CANVAS, 4.5],
  ["muted-foreground", "canvas", CANVAS, 4.5],
  ["action", "canvas", CANVAS, 4.5],
  ["link", "canvas", CANVAS, 4.5],
  ["ring", "canvas", CANVAS, 3],
  ["foreground", "background", BG, 4.5],
  ["foreground", "card", CARD, 4.5],
  ["foreground", "well", [...CARD, "well"], 4.5],
  ["foreground", "well-hover", [...CARD, "well-hover"], 4.5],
  // The map's node cards: `hover:bg-well-hover` replaces their `bg-card`, so the hover lies on the page.
  ["foreground", "well-hover over background", [...BG, "well-hover"], 4.5],
  // A hovered row inside a well: a translucent `well-hover` lies over the well, not the card.
  ["foreground", "well-hover over well", [...CARD, "well", "well-hover"], 4.5],
  ["foreground", "muted", [...CARD, "muted"], 4.5],
  ["foreground", "popover", POPOVER, 4.5],
  ["foreground", "secondary", [...BG, "secondary"], 4.5],
  ["foreground", "accent", [...BG, "accent"], 4.5],
  ["foreground", "surface-subtle", [...BG, "surface-subtle"], 4.5],
  ["foreground", "surface-subtle over card", [...CARD, "surface-subtle"], 4.5],
  ["card-foreground", "card", CARD, 4.5],
  ["popover-foreground", "popover", POPOVER, 4.5],
  ["muted-foreground", "background", BG, 4.5],
  ["muted-foreground", "card", CARD, 4.5],
  ["muted-foreground", "well", [...CARD, "well"], 4.5],
  ["muted-foreground", "well-hover", [...CARD, "well-hover"], 4.5],
  ["muted-foreground", "well-hover over background", [...BG, "well-hover"], 4.5],
  ["muted-foreground", "well-hover over well", [...CARD, "well", "well-hover"], 4.5],
  ["muted-foreground", "muted", [...CARD, "muted"], 4.5],
  ["muted-foreground", "popover", POPOVER, 4.5],
  ["muted-foreground", "accent", [...BG, "accent"], 4.5],
  ["muted-foreground", "surface-subtle", [...BG, "surface-subtle"], 4.5],
  ["muted-foreground", "phone sheet", SHEET, 4.5],
  ["muted-foreground", "phone sheet hover wash", SHEET_HOVER, 4.5],
  ["secondary-foreground", "secondary", [...BG, "secondary"], 4.5],
  ["secondary-foreground", "surface-subtle", [...BG, "surface-subtle"], 4.5],
  ["accent-foreground", "accent", [...BG, "accent"], 4.5],
  ["sidebar-foreground", "phone sheet", SHEET, 4.5],
  ["sidebar-foreground", "phone sheet hover wash", SHEET_HOVER, 4.5],
  ["sidebar-accent-foreground", "phone sheet pill", SHEET_PILL, 4.5],
  // The focus ring and the attention dot (both `--ring`) on the phone sheet's ground, wash and pill.
  ["ring", "phone sheet", SHEET, 3],
  ["ring", "phone sheet hover wash", SHEET_HOVER, 3],
  ["ring", "phone sheet pill", SHEET_PILL, 3],
  ["primary-foreground", "primary", [...BG, "primary"], 4.5],
  ["primary-foreground", "action-hover", [...BG, "action-hover"], 4.5],
  ["action", "background", BG, 4.5],
  ["action", "card", CARD, 4.5],
  ["action", "popover", POPOVER, 4.5],

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
  // Decorative glyphs (separators, chevrons, info marks) in `--subtle-foreground` stay visible at 3:1 (DESIGN.md 4.1).
  ["subtle-foreground", "canvas", CANVAS, 3],
  ["subtle-foreground", "card", CARD, 3],
  ["subtle-foreground", "well", [...CARD, "well"], 3],
  // Hairlines (SourceTag rings, dividers, the empty ticks of the rules bar) are decorative, but never vanish.
  ["line", "card", CARD, HAIRLINE],
  ["line", "well", [...CARD, "well"], HAIRLINE],
  // `--action-soft` carries no muted text (DESIGN.md 4.2); ink on a drop target or a selection halo still reads.
  ["foreground", "action-soft over card", [...CARD, "action-soft"], 4.5],
  ["foreground", "action-soft over well", [...CARD, "well", "action-soft"], 4.5],
  // Status dots always sit beside their word (StatusDot), so they are not held to 3:1, only kept visible.
  ...(["dot-live", "dot-held", "dot-bad", "dot-quiet", "dot-accent", "action-dot"] as const).flatMap((dot) => [
    [dot, "card", CARD, MARK] as const,
    [dot, "well", [...CARD, "well"], MARK] as const,
  ]),
  // Avatar discs: initials are text on their disc, 4.5:1 (DESIGN.md 4.4, 16.2).
  ...([1, 2, 3, 4, 5, 6] as const).map((n) => [`avatar-${n}-fg`, `avatar-${n}-bg`, [...CARD, `avatar-${n}-bg`], 4.5] as const),
  // The islands, over the backdrop at each of its extremes (DESIGN.md 4.7).
  ...BACKDROPS.flatMap(([where, backdrop]) => islandPairs(where, backdrop)),
]


/** The effective palette a theme paints for one scheme: the base skin's rendered palette under its overrides. */
export function effectiveThemePalette(theme: StudioTheme, scheme: ThemeScheme): Readonly<Record<StudioThemeToken, string>> {
  return { ...BASE_PALETTES[effectiveBaseSkin(theme)][scheme], ...resolveCustomThemeTokens(theme, scheme) }
}

const schemesOf = (theme: StudioTheme): readonly ThemeScheme[] => theme.appearance ? [theme.appearance] : ["light", "dark"]

/** Verify WCAG AA contrast for every ink/surface pair Studio renders, in each scheme the theme applies to. */
export function validateStudioThemeContrast(theme: StudioTheme): readonly StudioThemeValidationIssue[] {
  const issues: StudioThemeValidationIssue[] = []
  for (const scheme of schemesOf(theme)) {
    const palette = effectiveThemePalette(theme, scheme)
    const color = (token: StudioThemeToken): Color => parseThemeColor(palette[token])!
    // `body` paints `bg-canvas` over the browser's opaque page, so every layer composites from there.
    const paint = (surface: Surface): Color => surface.reduce<Color>((below, layer) => {
      const [token, alpha] = typeof layer === "string" ? [layer, 1] : layer
      return over(fade(color(token), typeof alpha === "number" ? alpha : alpha[scheme]), below)
    }, scheme === "dark" ? OPAQUE_DARK : OPAQUE_LIGHT)
    for (const [ink, label, surface, floor] of PAIRS) {
      const ratio = contrastOn(color(ink), paint(surface))
      if (ratio < floor) {
        issues.push({
          code: THEME_ISSUE.contrast,
          path: `contrast.${scheme}.${ink} (on ${label})`,
          message: `${scheme} ${ink} (on ${label}) contrast is ${ratio.toFixed(2)}:1 (WCAG AA requires >= ${floor.toFixed(1)}:1).`,
        })
      }
    }
  }
  return issues
}

/**
 * DESIGN.md 19.6, the monochrome rule: chrome is black, white and greys. Colour lives only in the
 * tokens for chart and canvas visualisation, a status beside its word, an avatar, the mascot, or a
 * data highlight. Studio's `test/monochrome.test.ts` holds every built-in skin to the same rule.
 */
export const THEME_DATA_TOKEN = /^(chart-\d|dot-(live|held|bad)|success|warning|destructive(-foreground|-text)?|avatar-\d-(bg|fg)|mascot-body|tone-[a-z]+|tint-[a-z]+|edge(-active)?|node-[a-z]+|canvas-[a-z]+)$/
/** The most OKLCH chroma a chrome colour may carry (Tailwind zinc is about 0.005; the old blue action was 0.20). */
export const MAX_CHROME_CHROMA = 0.02

/** Refuse every chrome token a theme paints with a hue, in each scheme the theme applies to. */
export function validateStudioThemeMonochrome(theme: StudioTheme): readonly StudioThemeValidationIssue[] {
  const issues: StudioThemeValidationIssue[] = []
  for (const scheme of schemesOf(theme)) {
    for (const [token, value] of Object.entries(resolveCustomThemeTokens(theme, scheme))) {
      if (THEME_DATA_TOKEN.test(token)) continue
      const color = parseThemeColor(value)
      if (color === null) continue
      const chroma = oklchChroma(color)
      if (chroma > MAX_CHROME_CHROMA) {
        issues.push({
          code: THEME_ISSUE.monochrome,
          path: `monochrome.${scheme}.${token}`,
          message: `${scheme} ${token} is ${value}, a hue (OKLCH chroma ${chroma.toFixed(3)} > ${MAX_CHROME_CHROMA}); chrome is black, white and greys, and colour is for data, status and avatars.`,
        })
      }
    }
  }
  return issues
}

/** Options for {@link parseStudioTheme}. */
export interface ParseThemeOptions {
  /**
   * How a colour value becomes canonical CSS, or null when it is not a colour. Defaults to the
   * canonical forms ({@link canonicalThemeColor}); Studio's editor passes a CSS Color 4 parser so a
   * person may paste `oklch(…)`, and stores what it returns.
   */
  readonly canonicalColor?: (raw: unknown) => string | null
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)

function validateTokenMap(
  input: unknown,
  pathPrefix: string,
  issues: StudioThemeValidationIssue[],
  canonical: (raw: unknown) => string | null,
): StudioThemeTokenMap {
  if (input === undefined) return {}
  if (!isRecord(input)) {
    issues.push({ code: THEME_ISSUE.schema, path: pathPrefix, message: "Expected an object mapping semantic token names to CSS colors." })
    return {}
  }
  const clean: Record<string, string> = {}
  for (const [rawKey, rawVal] of Object.entries(input)) {
    const key = rawKey.startsWith("--") ? rawKey.slice(2) : rawKey
    if (!ALLOWED_TOKENS.has(key)) {
      issues.push({ code: THEME_ISSUE.token, path: `${pathPrefix}.${rawKey}`, message: `Unknown or disallowed theme token "${rawKey}".` })
      continue
    }
    const color = canonical(rawVal)
    if (color === null) {
      issues.push({ code: THEME_ISSUE.color, path: `${pathPrefix}.${key}`, message: `Invalid color value for "${key}". Use a CSS color such as #rrggbb or rgba(r, g, b, a).` })
      continue
    }
    clean[key] = color
  }
  return clean
}

/**
 * Read a theme (a JSON string or an object) into its canonical payload: its schema, its tokens and
 * its colours, without the contrast and monochrome gates. What an editor sends to the cell, which
 * gates it; {@link parseStudioTheme} adds the gates.
 */
export function normalizeStudioTheme(raw: string | unknown, options: ParseThemeOptions = {}): StudioThemeParseResult {
  const canonical = options.canonicalColor ?? canonicalThemeColor
  let parsed: unknown = raw
  if (typeof raw === "string") {
    try {
      parsed = JSON.parse(raw)
    } catch (error) {
      return {
        ok: false,
        issues: [{ code: THEME_ISSUE.schema, path: "$", message: `Invalid JSON: ${error instanceof Error ? error.message : String(error)}` }],
      }
    }
  }
  if (!isRecord(parsed)) {
    return { ok: false, issues: [{ code: THEME_ISSUE.schema, path: "$", message: "Theme must be a JSON object." }] }
  }
  const obj = parsed
  const issues: StudioThemeValidationIssue[] = []

  if (obj.version !== undefined && obj.version !== 1) {
    issues.push({ code: THEME_ISSUE.schema, path: "version", message: "Only theme version 1 is supported." })
  }
  if (obj.themeVersion !== undefined && obj.themeVersion !== "1") {
    issues.push({ code: THEME_ISSUE.schema, path: "themeVersion", message: 'Only themeVersion "1" is supported.' })
  }
  const name = typeof obj.name === "string" ? obj.name.trim() : ""
  if (name === "" || name.length > 64) {
    issues.push({ code: THEME_ISSUE.schema, path: "name", message: "Theme name must be a non-empty string (max 64 characters)." })
  }
  let baseSkin: ThemeSkin | undefined
  if (obj.baseSkin !== undefined) {
    if (typeof obj.baseSkin !== "string" || !VALID_SKINS.has(obj.baseSkin)) {
      issues.push({ code: THEME_ISSUE.skin, path: "baseSkin", message: 'baseSkin must be one of "polar", "open", "luma", or "glass".' })
    } else {
      baseSkin = obj.baseSkin as ThemeSkin
    }
  }
  let appearance: ThemeScheme | undefined
  if (obj.appearance !== undefined) {
    if (obj.appearance !== "light" && obj.appearance !== "dark") {
      issues.push({ code: THEME_ISSUE.schema, path: "appearance", message: 'appearance must be "light" or "dark".' })
    } else {
      appearance = obj.appearance
    }
  }
  let accent: string | undefined
  if (obj.accent !== undefined) {
    const color = canonical(obj.accent)
    if (color === null) issues.push({ code: THEME_ISSUE.color, path: "accent", message: "accent must be a CSS color such as #rrggbb or rgba(r, g, b, a)." })
    else accent = color
  }
  let canvas: string | undefined
  if (obj.canvas !== undefined) {
    const color = canonical(obj.canvas)
    if (color === null) issues.push({ code: THEME_ISSUE.color, path: "canvas", message: "canvas must be a CSS color such as #rrggbb or rgba(r, g, b, a)." })
    else canvas = color
  }

  const colors = validateTokenMap(obj.colors, "colors", issues, canonical)
  let variants: StudioTheme["variants"]
  if (obj.variants !== undefined) {
    if (!isRecord(obj.variants)) {
      issues.push({ code: THEME_ISSUE.schema, path: "variants", message: "variants must be an object with optional light and dark maps." })
    } else {
      const light = validateTokenMap(obj.variants.light, "variants.light", issues, canonical)
      const dark = validateTokenMap(obj.variants.dark, "variants.dark", issues, canonical)
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

  return { ok: true, theme, issues: [] }
}

/**
 * Parse, sanitize and gate a theme (a JSON string or an object): its schema, its tokens and
 * colours, WCAG AA contrast and the monochrome rule. The admitted theme holds canonical colours only.
 */
export function parseStudioTheme(raw: string | unknown, options: ParseThemeOptions = {}): StudioThemeParseResult {
  const normalized = normalizeStudioTheme(raw, options)
  if (!normalized.ok) return normalized
  const gated = [...validateStudioThemeContrast(normalized.theme), ...validateStudioThemeMonochrome(normalized.theme)]
  return gated.length > 0 ? { ok: false, issues: gated } : normalized
}

// ---------------------------------------------------------------------------------------------
// The builder: `defineTheme({...})` written as typed TypeScript, lowered to the `theme` definition.
// ---------------------------------------------------------------------------------------------

/**
 * The definition id of a workspace's theme. A workspace has one; installing a new version of it
 * repaints every open Studio page in that workspace, and retiring it brings back the default.
 */
export const WORKSPACE_THEME_ID = "workspace.theme"
/** The definition kind a theme is admitted and served as. */
export const THEME_KIND = "theme"

/** A colour in a form the theme payload stores; the lowering checks the digits and ranges. */
export type ThemeColor = `#${string}` | `rgb(${string})` | `rgba(${string})`
/** Token overrides keyed by the allowlist, so a misspelt token does not compile. */
export type ThemeTokens = { readonly [Token in StudioThemeToken]?: ThemeColor }

/** A theme written as code: the same fields as the payload, with typed tokens and colours. */
export interface ThemeSpec {
  /** The definition id; a workspace's own theme is {@link WORKSPACE_THEME_ID} (the default). */
  readonly id?: string
  readonly name: string
  readonly baseSkin?: ThemeSkin
  readonly appearance?: ThemeScheme
  readonly accent?: ThemeColor
  readonly canvas?: ThemeColor
  readonly colors?: ThemeTokens
  readonly variants?: { readonly light?: ThemeTokens; readonly dark?: ThemeTokens }
}

export interface ThemeDef extends ThemeSpec {
  readonly _tag: "Theme"
}

/** The definitions-apply submission a theme lowers to (`POST /v1/definitions:install`). */
export interface ThemeDefinition {
  readonly kind: typeof THEME_KIND
  readonly id: string
  readonly payload: StudioTheme
}

export type LowerThemeResult =
  | { readonly ok: true; readonly definition: ThemeDefinition }
  | { readonly ok: false; readonly issues: readonly StudioThemeValidationIssue[] }

/** Declare a theme. Nothing is checked until it is lowered, so a module may build one in steps. */
export function defineTheme(spec: ThemeSpec): ThemeDef {
  return { _tag: "Theme", ...spec }
}

const DEFINITION_ID = /^[a-z][a-z0-9._-]{0,127}$/

/**
 * Lower a theme to its `theme` definition: the payload Studio paints, its colours in canonical form,
 * gated by the same rules the cell admits it by (schema, tokens, colours, WCAG contrast in light and
 * dark, monochrome chrome, a known base skin). A refused theme lowers to its issues, never a payload.
 */
export function lowerTheme(def: ThemeDef | ThemeSpec): LowerThemeResult {
  const id = def.id ?? WORKSPACE_THEME_ID
  if (!DEFINITION_ID.test(id)) {
    return { ok: false, issues: [{ code: THEME_ISSUE.schema, path: "id", message: `Theme id "${id}" must match ^[a-z][a-z0-9._-]{0,127}$.` }] }
  }
  const { name, baseSkin, appearance, accent, canvas, colors, variants } = def
  const parsed = parseStudioTheme({ version: 1, name, baseSkin, appearance, accent, canvas, colors, variants })
  return parsed.ok ? { ok: true, definition: { kind: THEME_KIND, id, payload: parsed.theme } } : { ok: false, issues: parsed.issues }
}
