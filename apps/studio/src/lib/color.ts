import {
  blend,
  clampRgb,
  converter,
  formatHex,
  formatRgb,
  modeHsl,
  modeHwb,
  modeLab,
  modeLch,
  modeLrgb,
  modeOklab,
  modeOklch,
  modeRgb,
  parse,
  useMode,
  wcagContrast,
  type Rgb,
} from "culori/fn"

// culori's tree-shakable entry parses and converts only the modes registered here: the CSS Color 4
// syntaxes a theme may use (hex, named, rgb, hsl, hwb, lab, lch, oklab, oklch) and linear sRGB for
// WCAG luminance.
for (const mode of [modeRgb, modeLrgb, modeHsl, modeHwb, modeLab, modeLch, modeOklab, modeOklch]) useMode(mode)

const toRgb = converter("rgb")

/** An sRGB colour, channels 0–1, with alpha 0–1. */
export type Color = Required<Pick<Rgb, "r" | "g" | "b" | "alpha">> & { readonly mode: "rgb" }

/** A functional colour whose alpha is written `/ none`, e.g. `rgb(0 0 0 / none)`. */
const EXPLICIT_NONE_ALPHA = /\/\s*none\s*\)$/i

/**
 * Parse one CSS colour (CSS Color 4: hex, named, rgb, hsl, hwb, lab, lch, oklab, oklch) into sRGB,
 * clamped to the sRGB gamut, or null when it isn't one. Validation and rendering both read this
 * value, so what the contrast gate checks is what the page paints.
 */
export function parseColor(raw: string): Color | null {
  const value = raw.trim()
  if (value === "" || value.length > 64) return null
  let parsed: ReturnType<typeof parse>
  // culori 4.0.2 throws on some malformed input (`color(` reads a token that isn't there); that is
  // not a colour, so it is refused like any other.
  try { parsed = parse(value) } catch { return null }
  if (parsed === undefined) return null
  const rgb = clampRgb(toRgb(parsed))
  // A CSS `none` channel (`rgb(none 0 0)`) reads as missing; CSS paints it as 0, so it is 0 here too.
  // culori records an omitted alpha and an explicit `/ none` alike (both missing), so the explicit
  // one is read from the text: it paints as 0 (transparent), while an omitted alpha is 1.
  const alpha = rgb.alpha ?? (EXPLICIT_NONE_ALPHA.test(value) ? 0 : 1)
  return { mode: "rgb", r: rgb.r ?? 0, g: rgb.g ?? 0, b: rgb.b ?? 0, alpha }
}

/** The canonical CSS for a colour: `#rrggbb` when opaque, else `rgba(r, g, b, a)`. */
export function cssColor(color: Color): string {
  return color.alpha >= 1 ? formatHex(color) : formatRgb(color)
}

/** A CSS colour string in canonical form, or null when it doesn't parse. */
export function canonicalColor(raw: string): string | null {
  const parsed = parseColor(raw)
  return parsed === null ? null : cssColor(parsed)
}

/** `top` painted over `bottom` (CSS `normal` blending, source-over). */
export function over(top: Color, bottom: Color): Color {
  const flat = blend([bottom, top], "normal") as Rgb
  return { mode: "rgb", r: flat.r, g: flat.g, b: flat.b, alpha: flat.alpha ?? 1 }
}

/** A colour with its alpha scaled, as `color-mix(in …, c p%, transparent)` and Tailwind's `/p` modifier paint it. */
export function fade(color: Color, ratio: number): Color {
  return { ...color, alpha: color.alpha * ratio }
}

/** WCAG 2.x contrast of `ink` painted over the opaque `surface`. */
export function contrastOn(ink: Color, surface: Color): number {
  return wcagContrast(over(ink, surface), surface)
}
