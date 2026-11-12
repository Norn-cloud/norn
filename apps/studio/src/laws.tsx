import { useQuery } from "@tanstack/react-query"
import { Scale } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { EmptyState, Panel, Well } from "@/components/panel"
import { useMode } from "@/lib/live-transport"
import { readProfile } from "@/lib/profile"
import { useView } from "@/lib/view"
import type { Concept, Law } from "./lib/catalog.ts"

/** How the cell holds every admitted law: what the kernel does on a write and on a loaded record. */
export const ENFORCED = "The cell checks it on every write: a change that would break it is refused, naming the law. A record loaded from a connected system that breaks it is held in quarantine instead of being served."

const FORM: Record<string, string> = { always: "must always hold", never: "must never hold", eventually: "must eventually hold", until: "must hold until" }

function LawRow({ law }: { readonly law: Law }) {
  const { view } = useView()
  return (
    <div className="space-y-1 px-4 py-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{law.rationale}</span>
        <Badge variant={law.severity === "error" ? "destructive" : "outline"}>{law.severity === "error" ? "Refuses" : "Warns"}</Badge>
      </div>
      <p className="text-meta text-muted-foreground">{FORM[law.modality] ?? law.modality}{law.response ? `, as a ${law.response}` : ""} · owned by {law.owner}{view === "technical" ? <> · <code className="font-mono text-xs">{law.id}</code></> : null}</p>
    </div>
  )
}

/** A type's records the cell holds back, from the cell's own profile of what this reader may see. */
function Quarantined({ objectType }: { readonly objectType: string }) {
  const profile = useQuery({ queryKey: ["profile", objectType], queryFn: () => readProfile(objectType), retry: false })
  if (!profile.data) return null
  const { quarantinedRows, rows } = profile.data.document
  return <p className="px-4 pb-3 text-meta text-muted-foreground tabular-nums"><b className="font-medium text-foreground">{quarantinedRows}</b> {quarantinedRows === 1 ? "record is" : "records are"} in quarantine now, beside {rows} served.</p>
}

/** The laws a type must keep, how the cell holds them, and (live) how many of its records it holds back. */
export function LawsPanel({ concept }: { readonly concept: Concept }) {
  const live = useMode().data?.kind === "live"
  const invariants = concept.rules.filter((rule) => !concept.laws.some(({ rationale }) => rationale === rule))
  return (
    <Panel icon={Scale} label="Rules it must keep">
      <Well className="divide-y">
        {concept.laws.length === 0 && invariants.length === 0 && <EmptyState icon={Scale}>No law is declared on this object type itself.</EmptyState>}
        {concept.laws.map((law) => <LawRow key={law.id} law={law} />)}
        {invariants.map((rule) => <div key={rule} className="px-4 py-3 text-sm">{rule}</div>)}
        {(concept.laws.length > 0 || invariants.length > 0) && <p className="px-4 py-3 text-meta text-muted-foreground">{ENFORCED}</p>}
        {live && concept.definition === "object type" && <Quarantined objectType={concept.name} />}
      </Well>
    </Panel>
  )
}
