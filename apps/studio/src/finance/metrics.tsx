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
const rate = ({scheduled,reconciled}: Week) => reconciled / scheduled * 100
const percent = (week: Week) => `${rate(week).toFixed(2)}%`

export const financeMetricMenu = (): readonly MenuEntry[] => [
  {href:"/metrics", label:headline, group:"Reconciliation"},
  {href:"#corrections", label:"Corrections", group:"Reconciliation"},
]

function Reading({ label, week, description, tone }: {
  readonly label: string; readonly week: Week; readonly description: string; readonly tone: "blue" | "amber" | "gray"
}) {
  return <div className={cn("space-y-2 rounded-2xl border bg-card p-4",tone==="blue"&&"border-data-accent/30 bg-data-accent/5",tone==="amber"&&"border-tone-amber/30 bg-tone-amber/5")}>
    <Chip tone={tone} dot={tone!=="gray"} className="rounded-full">{label}</Chip>
    <p className={cn("text-3xl font-semibold tabular-nums",tone==="blue"&&"text-tone-blue",tone==="amber"&&"text-tone-amber")}>{percent(week)}</p>
    <p className="text-xs tabular-nums text-muted-foreground">{egp(week.reconciled)} / {egp(week.scheduled)}</p>
    <p className="text-xs text-muted-foreground">{description}</p>
  </div>
}

export function FinanceMetrics() {
  const [selected,setSelected]=useState<(typeof weeks)[number]>(released)
  const [showEvidence,setShowEvidence]=useState(false)
  const points=weeks.map(week=>({week:week.label,value:rate(week)}))
  return <section className="space-y-4">
    <FinanceHeading title={headline} description="Share of scheduled repayment value matched to settlement evidence, measured in EGP." />
    <section aria-label="Weekly reconciliation trend" className="rounded-2xl border bg-card p-4">
      <header className="mb-4 flex flex-wrap items-end justify-between gap-2">
        <div><h2 className="text-sm font-semibold">Published weekly rate</h2><p className="mt-0.5 text-xs text-muted-foreground">Week of {selected.date} · {egp(selected.reconciled)} of {egp(selected.scheduled)}</p></div>
        <p className="text-3xl font-semibold tabular-nums text-tone-blue">{percent(selected)}</p>
      </header>
      <div className="h-48">
        <ResponsiveContainer width="100%" height="100%">
          <AreaChart data={points} margin={{top:12,right:18,left:0,bottom:3}}>
            <defs><linearGradient id="finance-metric-area" x1="0" y1="0" x2="0" y2="1"><stop offset="0%" stopColor="var(--data-accent)" stopOpacity={0.25}/><stop offset="100%" stopColor="var(--data-accent)" stopOpacity={0.02}/></linearGradient></defs>
            <CartesianGrid vertical={false} stroke="var(--border)" />
            <XAxis dataKey="week" tickLine={false} axisLine={false} tick={{fill:"var(--muted-foreground)",fontSize:10}}/>
            <YAxis width={35} domain={[95,99]} ticks={[95,96,97,98,99]} tickFormatter={v=>`${v}%`} tickLine={false} axisLine={false} tick={{fill:"var(--muted-foreground)",fontSize:10}}/>
            <Area type="monotone" dataKey="value" stroke="var(--data-accent)" strokeWidth={2.5} fill="url(#finance-metric-area)" isAnimationActive={false}/>
            <ReferenceDot x={selected.label} y={rate(selected)} r={6} fill="var(--data-accent)" stroke="var(--card)" strokeWidth={2}/>
          </AreaChart>
        </ResponsiveContainer>
      </div>
      <div aria-label="Select week" className="flex flex-wrap gap-1.5 border-t pt-3">
        {weeks.map(week=><button key={week.label} type="button" aria-pressed={week===selected} onClick={()=>setSelected(week)} className={cn("rounded-full border px-2.5 py-1 text-xs text-muted-foreground hover:bg-muted",week===selected&&"border-data-accent bg-data-accent/10 text-tone-blue")}>{week.label}</button>)}
      </div>
    </section>
    <section id="corrections" aria-labelledby="corrections-title" className="scroll-mt-4 space-y-3 rounded-2xl border bg-card p-4">
      <header>
        <h2 id="corrections-title" className="flex items-center gap-2 text-section font-semibold"><CalendarDays className="size-4 text-tone-blue" />Settlement correction</h2>
        <p className="mt-1 text-xs text-muted-foreground">Week of {released.date}. The {egp(settledBatch.amount)} Fawry settlement belongs to the week of {settledBatch.newWeek}.</p>
      </header>
      <div className="grid gap-3 md:grid-cols-3">
        <Reading label="Published" week={released} description={released.known} tone="blue"/>
        <Reading label="Corrected" week={corrected} description={corrected.known} tone="amber"/>
        <Reading label="As known on 16 Nov" week={released} description="Replayed at the original knowledge cutoff." tone="gray"/>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border bg-surface-subtle px-3 py-2">
        <div className="flex items-center gap-2"><Mark id="fawry" className="size-6"/><div><code className="text-xs font-medium">{settledBatch.id}</code><p className="text-[10px] text-muted-foreground">Fawry settlement batch</p></div></div>
        <button type="button" aria-expanded={showEvidence} onClick={()=>setShowEvidence(!showEvidence)} className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-tone-blue hover:bg-data-accent/10"><Waypoints className="size-3.5" />{showEvidence?"Hide settlement evidence":"Open settlement evidence"}</button>
      </div>
      {showEvidence&&<div aria-label="Fawry settlement evidence" className="space-y-2 rounded-lg border bg-card px-3 py-2 text-xs">
        <p className="flex flex-wrap items-center gap-1.5"><code>{settledBatch.sourcePath}</code><ArrowRight className="size-3.5 text-tone-blue"/>Fawry cash collection<ArrowRight className="size-3.5 text-tone-blue"/><code>Payment.amount</code></p>
        <p className="flex items-center gap-1.5 text-muted-foreground"><Clock3 className="size-3.5"/>Reclassified {corrected.known}</p>
        <Link to="/sources/$concept/$" params={{concept:"payment",_splat:settledBatch.id}} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">Open payment record <ArrowRight className="size-3.5"/></Link>
      </div>}
    </section>
    <Link to="/examples/$scene" params={{scene:"explorer"}} className="inline-flex items-center gap-1.5 text-sm font-medium text-tone-blue hover:underline">Explore sample records <ArrowRight className="size-4" /></Link>
  </section>
}
