import { useQuery } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { ShieldAlert, ShieldCheck } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { EmptyState, Panel, Well } from "@/components/panel"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { EvidenceList } from "./evidence-crop.tsx"
import { readFindings, type Evidence, type Finding, type RuleRef } from "./review.ts"

/** The rule a finding records, by its stable id. */
function ruleName(rule: RuleRef): string {
  if (rule.kind === "law") return rule.lawId
  if (rule.kind === "builtin") return rule.id
  return "an invariant"
}

const STATUS: Record<Finding["status"], { readonly label: string; readonly variant: "destructive" | "secondary" | "outline" }> = {
  open: { label: "Open", variant: "destructive" },
  acknowledged: { label: "Acknowledged", variant: "secondary" },
  resolved: { label: "Resolved", variant: "outline" },
  superseded: { label: "Superseded", variant: "outline" },
}

/**
 * What the cell found wrong with one object when it was imported, which law
 * each finding records, whether the object is held out of authoritative reads,
 * and the evidence of where its values were read.
 */
export function Findings({ objectType, id, actionFor }: { readonly objectType: string; readonly id: string; readonly actionFor?: (evidence: Evidence) => ReactNode }) {
  const read = useQuery({ queryKey: ["findings", objectType, id], queryFn: () => readFindings(objectType, id), retry: false })
  if (read.isPending) return <Panel icon={ShieldAlert} label="Findings"><Well className="p-3 text-meta text-muted-foreground">Reading the findings…</Well></Panel>
  if (read.isError) {
    const message = read.error instanceof NotServed ? "This cell does not serve findings yet." : read.error instanceof CellRefusal ? read.error.message : "The findings could not be read."
    return <Panel icon={ShieldAlert} label="Findings"><Well className="p-3 text-meta text-muted-foreground">{message}</Well></Panel>
  }
  const { quarantined, findings, evidence } = read.data.document
  return (
    <Panel icon={quarantined ? ShieldAlert : ShieldCheck} label="Findings" action={quarantined ? "Held out of authoritative reads" : undefined}>
      <Well className="space-y-3 p-3">
        {findings.length === 0 && <EmptyState icon={ShieldCheck} title="No findings">Every law the cell checks held when this record was imported.</EmptyState>}
        {quarantined && <p className="text-sm">This record is quarantined: an open finding keeps it out of answers and totals until a correction satisfies the law. Its source values stay readable here.</p>}
        {findings.map((finding) => (
          <article key={finding.id} className="space-y-1 rounded-md border bg-card p-3">
            <div className="flex flex-wrap items-center gap-2">
              <Badge variant={STATUS[finding.status].variant}>{STATUS[finding.status].label}</Badge>
              <span className="font-mono text-xs">{ruleName(finding.ruleRef)}</span>
              <span className="text-meta text-muted-foreground">{finding.severity} · first seen {finding.firstSeen}</span>
            </div>
            <p className="text-sm">{finding.message}</p>
          </article>
        ))}
        {evidence.length > 0 && <div className="space-y-2"><div className="text-meta text-muted-foreground">Where its values were read</div><EvidenceList refs={evidence} {...actionFor ? { actionFor } : {}} /></div>}
      </Well>
    </Panel>
  )
}
