import { createContext, use, useState, type ReactNode } from "react"

/**
 * The reader's perspective on the one workspace. Workspace is the operator's: home, inbox, records,
 * metrics, dashboards and workflows. Builder is the modeller's: sources and mappings, transforms, the
 * model, the canvas, rules, markings and connections. It chooses what the navigation lists and how
 * dense rows are; it is never a permission boundary. The server decides every read and action, and a
 * page outside the perspective still opens from search or a link.
 */
export type Perspective = "workspace" | "builder"

export const PERSPECTIVES: readonly { readonly id: Perspective; readonly label: string; readonly home: string; readonly density: "comfortable" | "compact" }[] = [
  { id: "workspace", label: "Workspace", home: "/", density: "comfortable" },
  { id: "builder", label: "Builder", home: "/sources", density: "compact" },
]

export const isPerspective = (value: unknown): value is Perspective => value === "workspace" || value === "builder"

const KEY = "norn.studio.perspective"

const PerspectiveContext = createContext<{ readonly perspective: Perspective; readonly setPerspective: (next: Perspective) => void }>({ perspective: "workspace", setPerspective: () => {} })

/** The perspective kept from the last visit; a browser that blocks storage (it can throw on access) starts in Workspace. */
function stored(): Perspective {
  try {
    const value = globalThis.localStorage?.getItem(KEY)
    return isPerspective(value) ? value : "workspace"
  } catch {
    return "workspace"
  }
}

/** The reader's perspective, kept for their next visit. */
export function PerspectiveProvider({ children }: { readonly children: ReactNode }) {
  const [perspective, set] = useState<Perspective>(stored)
  const setPerspective = (next: Perspective) => {
    set(next)
    // Blocked or full storage only means the choice is not kept for the next visit.
    try { globalThis.localStorage?.setItem(KEY, next) } catch { /* not kept */ }
  }
  return <PerspectiveContext value={{ perspective, setPerspective }}>{children}</PerspectiveContext>
}

export const usePerspective = () => use(PerspectiveContext)
