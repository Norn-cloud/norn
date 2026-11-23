import { Link } from "@tanstack/react-router"
import { ArrowRight, CalendarDays, Clock3, Waypoints } from "lucide-react"
import { useState } from "react"
import { Area, AreaChart, CartesianGrid, ReferenceDot, ResponsiveContainer, XAxis, YAxis } from "recharts"
import { Chip } from "@/components/chip"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { egp } from "./records.ts"
import { FinanceHeading, Mark } from "./workspace.tsx"

const headline = "Weekly repayment reconciliation rate (by amount)"
const settledBatch = { id: "FWR-SET-1027", amount: 150_000, newWeek: "16 Nov 2026", sourcePath: "$.settlement.batch_amount_egp" } as const
const released = { label: "9 Nov", date: "9 Nov 2026", scheduled: 30_000_000, reconciled: 29_400_000, known: "16 Nov 2026 · 09:00 EET" } as const
const corrected = { ...released, reconciled: released.reconciled - settledBatch.amount, known: "17 Nov 2026 · 09:42 EET" }
const weeks = [
  { label: "5 Oct", date: "5 Oct 2026", scheduled: 27_500_000, reconciled: 26_565_000 },
  { label: "12 Oct", date: "12 Oct 2026", scheduled: 28_200_000, reconciled: 27_325_800 },
  { label: "19 Oct", date: "19 Oct 2026", scheduled: 29_100_000, reconciled: 28_401_600 },
  { label: "26 Oct", date: "26 Oct 2026", scheduled: 28_500_000, reconciled: 27_787_500 },
  { label: "2 Nov", date: "2 Nov 2026", scheduled: 29_800_000, reconciled: 29_025_200 },
  released,
] as const

type Week = { readonly scheduled: number; readonly reconciled: number }
const rate = ({ scheduled, reconciled }: Week) => (reconciled / scheduled) * 100
const percent = (week: Week) => `${rate(week).toFixed(2)}%`

export const financeMetricMenu = (): readonly MenuEntry[] => [
  { href: "/metrics", label: headline, group: "Reconciliation" },
  { href: "#corrections", label: "Corrections", group: "Reconciliation" },
]

function Reading({ label, week, description, tone }: {
  readonly label: string; readonly week: Week; readonly description: string; readonly tone: "blue" | "amber" | "gray"
}) {
  return (
    <div className="space-y-1.5 p-4">
      <Chip tone={tone} dot={tone !== "gray"}>{label}</Chip>
      <p className={cn("text-2xl font-semibold tabular-nums", tone === "blue" && "text-tone-blue", tone === "amber" && "text-tone-amber")}>{percent(week)}</p>
      <p className="text-xs tabular-nums text-foreground/85">{egp(week.reconciled)} / {egp(week.scheduled)}</p>
      <p className="text-[11px] text-muted-foreground">{description}</p>
    </div>
  )
}

export function FinanceMetrics() {
  const [selected, setSelected] = useState<(typeof weeks)[number]>(released)
  const [showEvidence, setShowEvidence] = useState(false)
  const points = weeks.map((week) => ({ week: week.label, value: rate(week) }))
  return (
    <section className="space-y-4">
      <FinanceHeading title={headline} description="Share of scheduled repayment value matched to settlement evidence, measured in EGP." />

      <section aria-label="Weekly reconciliation trend" className="overflow-hidden rounded-lg border border-border bg-background">
        <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
          <div>
            <h2 className="text-xs font-semibold text-foreground">Published weekly rate</h2>
            <p className="text-[11px] text-muted-foreground">Week of {selected.date} · {egp(selected.reconciled)} of {egp(selected.scheduled)}</p>
          </div>
          <p className="text-2xl font-semibold tabular-nums text-tone-blue">{percent(selected)}</p>
        </header>
        <div className="p-4">
          <div className="h-48">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={points} margin={{ top: 12, right: 18, left: 0, bottom: 3 }}>
                <defs>
                  <linearGradient id="finance-metric-area" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor="var(--data-accent)" stopOpacity={0.2} />
                    <stop offset="100%" stopColor="var(--data-accent)" stopOpacity={0.01} />
                  </linearGradient>
                </defs>
                <CartesianGrid vertical={false} stroke="var(--border)" />
                <XAxis dataKey="week" tickLine={false} axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 10 }} />
                <YAxis width={35} domain={[95, 99]} ticks={[95, 96, 97, 98, 99]} tickFormatter={(v) => `${v}%`} tickLine={false} axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 10 }} />
                <Area type="monotone" dataKey="value" stroke="var(--data-accent)" strokeWidth={2} fill="url(#finance-metric-area)" isAnimationActive={false} />
                <ReferenceDot x={selected.label} y={rate(selected)} r={5} fill="var(--data-accent)" stroke="var(--background)" strokeWidth={2} />
              </AreaChart>
            </ResponsiveContainer>
          </div>
        </div>
        <div aria-label="Select week" className="flex flex-wrap gap-1 border-t border-border bg-surface-subtle px-4 py-2">
          {weeks.map((week) => (
            <button
              key={week.label}
              type="button"
              aria-pressed={week === selected}
              onClick={() => setSelected(week)}
              className={cn(
                "rounded-sm border px-2.5 py-1 text-xs font-medium transition-colors",
                week === selected ? "border-data-accent bg-background text-tone-blue" : "border-transparent text-muted-foreground hover:text-foreground",
              )}
            >
              {week.label}
            </button>
          ))}
        </div>
      </section>

      <section id="corrections" aria-labelledby="corrections-title" className="scroll-mt-4 overflow-hidden rounded-lg border border-border bg-background">
        <header className="border-b border-border bg-surface-subtle px-4 py-2.5">
          <h2 id="corrections-title" className="flex items-center gap-2 text-xs font-semibold text-foreground">
            <CalendarDays className="size-3.5 text-tone-blue" />Settlement correction · Week of {released.date}
          </h2>
          <p className="mt-0.5 text-[11px] text-muted-foreground">The {egp(settledBatch.amount)} Fawry settlement belongs to the week of {settledBatch.newWeek}.</p>
        </header>
        <div className="grid divide-y divide-border md:grid-cols-3 md:divide-x md:divide-y-0">
          <Reading label="Published" week={released} description={released.known} tone="blue" />
          <Reading label="Corrected" week={corrected} description={corrected.known} tone="amber" />
          <Reading label="As known on 16 Nov" week={released} description="Replayed at the original knowledge cutoff." tone="gray" />
        </div>
        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border bg-surface-subtle/60 px-4 py-2.5">
          <div className="flex items-center gap-2">
            <Mark id="fawry" className="size-5" />
            <code className="text-xs font-medium">{settledBatch.id}</code>
            <span className="text-[11px] text-muted-foreground">Fawry settlement batch</span>
          </div>
          <button
            type="button"
            aria-expanded={showEvidence}
            onClick={() => setShowEvidence(!showEvidence)}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-xs font-medium text-tone-blue hover:border-data-accent"
          >
            <Waypoints className="size-3.5" />{showEvidence ? "Hide settlement evidence" : "Open settlement evidence"}
          </button>
        </div>
        {showEvidence && (
          <div aria-label="Fawry settlement evidence" className="space-y-2 border-t border-border bg-background px-4 py-3 text-xs">
            <p className="flex flex-wrap items-center gap-1.5">
              <code className="rounded-xs bg-muted px-1.5 py-0.5">{settledBatch.sourcePath}</code>
              <ArrowRight className="size-3.5 text-tone-blue" />
              <span>Fawry cash collection</span>
              <ArrowRight className="size-3.5 text-tone-blue" />
              <code className="rounded-xs bg-muted px-1.5 py-0.5">Payment.amount</code>
            </p>
            <p className="flex items-center gap-1.5 text-[11px] text-muted-foreground"><Clock3 className="size-3.5" />Reclassified {corrected.known}</p>
            <Link to="/sources/$concept/$" params={{ concept: "payment", _splat: settledBatch.id }} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
              Open payment record <ArrowRight className="size-3.5" />
            </Link>
          </div>
        )}
      </section>
    </section>
  )
}
