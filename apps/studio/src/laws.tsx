import { useQuery } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { Scale, ShieldAlert, ShieldCheck } from "lucide-react"
import { Chip } from "@/components/chip"
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
    <div className="flex gap-3 px-4 py-3 text-sm">
      <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-green/10 text-tone-green dark:bg-tone-green/15"><ShieldCheck aria-hidden className="size-4" /></span>
      <div className="min-w-0 flex-1 space-y-1.5">
        <p className="font-medium text-pretty">{law.rationale}</p>
        <div className="flex flex-wrap items-center gap-1.5">
          <Chip tone={law.severity === "error" ? "red" : "amber"} icon={law.severity === "error" ? ShieldAlert : undefined} dot={law.severity !== "error"}>{law.severity === "error" ? "Refuses" : "Warns"}</Chip>
          <Chip tone="gray" title={law.response ? `as a ${law.response}` : undefined}>{FORM[law.modality] ?? law.modality}</Chip>
          <Chip tone="violet" title="The pack or module that owns this law">{law.owner}</Chip>
          {view === "technical" && <code className="font-mono text-xs text-muted-foreground">{law.id}</code>}
        </div>
      </div>
    </div>
  )
}

/** A type's records the cell holds back, from the cell's own profile of what this reader may see. */
function Quarantined({ objectType }: { readonly objectType: string }) {
  const profile = useQuery({ queryKey: ["profile", objectType], queryFn: () => readProfile(objectType), retry: false })
  if (!profile.data) return null
  const { quarantinedRows, rows } = profile.data.document
  return (
    <div className="flex flex-wrap items-center gap-1.5 bg-surface-subtle px-4 py-2.5 text-meta text-muted-foreground tabular-nums">
      <Chip tone={quarantinedRows > 0 ? "amber" : "gray"} dot>{quarantinedRows} in quarantine</Chip>
      <Chip tone="teal" dot>{rows} served</Chip>
      <span className="sr-only">{quarantinedRows} {quarantinedRows === 1 ? "record is" : "records are"} in quarantine now, beside {rows} served.</span>
    </div>
  )
}

/** The laws a type must keep, how the cell holds them, and (live) how many of its records it holds back. */
export function LawsPanel({ concept, label = "Rules it must keep", enforced = true }: { readonly concept: Concept; readonly label?: ReactNode; readonly enforced?: boolean }) {
  const live = useMode().data?.kind === "live"
  const invariants = concept.rules.filter((rule) => !concept.laws.some(({ rationale }) => rationale === rule))
  return (
    <Panel icon={Scale} label={label}>
      <Well className="divide-y">
        {concept.laws.length === 0 && invariants.length === 0 && <EmptyState icon={Scale}>No law is declared on this object type itself.</EmptyState>}
        {concept.laws.map((law) => <LawRow key={law.id} law={law} />)}
        {invariants.map((rule) => <div key={rule} className="flex gap-3 px-4 py-3 text-sm"><span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-green/10 text-tone-green dark:bg-tone-green/15"><ShieldCheck aria-hidden className="size-4" /></span><p className="min-w-0 flex-1 font-medium text-pretty">{rule}</p></div>)}
        {enforced && (concept.laws.length > 0 || invariants.length > 0) && <p className="px-4 py-3 text-meta text-muted-foreground">{ENFORCED}</p>}
        {live && concept.definition === "object type" && <Quarantined objectType={concept.name} />}
      </Well>
    </Panel>
  )
}
