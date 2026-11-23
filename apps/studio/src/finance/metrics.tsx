import { Link } from "@tanstack/react-router"
import { ArrowRight, CalendarDays, ChartLine, Clock3, Waypoints } from "lucide-react"
import { useEffect, useState } from "react"
import { Area, AreaChart, CartesianGrid, ReferenceDot, ResponsiveContainer, XAxis, YAxis } from "recharts"
import { Chip } from "@/components/chip"
import { PageHeader, Panel, Well } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { useInspector } from "@/lib/inspector"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { egp } from "./records.ts"
import { Mark } from "./workspace.tsx"

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
  readonly label: string; readonly week: Week; readonly description: string; readonly tone: "gray" | "amber"
}) {
  return (
    <div className="space-y-1.5 px-4 py-3.5">
      <Chip tone={tone} dot={tone === "amber"}>{label}</Chip>
      <p className={cn("pt-1 text-figure leading-none font-medium tracking-tight tabular-nums", tone === "amber" ? "text-tone-amber" : "text-foreground")}>{percent(week)}</p>
      <p className="text-meta tabular-nums text-foreground/85">{egp(week.reconciled)} / {egp(week.scheduled)}</p>
      <p className="text-xs text-muted-foreground">{description}</p>
    </div>
  )
}

function SettlementEvidenceBody() {
  return (
    <div aria-label="Fawry settlement evidence" className="divide-y text-meta">
      <div className="flex items-center gap-2 pb-3">
        <Mark id="fawry" className="size-5" />
        <div>
          <code className="block font-mono text-xs font-medium text-foreground">{settledBatch.id}</code>
          <span className="text-xs text-muted-foreground">Fawry settlement batch · {egp(settledBatch.amount)}</span>
        </div>
      </div>
      <div className="space-y-1.5 py-3">
        <p className="text-xs text-muted-foreground">Lineage path</p>
        <p className="flex flex-wrap items-center gap-1.5 text-xs">
          <code className="rounded-sm bg-muted px-1.5 py-0.5">{settledBatch.sourcePath}</code>
          <ArrowRight className="size-3 text-muted-foreground" />
          <span>Fawry cash collection</span>
          <ArrowRight className="size-3 text-muted-foreground" />
          <code className="rounded-sm bg-muted px-1.5 py-0.5">Payment.amount</code>
        </p>
      </div>
      <div className="py-3">
        <p className="text-xs text-muted-foreground">Impact on week of {released.date}</p>
        <p className="mt-1 tabular-nums text-foreground">98.00% → 97.50% (−{egp(settledBatch.amount)} moved to {settledBatch.newWeek})</p>
      </div>
      <div className="space-y-2.5 pt-3">
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground"><Clock3 className="size-3.5" />Reclassified {corrected.known}</p>
        <Link to="/sources/$concept/$" params={{ concept: "payment", _splat: settledBatch.id }} className="inline-flex items-center gap-1.5 text-xs font-medium text-foreground underline underline-offset-4">
          Open payment record <ArrowRight className="size-3.5 text-muted-foreground" />
        </Link>
      </div>
    </div>
  )
}

export function FinanceMetrics() {
  const [selected, setSelected] = useState<(typeof weeks)[number]>(released)
  const { inspected, inspect } = useInspector()
  const points = weeks.map((week) => ({ week: week.label, value: rate(week) }))

  useEffect(() => () => inspect(), [inspect])

  const toggleEvidence = () => {
    if (inspected) inspect()
    else inspect({ title: `Settlement evidence · ${settledBatch.id}`, body: <SettlementEvidenceBody /> })
  }

  return (
    <section className="space-y-6">
      <PageHeader
        title={headline}
        meta={<Chip tone="amber" dot>1 correction · 97.50%</Chip>}
        description="Share of scheduled repayment value matched to settlement evidence, measured in EGP."
        primary={
          <Button type="button" size="sm" variant="outline" aria-expanded={inspected !== undefined} onClick={toggleEvidence}>
            <Waypoints className="size-3.5" />{inspected ? "Hide settlement evidence" : "Open settlement evidence"}
          </Button>
        }
      />

      <Panel
        icon={ChartLine}
        label="Published weekly rate"
        action={<span className="text-base font-medium tabular-nums text-foreground">{percent(selected)}</span>}
      >
        <Well>
          <div className="px-4 pt-3 text-xs text-muted-foreground">
            Week of {selected.date} · {egp(selected.reconciled)} of {egp(selected.scheduled)}
          </div>
          <div aria-label="Weekly reconciliation trend" className="p-4">
            <div className="h-48">
              <ResponsiveContainer width="100%" height="100%">
                <AreaChart data={points} margin={{ top: 12, right: 18, left: 0, bottom: 3 }}>
                  <defs>
                    <linearGradient id="finance-metric-area" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor="var(--chart-1)" stopOpacity={0.12} />
                      <stop offset="100%" stopColor="var(--chart-1)" stopOpacity={0.01} />
                    </linearGradient>
                  </defs>
                  <CartesianGrid vertical={false} stroke="var(--border)" />
                  <XAxis dataKey="week" tickLine={false} axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                  <YAxis width={38} domain={[95, 99]} ticks={[95, 96, 97, 98, 99]} tickFormatter={(v) => `${v}%`} tickLine={false} axisLine={false} tick={{ fill: "var(--muted-foreground)", fontSize: 11 }} />
                  <Area type="monotone" dataKey="value" stroke="var(--chart-1)" strokeWidth={1.75} fill="url(#finance-metric-area)" isAnimationActive={false} />
                  <ReferenceDot x={selected.label} y={rate(selected)} r={4.5} fill="var(--chart-1)" stroke="var(--card)" strokeWidth={2} />
                </AreaChart>
              </ResponsiveContainer>
            </div>
          </div>
          <div aria-label="Select week" className="flex flex-wrap gap-1 border-t px-4 py-2">
            {weeks.map((week) => (
              <button
                key={week.label}
                type="button"
                aria-pressed={week === selected}
                onClick={() => setSelected(week)}
                className={cn(
                  "rounded-md px-2.5 py-1 text-xs font-medium transition-colors",
                  week === selected ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground",
                )}
              >
                {week.label}
              </button>
            ))}
          </div>
        </Well>
      </Panel>

      <Panel
        id="corrections"
        icon={CalendarDays}
        label={`Settlement correction · Week of ${released.date}`}
        action={`The ${egp(settledBatch.amount)} Fawry settlement belongs to ${settledBatch.newWeek}`}
        className="scroll-mt-4"
      >
        <Well>
          <div className="grid divide-y md:grid-cols-3 md:divide-x md:divide-y-0">
            <Reading label="Published" week={released} description={released.known} tone="gray" />
            <Reading label="Corrected" week={corrected} description={corrected.known} tone="amber" />
            <Reading label="As known on 16 Nov" week={released} description="Replayed at the original knowledge cutoff." tone="gray" />
          </div>
          <div className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-2.5 text-meta">
            <div className="flex items-center gap-2">
              <Mark id="fawry" className="size-5" />
              <code className="font-mono text-xs font-medium">{settledBatch.id}</code>
              <span className="text-xs text-muted-foreground">Fawry settlement batch · {egp(settledBatch.amount)}</span>
            </div>
            <Link to="/sources/$concept/$" params={{ concept: "payment", _splat: settledBatch.id }} className="inline-flex items-center gap-1 text-xs font-medium text-foreground hover:underline underline-offset-4">
              Open payment record <ArrowRight className="size-3.5 text-muted-foreground" />
            </Link>
          </div>
        </Well>
      </Panel>
    </section>
  )
}
