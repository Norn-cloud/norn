import { Chip, FactChip } from "@/components/chip"
import { useQuery } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { CircleAlert, CircleCheck, CircleDashed, CircleDot, ShieldAlert, ShieldCheck, type LucideIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Panel, Well } from "@/components/panel"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { useLocale } from "@/lib/locale"
import { transport } from "@/lib/transport"
import { catalogOf, lawHeadline } from "../lib/catalog.ts"
import { readableTime } from "../lib/format.ts"
import { useView } from "../lib/view.tsx"
import { Disclosure } from "@/components/disclosure"
import { compared, isList, roleOf } from "../lib/law-values.ts"
import { atScale } from "../lib/objects.ts"
import { EvidenceList } from "./evidence-crop.tsx"
import type { Evidence, Finding, RuleRef } from "./review.ts"
import type { StudioScope } from "../lib/definitions.ts"
import { findingsQuery } from "../lib/record-read.ts"

/** The rule a finding records, by its stable id: shown only under Technical, beside the rule's name. */
function ruleId(rule: RuleRef): string | undefined {
  if (rule.kind === "law") return rule.lawId
  if (rule.kind === "builtin") return rule.id
  return undefined
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
export function Findings({ objectType, id, scope, actionFor, quarantineStated = false }: {
  readonly objectType: string; readonly id: string
  /** The workspace the record was read at: its findings are read and cached there (#1745). */
  readonly scope: StudioScope | undefined
  readonly actionFor?: (evidence: Evidence) => ReactNode
  /** The page already says, once, that the record is held out of answers: the panel lists the findings only. */
  readonly quarantineStated?: boolean
}) {
  const read = useQuery(findingsQuery(objectType, id, scope))
  const { locale } = useLocale()
  const { view } = useView()
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const laws = catalog.data ? catalogOf(catalog.data.document, locale).byName.get(objectType)?.laws ?? [] : []
  if (read.isPending) return <Panel icon={ShieldAlert} label="Findings"><Well className="p-3 text-meta text-muted-foreground">Reading the findings…</Well></Panel>
  if (read.isError) {
    const message = read.error instanceof NotServed ? "This cell does not serve findings yet." : read.error instanceof CellRefusal ? read.error.message : "The findings could not be read."
    return <Panel icon={ShieldAlert} label="Findings"><Well className="p-3 text-meta text-muted-foreground">{message}</Well></Panel>
  }
  const { quarantined, findings, evidence } = read.data.document
  const said = quarantined && !quarantineStated
  return (
    <Panel icon={quarantined ? ShieldAlert : ShieldCheck} label="Findings" action={said ? <Chip tone="amber" dot title="Held out of authoritative reads">Held out of authoritative reads</Chip> : undefined}>
      <Well className="space-y-3 p-3">
        {findings.length === 0 && <p className="flex flex-wrap items-center gap-2 text-body"><Chip tone="green" icon={ShieldCheck}>No findings</Chip><span className="text-muted-foreground">Every law the cell checks held when this record was imported.</span></p>}
        {said && <p className="text-body">This record is quarantined: an open finding keeps it out of answers and totals until a correction satisfies the law. Its source values stay readable here.</p>}
        {findings.map((finding) => {
          // The rule by the name its definition gives it, as Home names it; its id only under Technical.
          const ref = finding.ruleRef
          const law = ref.kind === "law" ? laws.find(({ id: lawId }) => lawId === ref.lawId) : undefined
          const rid = ruleId(ref)
          return (
            <article key={finding.id} data-finding={finding.id} className="space-y-1.5 rounded-(--radius-row) bg-card p-3 ring-1 ring-line">
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                <StatusBadge status={finding.status} />
                <span lang={law?.titleLang} className="min-w-0 text-body text-foreground text-pretty">{law ? lawHeadline(law) : ref.kind === "law" || ref.kind === "builtin" ? "A rule this record breaks" : "An invariant"}</span>
                <span className="text-meta text-muted-foreground">{finding.severity} · first seen <time dateTime={finding.firstSeen} title={finding.firstSeen}>{view === "technical" ? finding.firstSeen : readableTime(finding.firstSeen)}</time></span>
              </div>
              <p className="text-body text-muted-foreground">{finding.message}</p>
              <ComparedValues objectType={objectType} finding={finding} />
              {rid !== undefined && (
                <Disclosure open={view === "technical" || law === undefined} summary="Technical" className="text-meta text-muted-foreground">
                  <code className="block ps-5 pt-1 font-mono [overflow-wrap:anywhere]">{rid}</code>
                </Disclosure>
              )}
            </article>
          )
        })}
        {evidence.length > 0 && <div className="space-y-2"><div className="text-meta text-muted-foreground">Where its values were read</div><EvidenceList refs={evidence} record={{ objectType, id }} {...actionFor ? { actionFor } : {}} /></div>}
      </Well>
    </Panel>
  )
}
