import { createContext, use, useEffect, useState, type ReactNode } from "react"
import {
  applyCustomTheme,
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
 * Swappable chrome layout applied to the shell (`data-shell`).
 * - `sidebar`: collapsible Spaces sidebar and navigation rail (default).
 * - `dock`: full-bleed open canvas with a centered floating dark pill dock at the bottom.
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
  { id: "sidebar", label: "Sidebar", detail: "Spaces sidebar and rail" },
  { id: "dock", label: "Floating dock", detail: "Full-bleed canvas with bottom dock" },
]

const SKIN_KEY = "norn.studio.skin"
const SHELL_KEY = "norn.studio.shell"

export const isSkin = (value: unknown): value is Skin =>
  value === "polar" || value === "open" || value === "luma" || value === "glass"

export const isShellChrome = (value: unknown): value is ShellChrome =>
  value === "sidebar" || value === "dock"

/** The skin kept from the last visit, else the monochrome Polar default. */
export function storedSkin(): Skin {
  try {
    const kept = globalThis.localStorage?.getItem(SKIN_KEY)
    if (isSkin(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return "polar"
}

/** The shell chrome kept from the last visit, else the sidebar workbench. */
export function storedShellChrome(): ShellChrome {
  try {
    const kept = globalThis.localStorage?.getItem(SHELL_KEY)
    if (isShellChrome(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return "sidebar"
}

/** Apply `data-skin`, `data-shell`, and any active custom JSON theme onto the document root. */
export function applyAppearance(
  skin: Skin,
  shell: ShellChrome,
  root: HTMLElement | undefined = globalThis.document?.documentElement,
  customTheme: StudioTheme | null = null,
) {
  if (!root) return
  root.dataset.skin = customTheme?.baseSkin ?? skin
  root.dataset.shell = shell
  const isDark = root.classList?.contains("dark") ?? false
  applyCustomTheme(customTheme, isDark, root)
}

interface AppearanceContextValue {
  readonly skin: Skin
  readonly setSkin: (skin: Skin) => void
  readonly shell: ShellChrome
  readonly setShell: (shell: ShellChrome) => void
  readonly customTheme: StudioTheme | null
  readonly setCustomTheme: (theme: StudioTheme) => void
  readonly clearCustomTheme: () => void
}

const AppearanceContext = createContext<AppearanceContextValue>({
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
  const [skin, setSkinState] = useState<Skin>(() => initialSkin ?? storedSkin())
  const [shell, setShellState] = useState<ShellChrome>(() => initialShell ?? storedShellChrome())
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

  const setShell = (next: ShellChrome) => {
    setShellState(next)
    applyAppearance(skin, next, undefined, customTheme)
    try { globalThis.localStorage?.setItem(SHELL_KEY, next) } catch { /* not kept */ }
  }

  const setCustomTheme = (theme: StudioTheme) => {
    const nextSkin = theme.baseSkin ?? skin
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
    <AppearanceContext value={{ skin, setSkin, shell, setShell, customTheme, setCustomTheme, clearCustomTheme }}>
      {children}
    </AppearanceContext>
  )
}

export const useAppearance = () => use(AppearanceContext)
