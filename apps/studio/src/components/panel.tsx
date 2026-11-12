import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * A page's head: its title, one line on what the page is for, and the page's own actions. Every
 * page uses it, so titles share one size, weight and rhythm.
 */
export function PageHeader({ title, description, actions, meta, lang }: {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  /** Small facts beside the title: a count, a status, an id. */
  meta?: ReactNode
  lang?: string
}) {
  return (
    <header className="flex flex-wrap items-end justify-between gap-x-6 gap-y-3 pb-1">
      <div className="min-w-0 space-y-1.5">
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <h1 lang={lang} className="min-w-0 text-title font-semibold tracking-tight [overflow-wrap:anywhere]">{title}</h1>
          {meta && <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground">{meta}</div>}
        </div>
        {description && <p className="max-w-[68ch] text-pretty text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
    </header>
  )
}

/**
 * A raised card with a quiet header row (small muted icon, label, optional
 * action at the end) around an inset well that holds the figure, list or chart.
 * It sits on the page with the shared soft depth: a hairline, a top highlight
 * and a low shadow, the same in every page.
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
    <section id={id} data-slot="panel" className={cn("min-w-0 rounded-node border border-node-border bg-card p-1.5 shadow-[var(--node-highlight),var(--elevation-1)]", className)}>
      <header className="flex min-h-10 items-center gap-2 px-2.5 text-sm">
        {Icon && <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />}
        <h2 className="min-w-0 truncate text-panel font-medium tracking-tight">{label}</h2>
        {/* A hint (text) gives way on a phone; a control (an element) stays. */}
        {action && <div className={cn("ms-auto shrink-0 text-meta text-muted-foreground", typeof action === "string" && "max-sm:hidden")}>{action}</div>}
      </header>
      {children}
    </section>
  )
}

/** The inset well inside a panel: a slightly grey fill with its own hairline and smaller radius. */
export function Well({ children, className }: { children: ReactNode; className?: string }) {
  return <div data-slot="well" className={cn("rounded-[calc(var(--node-radius)-6px)] border border-border/70 bg-surface-subtle", className)}>{children}</div>
}

/** An empty well: an icon, a short title ("No data yet" unless given) and a line saying why. */
export function EmptyState({ icon: Icon, title = "No data yet", children }: { icon: LucideIcon; title?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
      <span className="flex size-9 items-center justify-center rounded-full border border-node-border bg-card shadow-[var(--node-highlight),var(--elevation-1)]"><Icon aria-hidden className="size-4 text-muted-foreground" /></span>
      <div className="text-sm font-medium">{title}</div>
      <p className="max-w-xs text-meta text-muted-foreground">{children}</p>
    </div>
  )
}
