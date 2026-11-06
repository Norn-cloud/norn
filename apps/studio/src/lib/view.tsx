import { createContext, use, useState, type ReactNode } from "react"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"

/**
 * Business shows what a card means in the reader's words. Technical shows
 * only the Norn artefacts behind it: the typed definition, the laws, the
 * mapping, the policy decision, lineage and commit ids and the receipt.
 */
export type View = "business" | "technical"

const KEY = "norn.studio.view"

const ViewContext = createContext<{ readonly view: View; readonly setView: (view: View) => void }>({ view: "business", setView: () => {} })

/** The reader's global view, kept for their next visit. */
export function ViewProvider({ children }: { readonly children: ReactNode }) {
  const [view, set] = useState<View>(() => (globalThis.localStorage?.getItem(KEY) === "technical" ? "technical" : "business"))
  const setView = (next: View) => { set(next); globalThis.localStorage?.setItem(KEY, next) }
  return <ViewContext value={{ view, setView }}>{children}</ViewContext>
}

export const useView = () => use(ViewContext)

/** One card's view: the global view, unless the reader switched this card. */
export function useCardView() {
  const global = use(ViewContext).view
  const [override, setOverride] = useState<View>()
  // An override equal to the global view is dropped, so the card follows the next global switch.
  return { view: override ?? global, overridden: override !== undefined && override !== global, setView: (next: View) => setOverride(next === global ? undefined : next) }
}

export function ViewToggle({ view, onChange, label = "View" }: { readonly view: View; readonly onChange: (view: View) => void; readonly label?: string }) {
  return (
    <ToggleGroup type="single" variant="outline" size="sm" aria-label={label} value={view} onValueChange={(next) => { if (next === "business" || next === "technical") onChange(next) }}>
      <ToggleGroupItem value="business" className="px-3">Business</ToggleGroupItem>
      <ToggleGroupItem value="technical" className="px-3">Technical</ToggleGroupItem>
    </ToggleGroup>
  )
}
