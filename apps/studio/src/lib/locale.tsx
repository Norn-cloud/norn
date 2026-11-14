import { createContext, use, useEffect, useState, type ReactNode } from "react"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"

/**
 * The reader's language. Ids, codes and evidence never change with it; only
 * labels, messages and layout direction do (LABEL-LOCALE-1).
 */
export type Locale = "en" | "ar"

export const LOCALES: readonly { readonly tag: Locale; readonly name: string; readonly dir: "ltr" | "rtl" }[] = [
  { tag: "en", name: "English", dir: "ltr" },
  { tag: "ar", name: "العربية", dir: "rtl" },
]

const KEY = "norn.studio.locale"

export const isLocale = (value: unknown): value is Locale => value === "en" || value === "ar"

export const directionOf = (locale: Locale): "ltr" | "rtl" => LOCALES.find(({ tag }) => tag === locale)?.dir ?? "ltr"

/** The locale kept from the last visit, else the browser's; blocked storage starts from the browser's. */
export function storedLocale(): Locale {
  try {
    const kept = globalThis.localStorage?.getItem(KEY)
    if (isLocale(kept)) return kept
  } catch { /* storage blocked: fall through */ }
  return globalThis.navigator?.language?.toLowerCase().startsWith("ar") ? "ar" : "en"
}

/** Set the document's language and direction, so the browser mirrors layout and shapes text. */
export function applyLocale(locale: Locale, root: HTMLElement | undefined = globalThis.document?.documentElement) {
  if (!root) return
  root.lang = locale
  root.dir = directionOf(locale)
}

const LocaleContext = createContext<{ readonly locale: Locale; readonly setLocale: (locale: Locale) => void }>({ locale: "en", setLocale: () => {} })

export function LocaleProvider({ children, initial }: { readonly children: ReactNode; readonly initial?: Locale }) {
  const [locale, set] = useState<Locale>(() => initial ?? storedLocale())
  useEffect(() => applyLocale(locale), [locale])
  const setLocale = (next: Locale) => {
    set(next)
    // Blocked or full storage only means the choice is not kept for the next visit.
    try { globalThis.localStorage?.setItem(KEY, next) } catch { /* not kept */ }
  }
  return <LocaleContext value={{ locale, setLocale }}>{children}</LocaleContext>
}

export const useLocale = () => use(LocaleContext)

export function LocaleToggle({ locale, onChange }: { readonly locale: Locale; readonly onChange: (locale: Locale) => void }) {
  return (
    <ToggleGroup type="single" variant="outline" size="sm" aria-label="Language" value={locale} onValueChange={(next) => { if (isLocale(next)) onChange(next) }}>
      {LOCALES.map(({ tag, name }) => <ToggleGroupItem key={tag} value={tag} lang={tag} className="px-3">{name}</ToggleGroupItem>)}
    </ToggleGroup>
  )
}
