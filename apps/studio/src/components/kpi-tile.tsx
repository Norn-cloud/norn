import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"

/**
 * One figure (design v3, after Snowsight's stat row): a small muted label, a tabular number, a muted
 * line under it. Tiles sit in a StatStrip, which draws the one frame and the rules between them.
 */
export function KpiTile({ icon: Icon, label, value, sub }: { icon?: LucideIcon; label: ReactNode; value: ReactNode; sub?: ReactNode }) {
  return (
    <section data-slot="kpi" className="flex h-full min-w-0 flex-col px-4 py-3">
      <h2 className="flex items-center gap-1.5 text-meta text-muted-foreground">{Icon && <Icon aria-hidden className="size-3.5 shrink-0" />}<span className="min-w-0 truncate">{label}</span></h2>
      <div className="pt-1.5 text-figure leading-none font-medium tracking-tight tabular-nums">{value}</div>
      {sub !== undefined && <div className="truncate pt-1.5 text-meta text-muted-foreground tabular-nums">{sub}</div>}
    </section>
  )
}

/** Figures in one row under one hairline frame, ruled between; they stack on narrow screens. */
export function StatStrip({ children }: { children: ReactNode }) {
  return <div data-slot="stat-strip" className="grid divide-y overflow-hidden rounded-lg border bg-card sm:auto-cols-fr sm:grid-flow-col sm:divide-x sm:divide-y-0">{children}</div>
}
