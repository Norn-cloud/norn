import { ChevronRight } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * A disclosure: a native `<details>` whose summary is a quiet row led by a chevron that turns when
 * open (and points the reading way in RTL), never the browser's own triangle. It opens from the
 * keyboard, keeps its content in the page for find-in-page, and works before scripts load.
 */
export function Disclosure({ summary, children, className, summaryClassName, onToggle, ...props }: Omit<ComponentProps<"details">, "onToggle" | "children"> & {
  readonly summary: ReactNode
  readonly children: ReactNode
  readonly summaryClassName?: string
  readonly onToggle?: (open: boolean) => void
}) {
  return (
    <details data-slot="disclosure" className={cn("group/disclosure", className)} onToggle={onToggle && ((event) => onToggle(event.currentTarget.open))} {...props}>
      <summary className={cn("flex w-fit max-w-full cursor-pointer list-none items-center gap-1.5 rounded-(--radius-row) outline-none select-none hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring focus-visible:outline-solid [&::-webkit-details-marker]:hidden", summaryClassName)}>
        <ChevronRight aria-hidden strokeWidth={1.6} className="size-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-open/disclosure:rotate-90 motion-reduce:transition-none rtl:rotate-180 rtl:group-open/disclosure:rotate-90" />
        {summary}
      </summary>
      {children}
    </details>
  )
}
