import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * The frame every table sits in (design v4): one hairline at the panel radius (11px) on the subtle
 * card surface, a quietly tinted header band, hairline row dividers (never zebra stripes), smooth
 * row hover and tabular figures. Row height follows the shell's density.
 */
export function TableCard({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      data-slot="table-card"
      className={cn(
        "overflow-hidden rounded-xl border border-border/90 bg-card text-meta tabular-nums",
        // Directly in a panel the table is the panel's body: flush, under one hairline, no second frame.
        "[[data-slot=panel]>&]:rounded-none [[data-slot=panel]>&]:border-x-0 [[data-slot=panel]>&]:border-b-0",
        "[&_thead_tr]:bg-surface-subtle/85 [&_th]:h-9 [&_th]:border-b [&_th]:border-border/85 [&_th]:text-xs [&_th]:font-medium [&_th]:text-muted-foreground [&_td]:h-cell [&_td]:py-1.5",
        className,
      )}
    >
      {children}
    </div>
  )
}
