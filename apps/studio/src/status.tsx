import { Gauge } from "lucide-react"
import { Panel, Well } from "@/components/panel"
import STATUS from "./data/status.json"

/** One area's estimated progress: a bar with its percentage and a one-line detail. */
function Area({ area, percent, detail }: { readonly area: string; readonly percent: number; readonly detail: string }) {
  return (
    <div className="space-y-1.5 px-4 py-3">
      <div className="flex items-baseline justify-between gap-3">
        <span className="font-medium">{area}</span>
        <span className="text-meta text-muted-foreground tabular-nums">{percent}%</span>
      </div>
      <div role="progressbar" aria-label={area} aria-valuenow={percent} aria-valuemin={0} aria-valuemax={100} className="h-1.5 overflow-hidden rounded-full bg-muted">
        <div className="h-full rounded-full bg-primary" style={{ width: `${percent}%` }} />
      </div>
      <p className="text-meta text-muted-foreground">{detail}</p>
    </div>
  )
}

export function StatusPage() {
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Status</h1>
        <p className="max-w-2xl text-muted-foreground">Where the alpha stands, area by area.</p>
      </div>
      <Panel icon={Gauge} label="Verdict">
        <Well className="px-4 py-4 text-panel leading-relaxed">{STATUS.verdict}</Well>
      </Panel>
      <Panel icon={Gauge} label="By area">
        <Well className="divide-y">
          {STATUS.areas.map((area) => <Area key={area.area} {...area} />)}
        </Well>
      </Panel>
      <p className="text-meta text-muted-foreground">{STATUS.footnote}</p>
    </section>
  )
}
