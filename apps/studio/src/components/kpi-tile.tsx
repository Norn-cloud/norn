import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"

/**
 * One key figure (v6 StatWell craft): quiet normal-weight label in the outer rim, commanding
 * tabular number and muted caption inside the soft inner well.
 */
export function KpiTile({ icon: Icon, label, value, sub }: { icon?: LucideIcon; label: ReactNode; value: ReactNode; sub?: ReactNode }) {
  return (
    <section
      data-slot="kpi"
      className="flex h-full min-w-0 flex-col rounded-(--radius-panel) border border-(--panel-border) bg-card p-1 shadow-(--panel-shadow)"
    >
      <h2 className="flex h-9 items-center gap-1.5 px-3 text-sm font-normal text-foreground">
        {Icon && <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" strokeWidth={1.6} />}
        <span className="min-w-0 truncate">{label}</span>
      </h2>
      <div
        data-slot="kpi-well"
        className="flex flex-1 flex-col justify-between gap-1.5 rounded-(--radius-well) border border-(--well-border) bg-(--well) px-3.5 pt-3 pb-2.5"
      >
        <div className="text-figure leading-none font-normal tracking-[-0.025em] text-foreground tabular-nums">{value}</div>
        {sub !== undefined && <div className="truncate text-meta text-muted-foreground tabular-nums">{sub}</div>}
      </div>
    </section>
  )
}

/** Row of v6 StatWell cards; adapts cleanly across desktop and narrow viewports. */
export function StatStrip({ children }: { children: ReactNode }) {
  return (
    <div
      data-slot="stat-strip"
      className="grid gap-3 sm:auto-cols-fr sm:grid-flow-col"
    >
      {children}
    </div>
  )
}
