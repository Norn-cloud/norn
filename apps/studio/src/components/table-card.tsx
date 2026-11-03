import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * The frame every table sits in: a hairline-bordered card at the dense radius,
 * a grey header row, hairline row dividers (never zebra stripes) and 13px
 * tabular figures.
 */
export function TableCard({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      data-slot="table-card"
      className={cn(
        "overflow-hidden rounded-sm border bg-card text-meta tabular-nums",
        "[&_thead]:bg-surface-subtle [&_th]:h-10 [&_th]:text-meta [&_th]:font-medium [&_th]:text-muted-foreground [&_td]:h-11 [&_td]:py-0",
        className,
      )}
    >
      {children}
    </div>
  )
}
