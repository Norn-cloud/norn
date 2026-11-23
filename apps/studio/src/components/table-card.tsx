import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * The frame every table sits in: one hairline at the frame radius, a quiet
 * header row (small muted labels, no fill), hairline row dividers (never zebra
 * stripes) and 13px tabular figures. Row height follows the shell's density.
 */
export function TableCard({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      data-slot="table-card"
      className={cn(
        "overflow-hidden rounded-lg border bg-card text-meta tabular-nums",
        // Directly in a panel the table is the panel's body: flush, under one hairline, no second frame.
        "[[data-slot=panel]>&]:rounded-none [[data-slot=panel]>&]:border-x-0 [[data-slot=panel]>&]:border-b-0",
        "[&_th]:h-8 [&_th]:border-b [&_th]:text-xs [&_th]:font-medium [&_th]:text-muted-foreground [&_td]:h-cell [&_td]:py-1",
        className,
      )}
    >
      {children}
    </div>
  )
}
