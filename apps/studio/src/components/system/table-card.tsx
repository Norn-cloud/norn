import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { cn } from "@/lib/utils"
import { Panel, useInPanel } from "./panel"

/** The table's own craft: hairline row dividers (never zebra), a quiet step-1 header at 400, tabular figures. */
const TABLE = cn(
  "overflow-hidden text-meta tabular-nums",
  "[&>[data-slot=table-container]]:overflow-x-auto",
  "[&_thead_tr]:bg-surface-subtle/80 [&_th]:h-9 [&_th]:border-b [&_th]:border-(--table-border) [&_th]:text-meta [&_th]:font-normal [&_th]:text-muted-foreground [&_td]:h-cell [&_td]:py-1.5",
)

/**
 * A table in the one nesting pattern (DESIGN.md 10, DataTable): it is always the inset well of a
 * Panel. Inside a Panel it is that panel's well; anywhere else it brings its own Panel (with a header
 * when given a `label`), so a table never draws a rim of its own.
 */
export function TableCard({ children, className, label, icon, action }: {
  children: ReactNode
  className?: string
  /** The Panel header's title, when the table brings its own Panel. */
  label?: ReactNode
  icon?: LucideIcon
  action?: ReactNode
}) {
  const inPanel = useInPanel()
  const well = (
    <div data-slot="table-card" className={cn(TABLE, "rounded-(--radius-well) border border-(--well-border) bg-(--well)",
      // Already inside a well, it is a ruled part of it, not a frame in the well.
      "[[data-slot=well]_&]:rounded-none [[data-slot=well]_&]:border-x-0 [[data-slot=well]_&]:border-b-0 [[data-slot=well]_&]:bg-transparent", className)}>
      {children}
    </div>
  )
  return inPanel ? well : <Panel label={label} icon={icon} action={action}>{well}</Panel>
}
