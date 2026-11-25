import { createContext, use, useEffect, useState, useSyncExternalStore, type ReactNode } from "react"
import {
  applyCustomTheme,
  effectiveBaseSkin,
  persistCustomTheme,
  storedCustomTheme,
  type StudioTheme,
} from "./studio-theme.ts"

/**
 * Swappable visual skin applied to the document root (`data-skin`).
 * - `polar`: strict Polar / Linear monochrome surfaces (black/grey/white) with colour reserved for data, status, badges, CTAs, and the knot (default).
 * - `open`: airy open-canvas surface with double-shell cards and crisp blue accent.
 * - `luma`: two-surface architectural mist shell with knot-violet accent.
 * - `glass`: translucent liquid-glass surfaces with backdrop blur and specular hairlines.
 */
export type Skin = "polar" | "open" | "luma" | "glass"

/**
 * Swappable chrome layout applied to the shell (`data-shell`), DESIGN.md section 9.
 * - `sidebar`: the thin source-list sidebar, always the default.
 * - `dock`: the floating dark pill dock, opt-in only, for kiosk or mobile-first surfaces an app chooses.
 */
export type ShellChrome = "sidebar" | "dock"

export const SKINS: readonly {
  readonly id: Skin
  readonly label: string
  readonly detail: string
  readonly swatch: string
}[] = [
  { id: "polar", label: "Polar", detail: "Monochrome ink and flat hairlines", swatch: "#18181b" },
  { id: "open", label: "Open", detail: "Double-shell white canvas, blue accent", swatch: "#3a6df0" },
  { id: "luma", label: "Luma", detail: "Two-surface mist and knot violet", swatch: "#554ce8" },
  { id: "glass", label: "Glass", detail: "Translucent frosted liquid glass", swatch: "#dbe6ff" },
]

export const SHELLS: readonly {
  readonly id: ShellChrome
  readonly label: string
  readonly detail: string
}[] = [
  { id: "sidebar", label: "Sidebar", detail: "Thin source-list sidebar" },
  { id: "dock", label: "Floating dock", detail: "Full-bleed canvas with bottom dock" },
]

/**
 * The reader's colour scheme: `system` follows the OS (the default); `light` and `dark` pin it. A
 * custom theme that sets its own `appearance` wins over all three while it is applied.
 */
export type Scheme = "system" | "light" | "dark"

export const SCHEMES: readonly { readonly id: Scheme; readonly label: string }[] = [
  { id: "system", label: "Match system" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
]

const SKIN_KEY = "norn.studio.skin"
const SHELL_KEY = "norn.studio.shell"
const SCHEME_KEY = "norn.studio.scheme"

export const isScheme = (value: unknown): value is Scheme => value === "system" || value === "light" || value === "dark"

/** The scheme kept from the last visit, else `system`. */
export function storedScheme(): Scheme {
  try {
    const kept = globalThis.localStorage?.getItem(SCHEME_KEY)
    if (isScheme(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return "system"
}

export const isSkin = (value: unknown): value is Skin =>
  value === "polar" || value === "open" || value === "luma" || value === "glass"

export const isShellChrome = (value: unknown): value is ShellChrome =>
  value === "sidebar" || value === "dock"

/** The skin kept from the last visit, else the surface's default, else the monochrome Polar default. */
export function storedSkin(surfaceDefault: Skin = "polar"): Skin {
  try {
    const kept = globalThis.localStorage?.getItem(SKIN_KEY)
    if (isSkin(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return surfaceDefault
}

/**
 * The shell chrome: the reader's choice kept from the last visit, else the surface's own default
 * (an app may opt a kiosk or mobile-first surface into the dock), else the sidebar. A reader's stored
 * choice always wins over a default, including a surface's dock opt-in (lead ruling, 2026-11-25).
 */
export function storedShellChrome(surfaceDefault: ShellChrome = "sidebar"): ShellChrome {
  try {
    const kept = globalThis.localStorage?.getItem(SHELL_KEY)
    if (isShellChrome(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return surfaceDefault
}

/** Resolve OS dark-mode preference when no custom theme forces an explicit appearance. */
function systemPrefersDark(): boolean {
  if (typeof globalThis.window === "undefined" || typeof globalThis.window.matchMedia !== "function") return false
  return globalThis.window.matchMedia("(prefers-color-scheme: dark)").matches
}

/** Whether a scheme paints dark now: pinned, or the OS preference for `system`. */
export function schemeIsDark(scheme: Scheme): boolean {
  return scheme === "dark" || (scheme === "system" && systemPrefersDark())
}

/** Paint the reader's scheme (`.dark`), unless a custom theme pins its own appearance. */
export function applyScheme(scheme: Scheme, root: HTMLElement | undefined = globalThis.document?.documentElement) {
  if (!root) return
  root.dataset.scheme = scheme
  const forced = root.dataset.forcedAppearance
  if (forced === "dark" || forced === "light") return
  root.classList?.toggle("dark", schemeIsDark(scheme))
}

/** Apply `data-skin`, `data-shell`, `.dark` (honoring forced custom theme appearance), and any custom JSON theme tokens. */
export function applyAppearance(
  skin: Skin,
  shell: ShellChrome,
  root: HTMLElement | undefined = globalThis.document?.documentElement,
  customTheme: StudioTheme | null = null,
) {
  if (!root) return
  root.dataset.skin = customTheme ? effectiveBaseSkin(customTheme) : skin
  root.dataset.shell = shell
  if (typeof root.classList?.toggle === "function") {
    if (customTheme?.appearance === "dark") {
      root.classList.toggle("dark", true)
    } else if (customTheme?.appearance === "light") {
      root.classList.toggle("dark", false)
    } else if (root.dataset.forcedAppearance !== undefined) {
      // The theme no longer pins a scheme: back to the reader's own.
      root.classList.toggle("dark", schemeIsDark(storedScheme()))
    }
  }
  if (customTheme?.appearance) {
    root.dataset.forcedAppearance = customTheme.appearance
  } else {
    delete root.dataset.forcedAppearance
  }
  const isDark = root.classList?.contains("dark") ?? false
  applyCustomTheme(customTheme, isDark, root)
}

interface AppearanceContextValue {
  readonly scheme: Scheme
  readonly setScheme: (scheme: Scheme) => void
  readonly skin: Skin
  readonly setSkin: (skin: Skin) => void
  readonly shell: ShellChrome
  readonly setShell: (shell: ShellChrome) => void
  readonly customTheme: StudioTheme | null
  readonly setCustomTheme: (theme: StudioTheme) => void
  readonly clearCustomTheme: () => void
}

const AppearanceContext = createContext<AppearanceContextValue>({
  scheme: "system",
  setScheme: () => {},
  skin: "polar",
  setSkin: () => {},
  shell: "sidebar",
  setShell: () => {},
  customTheme: null,
  setCustomTheme: () => {},
  clearCustomTheme: () => {},
})

export function AppearanceProvider({
  children,
  initialSkin,
  initialShell,
  initialCustomTheme,
}: {
  readonly children: ReactNode
  readonly initialSkin?: Skin
  readonly initialShell?: ShellChrome
  readonly initialCustomTheme?: StudioTheme | null
}) {
  // `initialSkin` and `initialShell` are the surface's defaults; the reader's stored choice wins over them.
  const [skin, setSkinState] = useState<Skin>(() => storedSkin(initialSkin))
  const [shell, setShellState] = useState<ShellChrome>(() => storedShellChrome(initialShell))
  const [scheme, setSchemeState] = useState<Scheme>(storedScheme)
  const [customTheme, setCustomThemeState] = useState<StudioTheme | null>(
    () => (initialCustomTheme !== undefined ? initialCustomTheme : storedCustomTheme()),
  )

  useEffect(() => {
    applyAppearance(skin, shell, undefined, customTheme)
  }, [skin, shell, customTheme])

  // Keep custom JSON theme light/dark variants synced when the document root toggles `.dark`.
  useEffect(() => {
    const root = globalThis.document?.documentElement
    if (!root || !customTheme || typeof MutationObserver === "undefined") return
    const observer = new MutationObserver(() => {
      applyCustomTheme(customTheme, root.classList.contains("dark"), root)
    })
    observer.observe(root, { attributes: true, attributeFilter: ["class"] })
    return () => observer.disconnect()
  }, [customTheme])

  const setSkin = (next: Skin) => {
    setSkinState(next)
    setCustomThemeState(null)
    persistCustomTheme(null)
    applyAppearance(next, shell, undefined, null)
    try { globalThis.localStorage?.setItem(SKIN_KEY, next) } catch { /* not kept */ }
  }

  const setScheme = (next: Scheme) => {
    setSchemeState(next)
    try { globalThis.localStorage?.setItem(SCHEME_KEY, next) } catch { /* not kept */ }
    applyScheme(next)
  }

  const setShell = (next: ShellChrome) => {
    setShellState(next)
    applyAppearance(skin, next, undefined, customTheme)
    try { globalThis.localStorage?.setItem(SHELL_KEY, next) } catch { /* not kept */ }
  }

  const setCustomTheme = (theme: StudioTheme) => {
    const nextSkin = effectiveBaseSkin(theme)
    setSkinState(nextSkin)
    setCustomThemeState(theme)
    persistCustomTheme(theme)
    applyAppearance(nextSkin, shell, undefined, theme)
    try { globalThis.localStorage?.setItem(SKIN_KEY, nextSkin) } catch { /* not kept */ }
  }

  const clearCustomTheme = () => {
    setCustomThemeState(null)
    persistCustomTheme(null)
    applyAppearance(skin, shell, undefined, null)
  }

  return (
    <AppearanceContext value={{ scheme, setScheme, skin, setSkin, shell, setShell, customTheme, setCustomTheme, clearCustomTheme }}>
      {children}
    </AppearanceContext>
  )
}

export const useAppearance = () => use(AppearanceContext)

/** Follow the scheme on screen (`.dark` on the root), whoever set it: the reader, the OS or a theme. */
function subscribePainted(onChange: () => void): () => void {
  const root = globalThis.document?.documentElement
  if (!root || typeof MutationObserver === "undefined") return () => {}
  const observer = new MutationObserver(onChange)
  observer.observe(root, { attributes: true, attributeFilter: ["class"] })
  return () => observer.disconnect()
}

export function usePaintedScheme(): "light" | "dark" {
  return useSyncExternalStore(
    subscribePainted,
    () => (globalThis.document?.documentElement.classList.contains("dark") ? "dark" : "light"),
    () => "light",
  )
}
