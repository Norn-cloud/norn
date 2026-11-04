import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { Panel, Well } from "@/components/panel"

/** One figure: a panel whose well holds a large tabular number and a muted line under it. */
export function KpiTile({ icon, label, value, sub }: { icon?: LucideIcon; label: ReactNode; value: ReactNode; sub?: ReactNode }) {
  return (
    <Panel icon={icon} label={label} className="flex h-full flex-col">
      <Well className="flex-1 px-4 py-4">
        <div className="text-figure leading-none font-normal tracking-[-0.02em] tabular-nums">{value}</div>
        {sub !== undefined && <div className="pt-2 text-meta text-muted-foreground tabular-nums">{sub}</div>}
      </Well>
    </Panel>
  )
}

/** Three figures in a row, stacking on narrow screens. */
export function StatStrip({ children }: { children: ReactNode }) {
  return <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">{children}</div>
}
