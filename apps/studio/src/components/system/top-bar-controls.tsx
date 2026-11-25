import { Moon, Sun } from "lucide-react"
import { Hint } from "./kit"
import { cn } from "@/lib/utils"

const pill = "flex h-[32px] shrink-0 items-center justify-center rounded-full text-body text-foreground ring-1 ring-input ring-inset outline-none transition-colors duration-150 hover:bg-(--well) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring motion-reduce:transition-none max-sm:h-[44px]"

export interface LanguageChoice {
  readonly tag: string
  /** The language's own name ("English", "العربية"). */
  readonly name: string
  /** Its name in the language the page is in now, for the hint ("Arabic"). */
  readonly hint?: string
}

/**
 * The top bar's language pill (DESIGN.md 9.1): it shows the other language in its own script and
 * switches to it. The visible name is part of the accessible name, set in its own `lang` so a
 * screen reader voices it correctly. Local view state, never a cell action (DESIGN.md 16.1).
 */
export function LanguagePill({ current, next, onSwitch, className }: {
  readonly current: LanguageChoice
  readonly next: LanguageChoice
  readonly onSwitch: (tag: string) => void
  readonly className?: string
}) {
  return (
    <Hint side="bottom" label={`Show in ${next.hint ?? next.name}`}>
      <button type="button" data-slot="language-pill" data-locale={current.tag} onClick={() => onSwitch(next.tag)}
        className={cn(pill, "px-3", className)}>
        <span className="sr-only">Language: {current.name}. Switch to </span>
        <span lang={next.tag}>{next.name}</span>
      </button>
    </Hint>
  )
}

/**
 * The top bar's scheme toggle (DESIGN.md 9.1): an icon pill that shows the scheme it switches to.
 * When a custom theme pins a scheme it says so and stays put, instead of a silent disabled button.
 */
export function SchemeToggle({ painted, pinnedBy, onToggle, className }: {
  /** The scheme on screen now. */
  readonly painted: "light" | "dark"
  /** The custom theme that pins the scheme, if one does. */
  readonly pinnedBy?: string
  readonly onToggle: () => void
  readonly className?: string
}) {
  const target = painted === "dark" ? "light" : "dark"
  const name = pinnedBy !== undefined ? `Colour scheme: ${painted}, set by the theme “${pinnedBy}”` : `Switch to ${target} mode`
  const Icon = painted === "dark" ? Sun : Moon
  return (
    <Hint side="bottom" label={name}>
      <button type="button" data-slot="scheme-toggle" data-scheme={painted} aria-label={name}
        aria-disabled={pinnedBy !== undefined || undefined}
        onClick={() => { if (pinnedBy === undefined) onToggle() }}
        className={cn(pill, "size-[32px] text-muted-foreground hover:text-foreground aria-disabled:cursor-default aria-disabled:hover:bg-transparent aria-disabled:hover:text-muted-foreground max-sm:size-[44px] [&_svg]:size-4", className)}>
        <Icon aria-hidden strokeWidth={1.6} />
      </button>
    </Hint>
  )
}
