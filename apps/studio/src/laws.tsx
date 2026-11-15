import { useQuery } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { Scale, ShieldAlert, ShieldCheck, Wand2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Chip } from "@/components/chip"
import { EmptyState, Panel, Well } from "@/components/panel"
import { cellRead, useMode } from "@/lib/live-transport"
import { readProfile } from "@/lib/profile"
import { useView } from "@/lib/view"
import { conceptTitle, type Concept, type Law } from "./lib/catalog.ts"
import { RouteLink, type Group, type Inbox } from "./inbox.tsx"

/** How the cell holds every admitted law: what the kernel does on a write and on a loaded record. */
export const ENFORCED = "The cell checks it on every write: a change that would break it is refused, naming the law. A record loaded from a connected system that breaks it is held in quarantine instead of being served."

const FORM: Record<string, string> = { always: "must always hold", never: "must never hold", eventually: "must eventually hold", until: "must hold until" }

/**
 * One law: its name in plain words (the artifact's name, or its label in the reader's language),
 * why it exists, how the cell holds it and, live, the records it is holding back right now with
 * the fix the server suggests for each. Everything here is the server's: Studio judges nothing.
 */
function LawRow({ law, held }: { readonly law: Law; readonly held: Group | undefined }) {
  const { view } = useView()
  const headline = law.title && law.title !== law.id ? law.title : law.rationale
  return (
    <div className="flex gap-3 px-4 py-3 text-sm" data-law={law.id}>
      <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-green/10 text-tone-green dark:bg-tone-green/15"><ShieldCheck aria-hidden className="size-4" /></span>
      <div className="min-w-0 flex-1 space-y-1.5">
        <p className="font-medium text-pretty">{headline}</p>
        {headline !== law.rationale && <p className="text-meta text-pretty text-muted-foreground">{law.rationale}</p>}
        <div className="flex flex-wrap items-center gap-1.5">
          <Chip tone={law.severity === "error" ? "red" : "amber"} icon={law.severity === "error" ? ShieldAlert : undefined} dot={law.severity !== "error"}>{law.severity === "error" ? "Refuses a breaking change" : "Warns"}</Chip>
          <Chip tone="gray" title={law.response ? `as a ${law.response}` : undefined}>{FORM[law.modality] ?? law.modality}</Chip>
          {law.inheritedFrom && <Chip tone="gray" title="Declared on a kind of record this one is">from {conceptTitle(law.inheritedFrom)}</Chip>}
          {held && <Chip tone="amber" dot data-held={held.count}>{held.count}{held.approximate ? "+" : ""} held back</Chip>}
          {view === "technical" && <code className="font-mono text-xs text-muted-foreground">{law.id}</code>}
        </div>
        {held && held.records.length > 0 && (
          <div className="mt-1 divide-y rounded-lg border bg-card">
            {held.records.slice(0, 3).map((record) => (
              <div key={`${record.objectType}:${record.id}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2">
                <RouteLink to={record.target} className="font-medium underline-offset-4 hover:underline">{recordName(record)}</RouteLink>
                {record.finding && <span className="min-w-0 flex-1 text-muted-foreground">{record.finding.words}</span>}
                {record.suggestedFix && (
                  <Button asChild size="sm" variant="secondary" className="ms-auto" title={record.suggestedFix.title}>
                    <RouteLink to={record.target}><Wand2 aria-hidden />{record.suggestedFix.label}</RouteLink>
                  </Button>
                )}
              </div>
            ))}
            {held.count > 3 && <div className="px-3 py-2"><Button asChild size="sm" variant="ghost"><RouteLink to={held.target}>Review all {held.count}{held.approximate ? "+" : ""}</RouteLink></Button></div>}
          </div>
        )}
      </div>
    </div>
  )
}

/** A held-back record's name as the inbox gives it: a document number, a label, or its id. */
const recordName = (record: Group["records"][number]) => {
  const values = (record.values ?? {}) as Record<string, unknown>
  const named = [values.doc_number, values.label, values.display_name].find((value) => typeof value === "string" && value !== "")
  return typeof named === "string" ? named : record.id
}

/** The held-back group the inbox reports for `law`: the one whose rule is that law's artifact. */
export const heldBy = (groups: readonly Group[], law: Law): Group | undefined =>
  groups.find(({ ruleRef }) => ruleRef.kind === "law" && (ruleRef.artifactId === law.artifactId || ruleRef.lawId === law.id))

/** The cell's held-back groups for this reader, live only (the same inbox read Home makes). */
export function useHeldBack(): readonly Group[] {
  const live = useMode().data?.kind === "live"
  const inbox = useQuery({ queryKey: ["inbox"], queryFn: () => cellRead<Inbox>("/v1/inbox", "The inbox is"), retry: false, enabled: live, staleTime: 30_000 })
  return inbox.data?.document.items.filter((item): item is Group => item.kind === "held_back") ?? []
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
  const held = useHeldBack()
  const invariants = concept.rules.filter((rule) => !concept.laws.some(({ rationale }) => rationale === rule))
  return (
    <Panel icon={Scale} label={label}>
      <Well className="divide-y">
        {concept.laws.length === 0 && invariants.length === 0 && <EmptyState icon={Scale}>No law is declared on this object type itself.</EmptyState>}
        {concept.laws.map((law) => <LawRow key={law.id} law={law} held={heldBy(held, law)} />)}
        {invariants.map((rule) => <div key={rule} className="flex gap-3 px-4 py-3 text-sm"><span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-lg bg-tone-green/10 text-tone-green dark:bg-tone-green/15"><ShieldCheck aria-hidden className="size-4" /></span><p className="min-w-0 flex-1 font-medium text-pretty">{rule}</p></div>)}
        {enforced && (concept.laws.length > 0 || invariants.length > 0) && <p className="px-4 py-3 text-meta text-muted-foreground">{ENFORCED}</p>}
        {live && concept.definition === "object type" && <Quarantined objectType={concept.name} />}
      </Well>
    </Panel>
  )
}
