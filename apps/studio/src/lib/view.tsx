import { createContext, use, useState, type ReactNode } from "react"

/**
 * Business shows what a card means in the reader's words. Technical shows
 * only the Norn artefacts behind it: the typed definition, the laws, the
 * mapping, the policy decision, lineage and commit ids and the receipt.
 */
export type View = "business" | "technical"

const KEY = "norn.studio.view"

const ViewContext = createContext<{ readonly view: View; readonly setView: (view: View) => void }>({ view: "business", setView: () => {} })

/** The view kept from the last visit; a browser that blocks storage (it can throw on access) starts in Business. */
function stored(): View {
  try {
    return globalThis.localStorage?.getItem(KEY) === "technical" ? "technical" : "business"
  } catch {
    return "business"
  }
}

/** The reader's global view, kept for their next visit. */
export function ViewProvider({ children }: { readonly children: ReactNode }) {
  const [view, set] = useState<View>(stored)
  const setView = (next: View) => {
    set(next)
    // Blocked or full storage only means the choice is not kept for the next visit.
    try { globalThis.localStorage?.setItem(KEY, next) } catch { /* not kept */ }
  }
  return <ViewContext value={{ view, setView }}>{children}</ViewContext>
}

export const useView = () => use(ViewContext)
