import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/** A grey figure tile: a muted label, one large regular-weight figure, and an optional muted line under it. */
export function KpiTile({ label, value, sub, className }: { label: ReactNode; value: ReactNode; sub?: ReactNode; className?: string }) {
  return (
    <div data-slot="kpi-tile" className={cn("min-w-0 rounded-lg bg-muted px-5 py-4", className)}>
      <div className="truncate text-meta text-muted-foreground">{label}</div>
      <div className="pt-2 text-figure leading-none font-normal tracking-[-0.02em] tabular-nums">{value}</div>
      {sub !== undefined && <div className="pt-2 text-meta text-muted-foreground tabular-nums">{sub}</div>}
    </div>
  )
}

/** Three or four tiles in a row, stacking on narrow screens. */
export function StatStrip({ children }: { children: ReactNode }) {
  return <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{children}</div>
}
