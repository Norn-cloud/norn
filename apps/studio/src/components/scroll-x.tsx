import { ArrowRight } from "lucide-react"
import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * Content wider than its well, scrolled sideways inside it with a visible cue: a fade on each edge
 * that has more beyond it, and one quiet line saying the content scrolls. The region takes focus only
 * while it overflows, so the keyboard can scroll it too. Directions are logical: in RTL the start is
 * on the right (`scrollLeft` is then zero or negative, so only its size is read).
 */
export function ScrollX({ label, children, className, hint = "Scroll sideways to see every column." }: {
  readonly label: string
  readonly children: ReactNode
  readonly className?: string
  readonly hint?: string
}) {
  const ref = useRef<HTMLDivElement>(null)
  const [edges, setEdges] = useState({ start: false, end: false })
  useLayoutEffect(() => {
    const element = ref.current
    if (element === null) return
    const measure = () => {
      const room = element.scrollWidth - element.clientWidth
      const at = Math.abs(element.scrollLeft)
      setEdges((previous) => {
        const next = { start: room > 1 && at > 1, end: room > 1 && room - at > 1 }
        return previous.start === next.start && previous.end === next.end ? previous : next
      })
    }
    measure()
    element.addEventListener("scroll", measure, { passive: true })
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(measure)
    observer?.observe(element)
    if (element.firstElementChild) observer?.observe(element.firstElementChild)
    return () => { element.removeEventListener("scroll", measure); observer?.disconnect() }
  }, [])
  const overflows = edges.start || edges.end
  return (
    <div data-slot="scroll-x" data-overflow={overflows ? (edges.end ? "end" : "start") : undefined} className={cn("min-w-0", className)}>
      <div className="relative">
        <div ref={ref} role="region" aria-label={label} tabIndex={overflows ? 0 : undefined}
          className="overflow-x-auto overscroll-x-contain outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring focus-visible:outline-solid">
          {children}
        </div>
        {edges.start && <span aria-hidden className="pointer-events-none absolute inset-y-0 start-0 w-8 bg-linear-to-r from-(--well) to-transparent rtl:bg-linear-to-l" />}
        {edges.end && <span aria-hidden className="pointer-events-none absolute inset-y-0 end-0 w-10 bg-linear-to-l from-(--well) to-transparent rtl:bg-linear-to-r" />}
      </div>
      {overflows && (
        <p data-scroll-hint="" className="flex items-center gap-1.5 px-2.5 pt-2 text-meta text-muted-foreground">
          <ArrowRight aria-hidden strokeWidth={1.6} className="size-3.5 shrink-0 rtl:rotate-180" />{hint}
        </p>
      )}
    </div>
  )
}
