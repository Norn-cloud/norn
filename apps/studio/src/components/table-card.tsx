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
        "[[data-slot=panel]>&]:mx-1.5 [[data-slot=panel]>&]:mb-1.5 [[data-slot=panel]>&]:rounded-lg [[data-slot=panel]>&]:border-0",
        "[&_th]:h-8 [&_th]:border-b [&_th]:text-xs [&_th]:font-medium [&_th]:text-muted-foreground [&_td]:h-[2.375rem] [&_td]:py-1.5",
        className,
      )}
    >
      {children}
    </div>
  )
}
