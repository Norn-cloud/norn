import type { LucideIcon } from "lucide-react"
import { createContext, use, type ReactNode } from "react"
import { cn } from "@/lib/utils"

/** True inside a Panel, so a part that must sit in one (a table) knows whether it already does. */
const InPanel = createContext(false)

/** Whether the caller renders inside a Panel. */
export const useInPanel = (): boolean => use(InPanel)

/**
 * A ShellCard (DESIGN.md 10): the card rim at --radius-panel with --panel-inset padding (index.css),
 * a quiet h-11 header row (muted icon, step-2 title at 400, an optional hint or control at the end),
 * then one inset well. A panel inside a well is a ruled section of it, never a card in a card.
 */
export function Panel({ icon: Icon, label, action, children, className, id }: {
  icon?: LucideIcon
  /** The header's title. A panel without one is the bare rim around its well (a table that names itself). */
  label?: ReactNode
  action?: ReactNode
  children: ReactNode
  className?: string
  id?: string
}) {
  return (
    <section id={id} data-slot="panel" className={cn("min-w-0 overflow-hidden rounded-(--radius-panel) border border-(--panel-border) bg-card shadow-(--panel-shadow)",
      // A panel inside another panel is a section of it, not a card in a card: one hairline above, no frame.
      "[[data-slot=well]_&]:rounded-none [[data-slot=well]_&]:border-x-0 [[data-slot=well]_&]:border-b-0 [[data-slot=well]_&]:bg-transparent [[data-slot=well]_&]:p-0 [[data-slot=well]_&]:shadow-none [[data-slot=well]_&>header]:px-0",
      className)}>
      {label !== undefined && <header className="flex min-h-11 flex-wrap items-center gap-x-2 gap-y-0.5 px-3.5 py-2 text-body">
        {Icon && <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.6} />}
        {/* A title wraps (two lines at most) rather than losing its end; a hint goes to the next line when both don't fit. */}
        <h2 className="line-clamp-2 min-w-0 flex-1 basis-40 text-body font-normal tracking-[-0.01em] text-foreground [overflow-wrap:anywhere]" title={typeof label === "string" ? label : undefined}>{label}</h2>
        {/* A hint (text) gives way on a phone; a control (an element) stays. */}
        {action && <div className={cn("ms-auto shrink-0 text-meta text-muted-foreground", typeof action === "string" && "max-sm:hidden")}>{action}</div>}
      </header>}
      <InPanel value={true}>{children}</InPanel>
    </section>
  )
}

/** The body of a panel: the inset well inside the rim, in every skin (the concentric double shell). */
export function Well({ children, className }: { children: ReactNode; className?: string }) {
  return <div data-slot="well" className={cn("overflow-hidden border-t border-(--well-border) bg-(--well)", className)}>{children}</div>
}

/**
 * An empty well: an icon, a short title ("No data yet" unless given), a line saying why, and the
 * next step when there is one (load data, connect a source, add the first item).
 */
export function EmptyState({ icon: Icon, title = "No data yet", children, next }: { icon: LucideIcon; title?: ReactNode; children: ReactNode; next?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2.5 px-6 py-12 text-center">
      <span aria-hidden className="flex size-[40px] items-center justify-center rounded-(--radius-well) bg-card text-muted-foreground ring-1 ring-line">
        <Icon className="size-[18px]" strokeWidth={1.6} />
      </span>
      <div className="text-body text-foreground">{title}</div>
      <p className="max-w-xs text-meta leading-relaxed text-muted-foreground">{children}</p>
      {next && <div className="flex flex-wrap justify-center gap-2 pt-1.5">{next}</div>}
    </div>
  )
}
