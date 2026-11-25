import { createContext, use, useCallback, useEffect, useState, useSyncExternalStore, type ReactNode } from "react"
import { applyCustomTheme, effectiveBaseSkin, type StudioTheme } from "./studio-theme.ts"
import type { ServedTheme } from "./workspace-theme.ts"

/**
 * Swappable visual skin applied to the document root (`data-skin`).
 * - `luma`: shadcn's Luma style (base-luma): its neutral theme, pill controls and rounded cards, on opaque monochrome islands (default; owner decision, 2026-11-25).
 * - `polar`: strict Polar / Linear monochrome surfaces (black/grey/white) with colour reserved for data, status, badges, CTAs, and the knot.
 * - `open`: airy open-canvas surface with double-shell cards and an ink accent (monochrome chrome, DESIGN.md 19.6).
 * - `glass`: translucent liquid-glass surfaces with backdrop blur and specular hairlines.
 */
export type Skin = "polar" | "open" | "luma" | "glass"

/**
 * Swappable chrome layout applied to the shell (`data-shell`), DESIGN.md section 9.
 * - `sidebar`: the three-island shell (applet rail, submenu island, main island), always the default.
 * - `dock`: the floating dark pill dock, opt-in only, for kiosk or mobile-first surfaces an app chooses.
 */
export type ShellChrome = "sidebar" | "dock"

export const SKINS: readonly {
  readonly id: Skin
  readonly label: string
  readonly detail: string
  readonly swatch: string
}[] = [
  { id: "luma", label: "Luma", detail: "shadcn Luma: neutral and rounded, on opaque islands", swatch: "#171717" },
  { id: "polar", label: "Polar", detail: "Monochrome ink and flat hairlines", swatch: "#18181b" },
  { id: "open", label: "Open", detail: "Double-shell white canvas, ink accent", swatch: "#fafafa" },
  { id: "glass", label: "Glass", detail: "Translucent frosted liquid glass", swatch: "#e4e4e7" },
]

export const SHELLS: readonly {
  readonly id: ShellChrome
  readonly label: string
  readonly detail: string
}[] = [
  { id: "sidebar", label: "Islands", detail: "Applet rail, submenu and main island" },
  { id: "dock", label: "Floating dock", detail: "Full-bleed canvas with bottom dock" },
]

/**
 * The reader's colour scheme: `system` follows the OS (the default); `light` and `dark` pin it. A
 * workspace theme that sets its own `appearance` wins over all three while it is applied.
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

/** The skin kept from the last visit, else the surface's default, else the luma default. */
export function storedSkin(surfaceDefault: Skin = "luma"): Skin {
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

/** Resolve OS dark-mode preference when no workspace theme forces an explicit appearance. */
function systemPrefersDark(): boolean {
  if (typeof globalThis.window === "undefined" || typeof globalThis.window.matchMedia !== "function") return false
  return globalThis.window.matchMedia("(prefers-color-scheme: dark)").matches
}

/** Whether a scheme paints dark now: pinned, or the OS preference for `system`. */
export function schemeIsDark(scheme: Scheme): boolean {
  return scheme === "dark" || (scheme === "system" && systemPrefersDark())
}

/** Paint the reader's scheme (`.dark`), unless a workspace theme pins its own appearance. */
export function applyScheme(scheme: Scheme, root: HTMLElement | undefined = globalThis.document?.documentElement) {
  if (!root) return
  root.dataset.scheme = scheme
  const forced = root.dataset.forcedAppearance
  if (forced === "dark" || forced === "light") return
  root.classList?.toggle("dark", schemeIsDark(scheme))
}

/** Apply `data-skin`, `data-shell`, `.dark` (honoring a workspace theme's pinned appearance), and the workspace theme's tokens. */
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
  /** The workspace theme painted now: the served theme when the shared gate admits it, else none (the built-in default). */
  readonly customTheme: StudioTheme | null
  /** The workspace's served theme as last read: its version (for the next apply), or why it does not paint. */
  readonly served: ServedTheme
  /** Paint what the server serves for the shown workspace (the served-theme feed calls this). */
  readonly setServed: (served: ServedTheme) => void
}

const AppearanceContext = createContext<AppearanceContextValue>({
  scheme: "system",
  setScheme: () => {},
  skin: "luma",
  setSkin: () => {},
  shell: "sidebar",
  setShell: () => {},
  customTheme: null,
  served: { kind: "loading" },
  setServed: () => {},
})

/**
 * Skin, shell and scheme are the reader's own (kept in this browser). The theme is the workspace's:
 * the server serves it (`workspace.theme`), and while one paints it decides the skin and any pinned
 * appearance; with none, the reader's skin paints (the built-in Luma by default).
 */
export function AppearanceProvider({
  children,
  initialSkin,
  initialShell,
  initialServed,
}: {
  readonly children: ReactNode
  readonly initialSkin?: Skin
  readonly initialShell?: ShellChrome
  readonly initialServed?: ServedTheme
}) {
  // `initialSkin` and `initialShell` are the surface's defaults; the reader's stored choice wins over them.
  const [skin, setSkinState] = useState<Skin>(() => storedSkin(initialSkin))
  const [shell, setShellState] = useState<ShellChrome>(() => storedShellChrome(initialShell))
  const [scheme, setSchemeState] = useState<Scheme>(storedScheme)
  const [served, setServedState] = useState<ServedTheme>(initialServed ?? { kind: "loading" })
  const customTheme = served.kind === "theme" ? served.theme : null

  useEffect(() => {
    applyAppearance(skin, shell, undefined, customTheme)
  }, [skin, shell, customTheme])

  // Keep the workspace theme's light/dark variants synced when the document root toggles `.dark`.
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
    applyAppearance(next, shell, undefined, customTheme)
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

  // A repeat read of the same version keeps the same object, so the page repaints only on a change.
  const setServed = useCallback((next: ServedTheme) => {
    setServedState((current) => sameServed(current, next) ? current : next)
  }, [])

  return (
    <AppearanceContext value={{ scheme, setScheme, skin, setSkin, shell, setShell, customTheme, served, setServed }}>
      {children}
    </AppearanceContext>
  )
}

const sameServed = (a: ServedTheme, b: ServedTheme): boolean => JSON.stringify(a) === JSON.stringify(b)

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
