import { Chip, FactChip } from "@/components/chip"
import { useQuery } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { CircleAlert, CircleCheck, CircleDashed, CircleDot, ShieldAlert, ShieldCheck, type LucideIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Panel, Well } from "@/components/panel"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { useLocale } from "@/lib/locale"
import { transport } from "@/lib/transport"
import { catalogOf } from "../lib/catalog.ts"
import { compared, isList, roleOf } from "../lib/law-values.ts"
import { atScale } from "../lib/objects.ts"
import { EvidenceList } from "./evidence-crop.tsx"
import { readFindings, type Evidence, type Finding, type RuleRef } from "./review.ts"

/** The rule a finding records, by its stable id. */
function ruleName(rule: RuleRef): string {
  if (rule.kind === "law") return rule.lawId
  if (rule.kind === "builtin") return rule.id
  return "an invariant"
}

const STATUS: Record<Finding["status"], { readonly label: string; readonly variant: "destructive" | "warning" | "success" | "outline"; readonly icon: LucideIcon }> = {
  open: { label: "Open", variant: "destructive", icon: CircleAlert },
  acknowledged: { label: "Acknowledged", variant: "warning", icon: CircleDot },
  resolved: { label: "Resolved", variant: "success", icon: CircleCheck },
  superseded: { label: "Superseded", variant: "outline", icon: CircleDashed },
}

/** A finding's state: its tone and its own icon, so the state never rests on colour alone. */
function StatusBadge({ status }: { readonly status: Finding["status"] }) {
  const { label, variant, icon: Icon } = STATUS[status]
  return <Badge variant={variant}><Icon aria-hidden />{label}</Badge>
}

/**
 * DIAG-REASON-1: the values a law finding compared, as the cell recorded them when the law read
 * them (never the record's current values), in the order the law states them. A list is every
 * line the law summed, shown as its exact sum and its parts: "72.00 = 30.00 + 40.00 + 2.00".
 */
function ComparedValues({ objectType, finding }: { readonly objectType: string; readonly finding: Finding }) {
  const { locale } = useLocale()
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const values = finding.values
  if (values === undefined || Object.keys(values).length === 0) return null
  const concept = catalog.data ? catalogOf(catalog.data.document, locale).byName.get(objectType) : undefined
  const ref = finding.ruleRef
  const law = ref.kind === "law" ? concept?.laws.find(({ id }) => id === ref.lawId) : undefined
  const paths = law?.compares?.every((path) => Object.hasOwn(values, path)) ? [...law.compares] : Object.keys(values).sort()
  const fieldOf = (path: string) => concept?.fields.find(({ id }) => id === path)
  const scale = paths.map((path) => fieldOf(path)?.decimal?.scale).find((found) => found !== undefined)
  const sides = paths.map((path) => {
    const value = compared(values[path])
    const field = fieldOf(path)
    const label = field?.label ?? (isList(values[path]) ? `sum of ${roleOf(path).replaceAll("_", " ")}` : path.replaceAll(".", " ").replaceAll("_", " "))
    const text = atScale(value.text, field?.decimal?.scale ?? scale)
    const parts = value.parts && value.parts.length > 1 ? ` = ${value.parts.map((part) => atScale(part, scale)).join(" + ")}` : ""
    return { path, label, shown: `${text}${parts}` }
  })
  return (
    <div className="flex flex-wrap items-center gap-1.5 pt-1 tabular-nums" aria-label="The values the law compared">
      {sides.map(({ path, label, shown }, index) => <span key={path} className="contents">
        {index > 0 && <span aria-label={sides.length === 2 ? "does not equal" : undefined} className="text-meta text-muted-foreground">{sides.length === 2 ? "≠" : "·"}</span>}
        <FactChip label={label} className={index === 0 && finding.status === "open" ? "[&>span:last-child]:text-tone-red" : undefined}>{shown}</FactChip>
      </span>)}
    </div>
  )
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
    <Panel icon={quarantined ? ShieldAlert : ShieldCheck} label="Findings" action={quarantined ? <Chip tone="amber" dot title="Held out of authoritative reads">Held out of authoritative reads</Chip> : undefined}>
      <Well className="space-y-3 p-3">
        {findings.length === 0 && <p className="flex flex-wrap items-center gap-2 text-sm"><Chip tone="green" icon={ShieldCheck}>No findings</Chip><span className="text-muted-foreground">Every law the cell checks held when this record was imported.</span></p>}
        {quarantined && <p className="text-sm">This record is quarantined: an open finding keeps it out of answers and totals until a correction satisfies the law. Its source values stay readable here.</p>}
        {findings.map((finding) => (
          <article key={finding.id} className="space-y-1 rounded-md border bg-card p-3">
            <div className="flex flex-wrap items-center gap-2">
              <StatusBadge status={finding.status} />
              <span className="font-mono text-xs">{ruleName(finding.ruleRef)}</span>
              <span className="text-meta text-muted-foreground">{finding.severity} · first seen {finding.firstSeen}</span>
            </div>
            <p className="text-sm">{finding.message}</p>
            <ComparedValues objectType={objectType} finding={finding} />
          </article>
        ))}
        {evidence.length > 0 && <div className="space-y-2"><div className="text-meta font-medium text-muted-foreground">Where its values were read</div><EvidenceList refs={evidence} record={{ objectType, id }} {...actionFor ? { actionFor } : {}} /></div>}
      </Well>
    </Panel>
  )
}
