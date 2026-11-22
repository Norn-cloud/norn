import { Link } from "@tanstack/react-router"
import { ArrowRight, CalendarDays } from "lucide-react"
import { Chip } from "@/components/chip"
import type { MenuEntry } from "../shell.tsx"
import { egp } from "./records.ts"
import { FinanceHeading, Mark } from "./workspace.tsx"

const headline = "Weekly repayment reconciliation rate (by amount)"
export const financeMetricMenu = (): readonly MenuEntry[] => [{href:"/metrics",label:headline,group:"Reconciliation"}]

export function FinanceMetrics() {
  return <section className="space-y-4">
    <FinanceHeading title={headline} description="Share of scheduled repayment value matched to settlement evidence, measured in EGP." />
    <div className="grid auto-rows-fr gap-3 sm:grid-cols-2">
      <article className="space-y-2 rounded-2xl border border-data-accent/30 bg-data-accent/5 p-5">
        <Chip tone="blue" dot className="rounded-full">Published</Chip><p className="text-3xl font-semibold tabular-nums">98.00%</p>
        <p className="text-sm text-muted-foreground">{egp(29_400_000)} / {egp(30_000_000)}</p>
      </article>
      <article className="space-y-2 rounded-2xl border border-tone-amber/30 bg-tone-amber/5 p-5">
        <Chip tone="amber" dot className="rounded-full">Corrected</Chip><p className="text-3xl font-semibold tabular-nums">97.50%</p>
        <p className="text-sm text-muted-foreground">{egp(29_250_000)} / {egp(30_000_000)}</p>
      </article>
    </div>
    <article className="space-y-2 rounded-2xl border bg-card p-4">
      <h2 className="flex items-center gap-2 text-sm font-semibold"><CalendarDays className="size-4 text-tone-blue" />Week of 16 Nov 2026</h2>
      <p className="text-xs text-muted-foreground">A late {egp(150_000)} Fawry settlement moved into the following week.</p>
      <div className="flex items-center gap-2 text-xs font-medium"><Mark id="fawry" className="size-6" /> <code>FWR-SET-1027</code></div>
    </article>
    <Link to="/examples/$scene" params={{scene:"explorer"}} className="inline-flex items-center gap-1.5 text-sm font-medium text-tone-blue hover:underline">Explore sample records <ArrowRight className="size-4" /></Link>
  </section>
}
