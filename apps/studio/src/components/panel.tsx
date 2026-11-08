import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * A white card with a quiet header row (small muted icon, label, optional
 * action at the end) around an inset well that holds the figure, list or chart.
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
    <section id={id} data-slot="panel" className={cn("min-w-0 rounded-lg border bg-card p-1.5", className)}>
      <header className="flex h-9 items-center gap-2 px-2.5 text-sm">
        {Icon && <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />}
        <h2 className="min-w-0 truncate font-medium">{label}</h2>
        {action && <div className="ms-auto shrink-0 text-meta text-muted-foreground max-sm:hidden">{action}</div>}
      </header>
      {children}
    </section>
  )
}

/** The inset well inside a panel: a slightly grey fill with its own hairline and smaller radius. */
export function Well({ children, className }: { children: ReactNode; className?: string }) {
  return <div data-slot="well" className={cn("rounded-md border bg-surface-subtle", className)}>{children}</div>
}

/** What a well shows when the server has nothing for it: an icon, "No data yet" and one plain sentence. */
/** An empty section: what is missing, in the section's own words (`title`), and what to do about it. */
export function EmptyState({ icon: Icon, title = "No data yet", children }: { icon: LucideIcon; title?: string; children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
      <Icon aria-hidden className="size-5 text-muted-foreground" />
      <div className="text-sm font-medium">{title}</div>
      <p className="max-w-xs text-meta text-muted-foreground">{children}</p>
    </div>
  )
}
