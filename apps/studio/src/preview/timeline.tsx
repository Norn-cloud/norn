import { ArrowRight, CalendarClock, History, User } from "lucide-react"
import { Chip } from "@/components/chip"
import { cn } from "@/lib/utils"
import { BORROWER, CHANGES, THEN_NOW } from "./lending.ts"
import { Person, Slide, SourceChip } from "./slide.tsx"

/** Where each change sits on the scrubber, from 1 Sep (0) to 20 Nov (1). */
const MARKS = [
  { at: 14 / 80, tone: "bg-tone-green" }, { at: 44 / 80, tone: "bg-tone-red" }, { at: 63 / 80, tone: "bg-tone-violet" },
  { at: 75 / 80, tone: "bg-tone-red" }, { at: 78 / 80, tone: "bg-tone-amber" }, { at: 1, tone: "bg-tone-red" },
]
/** Date labels at their day: 1 Sep is day 0 of 80. */
const TICKS = [["1 Sep", 0], ["15 Sep", 14], ["1 Oct", 30], ["15 Oct", 44], ["1 Nov", 61], ["15 Nov", 75], ["20 Nov", 80]] as const

/** Slide 4: the borrower scrubbed back to 1 Sep, beside today, with every change between. */
export function TimelineSlide() {
  return (
    <Slide eyebrow="The Weave · history" title="Go back to any day." preview="The scrubber layout is a preview over Studio's history and as-of reads."
      lead="Scrub one borrower, or the whole book, back in time: what it looked like then, what changed since, who changed it and why.">
      <div className="grid gap-4">
        {/* The scrubber */}
        <section className="rounded-[22px] bg-background px-7 pt-5 pb-6 shadow-[var(--elevation-node)] ring-1 ring-foreground/5">
          <div className="flex items-center gap-3">
            <div className="flex rounded-xl bg-muted p-1 text-sm font-medium">
              <span className="rounded-lg bg-background px-3 py-1.5 shadow-[var(--elevation-1)]">{BORROWER.name} · {BORROWER.loan}</span>
              <span className="px-3 py-1.5 text-muted-foreground">Whole book</span>
            </div>
            <span className="ms-auto flex items-center gap-2 text-sm"><CalendarClock aria-hidden className="size-4 text-muted-foreground" />Showing the record as of <span className="font-semibold">1 Sep 2026</span></span>
          </div>
          <div className="relative mt-7 h-2 rounded-full bg-muted">
            <div className="absolute inset-y-0 left-0 w-full rounded-full bg-[color-mix(in_oklab,var(--action)_18%,transparent)]" />
            {MARKS.map(({ at, tone }) => <span key={at} className={cn("absolute top-1/2 size-3 -translate-x-1/2 -translate-y-1/2 rounded-full ring-2 ring-background", tone)} style={{ left: `${at * 100}%` }} />)}
            <span className="absolute top-1/2 left-0 flex size-6 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full bg-background shadow-[var(--elevation-float)] ring-2 ring-[var(--action)]"><span className="size-2 rounded-full bg-[var(--action)]" /></span>
          </div>
          <div className="relative mt-3 h-5 text-xs text-muted-foreground tabular-nums">{TICKS.map(([tick, day]) => <span key={tick} className={cn("absolute whitespace-nowrap", day === 0 ? "" : day === 80 ? "-translate-x-full" : "-translate-x-1/2")} style={{ left: `${(day / 80) * 100}%` }}>{tick}</span>)}</div>
        </section>
        <div className="grid grid-cols-[1fr_1.05fr] gap-4">
          {/* Then and now */}
          <section className="rounded-[22px] bg-background p-2.5 shadow-[var(--elevation-node)] ring-1 ring-foreground/5">
            <div className="flex items-center gap-3 px-3.5 pt-2 pb-3">
              <Person initials={BORROWER.initials} tone="violet" size="md" />
              <div className="min-w-0 flex-1"><div className="text-[1.125rem] font-semibold tracking-tight">{BORROWER.name}</div><div className="text-meta text-muted-foreground">{BORROWER.loan} · then and now</div></div>
            </div>
            <div className="rounded-[16px] bg-surface-subtle px-4 py-1 ring-1 ring-foreground/[0.04]">
              <div className="grid grid-cols-[9rem_1fr_auto_1fr_auto] items-center gap-x-3 border-b border-border/70 py-2 text-xs font-medium text-muted-foreground">
                <span /> <span>1 Sep 2026</span> <span /> <span>Today, 20 Nov</span> <span>Source</span>
              </div>
              {THEN_NOW.map(({ label, then, now, from }) => (
                <div key={label} className="grid grid-cols-[9rem_1fr_auto_1fr_auto] items-center gap-x-3 border-b border-border/70 py-2.5 last:border-0">
                  <span className="text-meta text-muted-foreground">{label}</span>
                  <span className="text-sm text-muted-foreground tabular-nums">{then}</span>
                  <ArrowRight aria-hidden className="size-3.5 text-muted-foreground" />
                  <span className="text-sm font-semibold tabular-nums">{now}</span>
                  <SourceChip system={from} />
                </div>
              ))}
            </div>
          </section>
          {/* What changed */}
          <section className="rounded-[22px] bg-background px-5 pt-4 pb-3 shadow-[var(--elevation-node)] ring-1 ring-foreground/5">
            <h2 className="flex items-center gap-1.5 pb-3 text-meta font-medium text-muted-foreground"><History aria-hidden className="size-3.5" />What changed since 1 Sep · 6 changes</h2>
            <ol className="relative space-y-2.5 border-s border-border ps-5">
              {CHANGES.map(({ when, who, from, what, why, tone }) => (
                <li key={when} className="relative">
                  <span className={cn("absolute top-1.5 -start-[1.6rem] size-2.5 rounded-full ring-2 ring-background", tone === "red" ? "bg-tone-red" : tone === "green" ? "bg-tone-green" : "bg-tone-gray")} />
                  <div className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate text-sm font-medium">{what}</span>
                    {from ? <SourceChip system={from} /> : <Chip tone="gray" icon={User}>Person</Chip>}
                  </div>
                  <div className="text-xs text-muted-foreground">{when} · {who} · {why}</div>
                </li>
              ))}
            </ol>
          </section>
        </div>
      </div>
    </Slide>
  )
}
