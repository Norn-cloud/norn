import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * The frame every table sits in (v6 craft): outer card rim at the panel radius with a soft inner
 * well, hairline row dividers (never zebra stripes), smooth hover wash, and tabular figures.
 * Directly inside a Panel it becomes the panel's inner well with zero double-box nesting.
 */
export function TableCard({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      data-slot="table-card"
      className={cn(
        "overflow-hidden rounded-(--radius-panel) border border-(--table-border) bg-card p-1 text-meta tabular-nums shadow-(--panel-shadow)",
        // Directly in a panel the table is the panel's inner well: flush, zero outer padding or shadow.
        "[[data-slot=panel]>&]:rounded-(--radius-well) [[data-slot=panel]>&]:border-(--well-border) [[data-slot=panel]>&]:bg-(--well) [[data-slot=panel]>&]:p-0 [[data-slot=panel]>&]:shadow-none",
        "[&>[data-slot=table-container]]:overflow-x-auto [&>[data-slot=table-container]]:rounded-(--radius-well) [&>[data-slot=table-container]]:border [&>[data-slot=table-container]]:border-(--well-border) [&>[data-slot=table-container]]:bg-(--well)",
        "[[data-slot=panel]>&>[data-slot=table-container]]:rounded-none [[data-slot=panel]>&>[data-slot=table-container]]:border-0 [[data-slot=panel]>&>[data-slot=table-container]]:bg-transparent",
        "[&_thead_tr]:bg-surface-subtle/80 [&_th]:h-9 [&_th]:border-b [&_th]:border-(--table-border) [&_th]:text-xs [&_th]:font-normal [&_th]:text-muted-foreground [&_td]:h-cell [&_td]:py-1.5",
        className,
      )}
    >
      {children}
    </div>
  )
}
