import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"

/**
 * One key figure, a StatWell (DESIGN.md 10): a quiet step-2 label in the rim, then in the inset well
 * the figure (step 5, 400) with its unit at step 2 in muted ink, and a muted step-1 caption. Numbers
 * come from the server only; a value the cell did not return is never shown as 0.
 */
export function KpiTile({ icon: Icon, label, value, unit, sub }: { icon?: LucideIcon; label: ReactNode; value: ReactNode; unit?: ReactNode; sub?: ReactNode }) {
  return (
    <section
      data-slot="kpi"
      className="flex h-full min-w-0 flex-col rounded-(--radius-panel) border border-(--panel-border) bg-card p-(--panel-inset) shadow-(--panel-shadow)"
    >
      <h2 className="flex h-9 items-center gap-1.5 px-3 text-body font-normal text-foreground">
        {Icon && <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" strokeWidth={1.6} />}
        <span className="min-w-0 truncate">{label}</span>
      </h2>
      <div
        data-slot="kpi-well"
        className="flex flex-1 flex-col justify-between gap-1.5 rounded-(--radius-well) border border-(--well-border) bg-(--well) px-4 pt-3.5 pb-3"
      >
        <div className="text-figure font-normal text-foreground tabular-nums">{value}{unit !== undefined && <span className="ms-1 text-body tracking-normal text-muted-foreground">{unit}</span>}</div>
        {sub !== undefined && <div className="truncate text-meta text-muted-foreground tabular-nums">{sub}</div>}
      </div>
    </section>
  )
}

/** The band of 2–4 StatWells at the top of a page: one column on a phone, equal columns from 640px. */
export function StatStrip({ children }: { children: ReactNode }) {
  return (
    <div
      data-slot="stat-strip"
      className="grid gap-4 sm:auto-cols-fr sm:grid-flow-col"
    >
      {children}
    </div>
  )
}
