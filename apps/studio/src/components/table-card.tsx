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
        "overflow-hidden rounded-lg border bg-card text-meta tabular-nums",
        // Directly in a panel the table is the panel's body: no second frame.
        "[[data-slot=panel]>&]:rounded-none [[data-slot=panel]>&]:border-0",
        "[&_thead]:bg-surface-subtle [&_th]:h-9 [&_th]:text-xs [&_th]:font-medium [&_th]:text-muted-foreground [&_td]:h-11 [&_td]:py-2",
        className,
      )}
    >
      {children}
    </div>
  )
}
