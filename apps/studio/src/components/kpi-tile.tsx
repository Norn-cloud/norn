import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"

/**
 * One figure (design v4): a small muted label, a commanding tabular number, and a quiet line under
 * it. Tiles sit in a StatStrip, which draws the panel frame and hairline rules between them.
 */
export function KpiTile({ icon: Icon, label, value, sub }: { icon?: LucideIcon; label: ReactNode; value: ReactNode; sub?: ReactNode }) {
  return (
    <section data-slot="kpi" className="flex h-full min-w-0 flex-col px-5 py-4">
      <h2 className="flex items-center gap-1.5 text-meta font-medium text-muted-foreground">{Icon && <Icon aria-hidden className="size-3.5 shrink-0" />}<span className="min-w-0 truncate">{label}</span></h2>
      <div className="pt-2 text-figure leading-none font-semibold tracking-[-0.035em] text-foreground tabular-nums">{value}</div>
      {sub !== undefined && <div className="truncate pt-2 text-meta text-muted-foreground tabular-nums">{sub}</div>}
    </section>
  )
}

/** Figures in one row under one panel frame with hairline dividers; they stack on narrow screens. */
export function StatStrip({ children }: { children: ReactNode }) {
  return <div data-slot="stat-strip" className="grid divide-y divide-(--panel-border) overflow-hidden rounded-xl border border-(--panel-border) bg-card shadow-(--panel-shadow) sm:auto-cols-fr sm:grid-flow-col sm:divide-x sm:divide-y-0">{children}</div>
}
