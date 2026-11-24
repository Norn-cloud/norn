import { Link } from "@tanstack/react-router"
import { MenuTrigger } from "@ui-sdk/react/menu"
import { Ellipsis, type LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { KitMenuContent, KitMenuItem, Menu } from "@/components/kit"
import { cn } from "@/lib/utils"

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
          <h1 lang={lang} className="min-w-0 text-title leading-[1.15] font-semibold tracking-[-0.025em] text-foreground [overflow-wrap:anywhere]">{title}</h1>
          {meta && <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">{meta}</div>}
        </div>
        {description && <p className="max-w-[70ch] text-sm leading-relaxed text-pretty text-muted-foreground">{description}</p>}
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

/**
 * A section of a page (design v4): one hairline frame on a subtly tinted card surface at the panel
 * radius (11px), a quiet header row (small muted icon, title, optional hint or control at the end),
 * then the list, table or figure flush inside. A panel inside a panel is a ruled section, never a
 * card in a card.
 */
export function Panel({ icon: Icon, label, action, children, className, id }: {
  icon?: LucideIcon
  label: ReactNode
  action?: ReactNode
  children: ReactNode
  className?: string
  id?: string
}) {
  return (
    <section id={id} data-slot="panel" className={cn("min-w-0 overflow-hidden rounded-xl border border-(--panel-border) bg-card shadow-(--panel-shadow)",
      // A panel inside another panel is a section of it, not a card in a card: one hairline above, no frame.
      "[[data-slot=well]_&]:rounded-none [[data-slot=well]_&]:border-x-0 [[data-slot=well]_&]:border-b-0 [[data-slot=well]_&]:bg-transparent [[data-slot=well]_&]:shadow-none [[data-slot=well]_&>header]:px-0",
      className)}>
      <header className="flex min-h-11 flex-wrap items-center gap-x-2 gap-y-0.5 px-4 py-2 text-sm">
        {Icon && <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />}
        {/* A title wraps (two lines at most) rather than losing its end; a hint goes to the next line when both don't fit. */}
        <h2 className="line-clamp-2 min-w-0 flex-1 basis-40 text-panel font-medium tracking-[-0.015em] text-foreground [overflow-wrap:anywhere]" title={typeof label === "string" ? label : undefined}>{label}</h2>
        {/* A hint (text) gives way on a phone; a control (an element) stays. */}
        {action && <div className={cn("ms-auto shrink-0 text-meta text-muted-foreground", typeof action === "string" && "max-sm:hidden")}>{action}</div>}
      </header>
      {children}
    </section>
  )
}

/** The body of a panel: flush with the card under one hairline, so a panel reads as one calm surface. */
export function Well({ children, className }: { children: ReactNode; className?: string }) {
  return <div data-slot="well" className={cn("border-t border-(--panel-border) bg-card", className)}>{children}</div>
}

/**
 * An empty well: an icon, a short title ("No data yet" unless given), a line saying why, and the
 * next step when there is one (load data, connect a source, add the first item).
 */
export function EmptyState({ icon: Icon, title = "No data yet", children, next }: { icon: LucideIcon; title?: ReactNode; children: ReactNode; next?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2.5 px-6 py-12 text-center">
      <span aria-hidden className="flex size-10 items-center justify-center rounded-xl border border-border/80 bg-surface-subtle text-muted-foreground">
        <Icon className="size-4.5" />
      </span>
      <div className="text-sm font-semibold text-foreground">{title}</div>
      <p className="max-w-xs text-meta leading-relaxed text-muted-foreground">{children}</p>
      {next && <div className="flex flex-wrap justify-center gap-2 pt-1.5">{next}</div>}
    </div>
  )
}
