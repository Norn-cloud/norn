import { LanguagePill, SchemeToggle } from "@/components/system/top-bar-controls"
import { useAppearance, usePaintedScheme } from "@/lib/appearance"
import { LOCALES, isLocale, useLocale } from "@/lib/locale"
import { RulesTickBar } from "@/rules-bar"

/** The language pill wired to the reader's locale (local view state, never a cell action). */
export function LanguageSwitch({ className }: { readonly className?: string }) {
  const { locale, setLocale } = useLocale()
  const current = LOCALES.find(({ tag }) => tag === locale) ?? LOCALES[0]!
  const next = LOCALES.find(({ tag }) => tag !== locale) ?? LOCALES[0]!
  return <LanguagePill className={className} current={current} next={next} onSwitch={(tag) => { if (isLocale(tag)) setLocale(tag) }} />
}

/** The scheme toggle wired to the stored scheme; a custom theme that pins one says so and wins. */
export function SchemeSwitch({ className }: { readonly className?: string }) {
  const { setScheme, customTheme } = useAppearance()
  const painted = usePaintedScheme()
  return <SchemeToggle className={className} painted={painted} pinnedBy={customTheme?.appearance === undefined ? undefined : customTheme.name}
    onToggle={() => setScheme(painted === "dark" ? "light" : "dark")} />
}

/**
 * The top bar's extras (DESIGN.md 9.1), in the spec's order: the rules tick bar (server state only),
 * the language pill and the scheme toggle. Standalone, so any shell places them; each takes the class
 * that lets it give way as the bar narrows (on a phone the "Language and view" menu holds the
 * language and the scheme instead).
 */
export function TopBarExtras({ tickBar, language, scheme }: { readonly tickBar?: string; readonly language?: string; readonly scheme?: string }) {
  return <>
    <RulesTickBar className={tickBar} />
    <LanguageSwitch className={language} />
    <SchemeSwitch className={scheme} />
  </>
}
