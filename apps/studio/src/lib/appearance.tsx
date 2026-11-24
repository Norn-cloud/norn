import { createContext, use, useEffect, useState, type ReactNode } from "react"

/**
 * Swappable visual skin applied to the document root (`data-skin`).
 * - `open`: airy open-canvas surface with whisper hairlines and minimal nesting (default).
 * - `polar`: deep carbon surfaces with crisp white hierarchy and precision metallic hairlines.
 * - `luma`: two-surface architectural mist shell with knot-violet accent.
 * - `glass`: translucent liquid-glass surfaces with backdrop blur and specular hairlines.
 */
export type Skin = "open" | "polar" | "luma" | "glass"

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
  { id: "open", label: "Open", detail: "Airy pure-white open canvas", swatch: "#ffffff" },
  { id: "polar", label: "Polar", detail: "Deep carbon precision surfaces", swatch: "#151518" },
  { id: "luma", label: "Luma", detail: "Two-surface mist and violet", swatch: "#f3f4f6" },
  { id: "glass", label: "Glass", detail: "Translucent liquid glass", swatch: "#e8edff" },
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
  value === "open" || value === "polar" || value === "luma" || value === "glass"

export const isShellChrome = (value: unknown): value is ShellChrome =>
  value === "sidebar" || value === "dock"

/** The skin kept from the last visit, else the open-canvas default. */
export function storedSkin(): Skin {
  try {
    const kept = globalThis.localStorage?.getItem(SKIN_KEY)
    if (isSkin(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return "open"
}

/** The shell chrome kept from the last visit, else the sidebar workbench. */
export function storedShellChrome(): ShellChrome {
  try {
    const kept = globalThis.localStorage?.getItem(SHELL_KEY)
    if (isShellChrome(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return "sidebar"
}

/** Apply `data-skin` and `data-shell` attributes onto the document root so tokens cascade everywhere. */
export function applyAppearance(
  skin: Skin,
  shell: ShellChrome,
  root: HTMLElement | undefined = globalThis.document?.documentElement,
) {
  if (!root) return
  root.dataset.skin = skin
  root.dataset.shell = shell
}

interface AppearanceContextValue {
  readonly skin: Skin
  readonly setSkin: (skin: Skin) => void
  readonly shell: ShellChrome
  readonly setShell: (shell: ShellChrome) => void
}

const AppearanceContext = createContext<AppearanceContextValue>({
  skin: "open",
  setSkin: () => {},
  shell: "sidebar",
  setShell: () => {},
})

export function AppearanceProvider({
  children,
  initialSkin,
  initialShell,
}: {
  readonly children: ReactNode
  readonly initialSkin?: Skin
  readonly initialShell?: ShellChrome
}) {
  const [skin, setSkinState] = useState<Skin>(() => initialSkin ?? storedSkin())
  const [shell, setShellState] = useState<ShellChrome>(() => initialShell ?? storedShellChrome())

  useEffect(() => {
    applyAppearance(skin, shell)
  }, [skin, shell])

  const setSkin = (next: Skin) => {
    setSkinState(next)
    applyAppearance(next, shell)
    try { globalThis.localStorage?.setItem(SKIN_KEY, next) } catch { /* not kept */ }
  }

  const setShell = (next: ShellChrome) => {
    setShellState(next)
    applyAppearance(skin, next)
    try { globalThis.localStorage?.setItem(SHELL_KEY, next) } catch { /* not kept */ }
  }

  return (
    <AppearanceContext value={{ skin, setSkin, shell, setShell }}>
      {children}
    </AppearanceContext>
  )
}

export const useAppearance = () => use(AppearanceContext)
