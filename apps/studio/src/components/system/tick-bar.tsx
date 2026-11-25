import { PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover"
import type { ReactNode } from "react"
import { KitPopoverContent, Popover } from "./kit"
import { DOT, StatusDot, type DotTone } from "./status-dot"
import { cn } from "@/lib/utils"

/** How many hairline ticks the bar draws, whatever the total (DESIGN.md 12.4). */
export const TICKS = 10

/**
 * How many of `ticks` to fill for `marked` of `total`: proportional, at least one when anything is
 * marked, and never all of them while some are not, so a small share never vanishes and a large
 * one never reads as everything.
 */
export function filledTicks(marked: number, total: number, ticks: number = TICKS): number {
  if (total <= 0 || marked <= 0) return 0
  if (marked >= total) return ticks
  return Math.min(ticks - 1, Math.max(1, Math.round((ticks * marked) / total)))
}

export interface TickBarItem {
  readonly id: string
  readonly title: ReactNode
  readonly lang?: string
  /** The item's state, dot first; an item without one makes no claim. */
  readonly state?: { readonly tone: DotTone; readonly word: ReactNode }
}

/**
 * The top bar's proportional tick bar (DESIGN.md 9.1 and 12.4): a pill with a quiet label, ten
 * hairline ticks with the marked share filled in one dot tone, and a muted count, opening a popover
 * that lists every item in plain words. It draws only what it is given: the caller passes the
 * server's items and counts, and a share it cannot know is left unfilled, never guessed.
 */
export function TickBar({ label, name, total, marked, tone, count, heading, items, note, footer, className }: {
  readonly label: ReactNode
  /** The trigger's full accessible name, with the count and its meaning in words. */
  readonly name: string
  readonly total: number
  readonly marked: number
  readonly tone: DotTone
  readonly count: ReactNode
  readonly heading: ReactNode
  readonly items: readonly TickBarItem[]
  /** One muted line above the list (a scan bound, a read that failed). */
  readonly note?: ReactNode
  readonly footer?: ReactNode
  readonly className?: string
}) {
  const filled = filledTicks(marked, total)
  return (
    <Popover>
      <PopoverTrigger aria-label={name} data-slot="tick-bar" data-marked={marked} data-total={total}
        className={cn("flex h-[32px] shrink-0 items-center gap-2.5 rounded-full px-3 text-body text-foreground ring-1 ring-input ring-inset outline-none transition-colors duration-150 hover:bg-(--well) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-open:bg-(--well) motion-reduce:transition-none max-sm:h-[44px]", className)}>
        <span>{label}</span>
        <span aria-hidden data-slot="ticks" className="flex h-[14px] items-stretch gap-[2px]">
          {Array.from({ length: TICKS }, (_, index) => (
            <span key={index} data-filled={index < filled || undefined} className={cn("w-[2px] rounded-full", index < filled ? DOT[tone] : "bg-line")} />
          ))}
        </span>
        <span className="text-muted-foreground tabular-nums">{count}</span>
      </PopoverTrigger>
      <KitPopoverContent align="end" className="w-[min(22rem,calc(100vw-24px))] p-2">
        <PopoverTitle className="px-2 pt-1 pb-2 text-body text-foreground">{heading}</PopoverTitle>
        {note !== undefined && <p className="px-2 pb-2 text-meta text-muted-foreground">{note}</p>}
        <ol data-slot="well" className="flex max-h-[50vh] flex-col overflow-y-auto rounded-(--radius-well) bg-(--well) p-1.5 ring-1 ring-(--well-border) ring-inset">
          {items.map((item) => (
            <li key={item.id} data-item={item.id} className="flex min-h-[36px] items-center gap-3 rounded-(--radius-row) px-2.5 py-1.5 text-body">
              <span lang={item.lang} className="min-w-0 flex-1 text-pretty text-foreground">{item.title}</span>
              {item.state && <StatusDot tone={item.state.tone} className="shrink-0">{item.state.word}</StatusDot>}
            </li>
          ))}
        </ol>
        {footer !== undefined && <div className="flex items-center justify-end gap-2 px-2 pt-2 text-meta text-muted-foreground">{footer}</div>}
      </KitPopoverContent>
    </Popover>
  )
}

