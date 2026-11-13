import { useQuery } from "@tanstack/react-query"
import { ChevronRight, History as HistoryIcon, PencilLine, Workflow, Wrench, type LucideIcon } from "lucide-react"
import { useState } from "react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { CopyId } from "@/components/copy-id"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"
import { systemName } from "../lib/catalog.ts"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { readMetrics } from "../lib/metrics.ts"
import { atScale } from "../lib/objects.ts"
import { byValidTime, timePropertyOf, timeline, writerCounts, type TimelineEntry, type WriterKind } from "../lib/record-page.ts"
import { useCatalog } from "../sources.tsx"
import { changedProperties, readHistory, versionValue, type ObjectVersion } from "./review.ts"

/** Who or what wrote one version, in a line. */
function Writer({ version }: { readonly version: ObjectVersion }) {
  const writer = version.writer
  if (writer.kind === "mapping") {
    return <span>Imported from {systemName(writer.lineage.sourceRecordRef.system)} record <code className="text-xs">{writer.lineage.sourceRecordRef.record}</code> by <code className="text-xs">{writer.lineage.mappingPin.artifactId}</code></span>
  }
  if (writer.kind === "action") return <span><span className="font-medium">{writer.actor}</span> ran <code className="text-xs">{writer.actionId}</code></span>
  return <span>Written by a rule, an import or a migration</span>
}

const KIND: Readonly<Record<WriterKind, { readonly label: string; readonly icon: LucideIcon }>> = {
  mapping: { label: "Imports", icon: Workflow },
  action: { label: "Actions", icon: PencilLine },
  other: { label: "Other", icon: Wrench },
}

type Time = "known" | "valid"

/**
 * Every version of one record, newest first, one line each: what wrote it (an import with its source
 * record, or a governed action with its verified actor), what it changed, and its commit. A version
 * opens to its full change, reason and evidence. The timeline reads by knowledge time (when Norn
 * committed it, the server's order) or by valid time (the version's value of the time property the
 * cell places this type in time by); filters keep one kind of writer.
 */
export function History({ objectType, id }: { readonly objectType: string; readonly id: string }) {
  const read = useQuery({ queryKey: ["history", objectType, id], queryFn: () => readHistory(objectType, id), retry: false })
  const metrics = useQuery({ queryKey: ["metrics"], queryFn: readMetrics, retry: false, staleTime: 60_000 })
  const [time, setTime] = useState<Time>("known")
  const [only, setOnly] = useState<WriterKind | null>(null)
  const [open, setOpen] = useState<ReadonlySet<number>>(new Set())
  // Exact decimals are stored canonically (72.00 as 72): each is shown at its property's declared scale.
  const { catalog } = useCatalog()
  const scales = new Map((catalog?.byName.get(objectType)?.fields ?? []).flatMap(({ id: field, decimal }) => decimal?.scale === undefined ? [] : [[field, decimal.scale] as const]))
  if (read.isPending) return <Panel icon={HistoryIcon} label="History"><Well className="p-3 text-meta text-muted-foreground">Reading the history…</Well></Panel>
  if (read.isError) {
    const message = read.error instanceof NotServed ? "This cell does not serve record history yet." : read.error instanceof CellRefusal ? read.error.message : "The history could not be read."
    return <Panel icon={HistoryIcon} label="History"><Well className="p-3 text-meta text-muted-foreground">{message}</Well></Panel>
  }
  const history = read.data
  const { versions, truncated } = history.document
  const timeProperty = timePropertyOf(metrics.data?.document.metrics ?? [], objectType)
  const entries = timeline(versions, timeProperty)
  const counts = writerCounts(entries)
  const shown = (time === "valid" && timeProperty !== null ? byValidTime(entries) : entries).filter((entry) => only === null || entry.kind === only)
  const indexOf = new Map(versions.map((version, index) => [version.version, index]))
  const toggle = (version: number) => setOpen((current) => { const next = new Set(current); if (next.has(version)) next.delete(version); else next.add(version); return next })
  const chip = (kind: WriterKind | null, label: string, count: number) => (
    <button key={kind ?? "all"} type="button" aria-pressed={only === kind} onClick={() => setOnly(kind)}
      className={cn("inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-meta transition-colors", only === kind ? "border-foreground/20 bg-foreground/5 text-foreground" : "border-node-border text-muted-foreground hover:text-foreground")}>
      {label}<span className="tabular-nums">{count}</span>
    </button>
  )
  return (
    <Panel icon={HistoryIcon} label="History" action={
      <ToggleGroup type="single" variant="outline" size="sm" aria-label="Order by" value={time} onValueChange={(next) => { if (next === "known" || next === "valid") setTime(next) }}>
        <ToggleGroupItem value="known" className="px-2.5" title="When Norn committed each version">Knowledge time</ToggleGroupItem>
        <ToggleGroupItem value="valid" className="px-2.5" disabled={timeProperty === null}
          title={timeProperty === null ? "The cell names no time property for this type" : `When each version says it was true (${timeProperty.replaceAll("_", " ")})`}>Valid time</ToggleGroupItem>
      </ToggleGroup>
    }>
      <Well className="space-y-3 p-3">
        {versions.length === 0 ? <EmptyState icon={HistoryIcon}>No version of this record is stored.</EmptyState> : <>
          <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Show">
            {chip(null, "All", entries.length)}
            {(Object.keys(KIND) as WriterKind[]).filter((kind) => counts[kind] > 0).map((kind) => chip(kind, KIND[kind].label, counts[kind]))}
            {truncated && <span className="text-meta text-muted-foreground">in the newest {versions.length} versions</span>}
          </div>
          <ol className="divide-y divide-border/70 rounded-md border border-border/70 bg-card">
            {shown.map((entry) => <Entry key={entry.version} entry={entry} time={time} history={history} index={indexOf.get(entry.version) ?? 0} truncated={truncated} scales={scales} open={open.has(entry.version)} onToggle={() => toggle(entry.version)} />)}
          </ol>
        </>}
        {truncated && <p className="text-meta text-muted-foreground">Older versions exist; these are the newest the cell lists.</p>}
      </Well>
    </Panel>
  )
}

function Entry({ entry, time, history, index, truncated, scales, open, onToggle }: {
  readonly entry: TimelineEntry
  readonly time: Time
  readonly history: Awaited<ReturnType<typeof readHistory>>
  readonly index: number
  readonly truncated: boolean
  readonly scales: ReadonlyMap<string, number>
  readonly open: boolean
  readonly onToggle: () => void
}) {
  const versions = history.document.versions
  const version = versions[index]
  if (version === undefined) return null
  const first = index === versions.length - 1 && !truncated
  const changed = first ? [] : changedProperties(history, index)
  const Icon = KIND[entry.kind].icon
  const when = time === "valid" ? entry.valid ?? "no valid time" : entry.known
  const change = (property: string) => `${property.replaceAll("_", " ")} ${atScale(versionValue(history, index + 1, property), scales.get(property))} → ${atScale(versionValue(history, index, property), scales.get(property))}`
  const writer = version.writer
  return (
    <li>
      <button type="button" aria-expanded={open} onClick={onToggle} className="flex w-full items-start gap-2.5 px-3 py-2 text-start text-sm outline-none hover:bg-muted/50 focus-visible:ring-2 focus-visible:ring-ring/40">
        <ChevronRight aria-hidden className={cn("mt-0.5 size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")} />
        <Icon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span className="block [overflow-wrap:anywhere]"><Writer version={version} /></span>
          <span className="block truncate text-meta text-muted-foreground">{first ? "Created" : changed.length === 0 ? "No value changed" : `${change(changed[0] ?? "")}${changed.length > 1 ? ` and ${changed.length - 1} more` : ""}`}</span>
        </span>
        <span className="shrink-0 text-end text-meta text-muted-foreground">
          <span className={cn("block font-mono text-xs tabular-nums", time === "valid" && entry.valid === null && "italic")}>{when}</span>
          <span className="block tabular-nums">v{entry.version} · commit {entry.commit}</span>
        </span>
      </button>
      {open && (
        <div className="space-y-2 px-3 pb-3 ps-[3.25rem] text-sm">
          {changed.length > 0 && (
            <table className="w-full text-sm">
              <tbody>
                {changed.map((property) => (
                  <tr key={property}>
                    <td className="py-0.5 pe-3 text-muted-foreground">{property.replaceAll("_", " ")}</td>
                    <td className="py-0.5 pe-3 font-mono tabular-nums text-muted-foreground line-through">{atScale(versionValue(history, index + 1, property), scales.get(property))}</td>
                    <td className="py-0.5 font-mono tabular-nums">{atScale(versionValue(history, index, property), scales.get(property))}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {writer.kind === "action" && <ActionReason input={writer.input} />}
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 text-meta text-muted-foreground">
            <dt>Known</dt><dd className="font-mono text-xs">{entry.known}</dd>
            <dt>Valid</dt><dd className="font-mono text-xs">{entry.valid ?? "—"}</dd>
            {writer.kind === "mapping" && writer.lineage.documentDigest && <><dt>Source document</dt><dd><CopyId value={writer.lineage.documentDigest} short={12} /></dd></>}
          </dl>
        </div>
      )}
    </li>
  )
}

/** The reason and evidence an action's input carries, when it does. */
function ActionReason({ input }: { readonly input: Readonly<Record<string, unknown>> }) {
  const reason = input.correction_reason ?? input.reason
  const evidence = input.correction_evidence ?? input.evidence
  if (typeof reason !== "string" && typeof evidence !== "string") return null
  return (
    <div className="space-y-0.5 text-meta text-muted-foreground">
      {typeof reason === "string" && <div>Reason: <span className="text-foreground">{reason}</span></div>}
      {typeof evidence === "string" && <div>Evidence: <code>{evidence}</code></div>}
    </div>
  )
}
