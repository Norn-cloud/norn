import type { Skin } from "./appearance.tsx"
import { canonicalColor } from "./color.ts"
import {
  BASE_PALETTES,
  isThemeToken,
  normalizeStudioTheme as normalizeThemeCore,
  parseStudioTheme as parseThemeCore,
  resolveCustomThemeTokens,
  type StudioTheme,
  type StudioThemeParseResult,
} from "../../../../tools/norn-ts/src/builder/theme.ts"

/**
 * Studio's side of themes as data. The contract (tokens, payload, the contrast and monochrome gates)
 * is the shared theme core in `tools/norn-ts/src/builder/theme.ts`, one source with the
 * `defineTheme` builder and the server's admission rules; this module paints a theme on the page,
 * keeps the editor's draft, and offers the starter presets.
 */
export {
  MAX_CHROME_CHROMA,
  STUDIO_THEME_TOKENS,
  THEME_DATA_TOKEN,
  THEME_ISSUE,
  effectiveBaseSkin,
  effectiveThemePalette,
  resolveCustomThemeTokens,
  validateStudioThemeContrast,
  validateStudioThemeMonochrome,
  type StudioTheme,
  type StudioThemeParseResult,
  type StudioThemeToken,
  type StudioThemeTokenMap,
  type StudioThemeValidationIssue,
  type ThemeIssueCode,
} from "../../../../tools/norn-ts/src/builder/theme.ts"

const APPLIED_TOKENS_ATTR = "customThemeTokens"

/**
 * Parse and gate a theme a person typed or pasted: any CSS Color 4 colour is read (culori) and
 * stored in canonical form, then the shared gates decide. What it admits is the payload the
 * workspace's `theme` definition carries.
 */
export function parseStudioTheme(raw: string | unknown): StudioThemeParseResult {
  return parseThemeCore(raw, { canonicalColor: anyCssColor })
}

/** Read what a person typed into the canonical payload the cell gates, without gating it here. */
export function normalizeStudioTheme(raw: string | unknown): StudioThemeParseResult {
  return normalizeThemeCore(raw, { canonicalColor: anyCssColor })
}

const anyCssColor = (value: unknown) => (typeof value === "string" ? canonicalColor(value) : null)

/**
 * Gate a theme the server served: canonical colours only, exactly the rules the cell admitted it
 * by. A served theme that fails here is never painted (the page keeps the default and says why).
 */
export function parseServedTheme(raw: unknown): StudioThemeParseResult {
  return parseThemeCore(raw)
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
        "muted-foreground": "#52525b",
        primary: "#111318",
        "primary-foreground": "#f8fafc",
        action: "#111318",
        "action-hover": "#27272a",
        "action-foreground": "#f8fafc",
        ring: "#3f3f46",
        "chart-1": "#111318",
        "chart-2": "#0f726a",
      },
      dark: {
        shell: "#08090c",
        canvas: "#08090c",
        background: "#0f1117",
        card: "#141720",
        well: "#1c1c1f",
        foreground: "#f1f5f9",
        "muted-foreground": "#a1a1aa",
        primary: "#f1f5f9",
        "primary-foreground": "#0f1117",
        action: "#f1f5f9",
        "action-hover": "#dbe2ec",
        "action-foreground": "#0f1117",
        border: "#27272a",
        ring: "#a1a1aa",
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
        line: "#dcd7cc",
        "subtle-foreground": "#8a847c",
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
        line: "#3a342e",
        "subtle-foreground": "#7c766e",
        ring: "#a8a29e",
      },
    },
  },
]

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
  const keys = Object.keys(tokens).filter((key) => isThemeToken(key) && canonicalColor(tokens[key]!) !== null)
  for (const key of keys) {
    root.style.setProperty(`--${key}`, canonicalColor(tokens[key]!)!)
  }
  root.dataset[APPLIED_TOKENS_ATTR] = keys.join(",")
  root.dataset.customTheme = theme.name
}

/** Serialize the workspace theme (or a starter matching the reader's built-in skin) as formatted JSON. */
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
