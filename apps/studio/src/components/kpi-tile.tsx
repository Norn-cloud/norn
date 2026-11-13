import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"

/** One figure (design system v1, after Midday and Polar): a small muted label, a large tabular number, a muted line under it. */
export function KpiTile({ icon: Icon, label, value, sub }: { icon?: LucideIcon; label: ReactNode; value: ReactNode; sub?: ReactNode }) {
  return (
    <section data-slot="kpi" className="flex h-full min-w-0 flex-col rounded-2xl bg-card px-5 py-4 shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
      <h2 className="flex items-center gap-1.5 text-meta font-medium text-muted-foreground">{Icon && <Icon aria-hidden className="size-3.5 shrink-0" />}<span className="min-w-0 truncate">{label}</span></h2>
      <div className="pt-2.5 text-[2rem] leading-none font-light tracking-[-0.04em] tabular-nums">{value}</div>
      {sub !== undefined && <div className="pt-2 text-meta text-muted-foreground tabular-nums">{sub}</div>}
    </section>
  )
}

/** Three figures in a row, stacking on narrow screens. */
export function StatStrip({ children }: { children: ReactNode }) {
  return <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{children}</div>
}
