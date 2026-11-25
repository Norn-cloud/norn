import { Link } from "@tanstack/react-router"
import { MenuTrigger } from "@ui-sdk/react/menu"
import { Ellipsis, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { KitMenuContent, KitMenuItem, Menu } from "@/components/system/kit"

export interface PageAction {
  readonly label: string
  readonly icon?: LucideIcon
  /** A route to open, or `onSelect` to run; one of the two. */
  readonly to?: string
  readonly onSelect?: () => void
}

/**
 * A page's head, the one pattern every page uses: its title (with small facts beside it), one line
 * on what the page is for, then on the end side at most one primary action, the page's own controls,
 * and every other action behind one overflow menu. Breadcrumbs live in the shell's bar above it.
 */
export function PageHeader({ title, description, actions, primary, secondary, meta, lang }: {
  title: ReactNode
  description?: ReactNode
  /** The page's own controls (a toggle, a refresh): shown as they are, before the overflow. */
  actions?: ReactNode
  /** The one primary action: a solid button. A page has at most one. */
  primary?: ReactNode
  /** Everything else the page offers, in an overflow menu so the header stays quiet. */
  secondary?: readonly PageAction[]
  /** Small facts beside the title: a count, a status, an id. */
  meta?: ReactNode
  lang?: string
}) {
  const end = actions !== undefined || primary !== undefined || (secondary?.length ?? 0) > 0
  return (
    <header data-slot="page-header" className="flex flex-wrap items-start justify-between gap-x-6 gap-y-3">
      <div className="min-w-0 flex-1 basis-80 space-y-1.5">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
          <h1 lang={lang} className="min-w-0 text-title font-medium text-foreground [overflow-wrap:anywhere]">{title}</h1>
          {meta && <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">{meta}</div>}
        </div>
        {description && <p className="max-w-[70ch] text-body leading-relaxed text-pretty text-muted-foreground">{description}</p>}
      </div>
      {end && (
        <div className="flex shrink-0 flex-wrap items-center gap-2">
          {actions}
          {secondary && secondary.length > 0 && <Overflow actions={secondary} />}
          {primary}
        </div>
      )}
    </header>
  )
}

/** The header's overflow: every secondary action, one menu behind a quiet "More" button. */
function Overflow({ actions }: { readonly actions: readonly PageAction[] }) {
  return (
    <Menu onAction={(value) => actions.find(({ label }) => label === value)?.onSelect?.()}>
      <MenuTrigger aria-label="More actions" className="flex size-8 items-center justify-center rounded-lg border border-transparent text-muted-foreground transition-colors outline-none hover:border-border/80 hover:bg-surface-subtle hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring data-open:border-border/80 data-open:bg-surface-subtle data-open:text-foreground">
        <Ellipsis aria-hidden className="size-4" />
      </MenuTrigger>
      <KitMenuContent align="end">
        {actions.map(({ label, icon: Icon, to }) => (
          <KitMenuItem key={label} value={label} label={label} render={to === undefined ? undefined : <Link to={to} />}>
            {Icon && <Icon aria-hidden />}{label}
          </KitMenuItem>
        ))}
      </KitMenuContent>
    </Menu>
  )
}
