import { useQuery } from "@tanstack/react-query"
import { ChevronRight, History as HistoryIcon, PencilLine, Workflow, Wrench, type LucideIcon } from "lucide-react"
import { Fragment, useState } from "react"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { CopyId } from "@/components/copy-id"
import { HiddenWriter } from "@/components/hidden-writer"
import { Segmented, SegmentedItem } from "@/components/segmented"
import { cn } from "@/lib/utils"
import { systemName } from "../lib/catalog.ts"
import { useStudioScope } from "../lib/definitions.ts"
import { readMetrics } from "../lib/metrics.ts"
import { atScale } from "../lib/objects.ts"
import { readableTime } from "../lib/format.ts"
import { useView } from "../lib/view.tsx"
import { byValidTime, timePropertyOf, timeline, writerCounts, type TimelineEntry, type WriterKind } from "../lib/record-page.ts"
import { useCatalog } from "../sources.tsx"
import { useTypeAccess } from "../record-rail.tsx"
import { changedProperties, historyQueryKey, historyUnavailable, readHistory, versionValue, type ObjectVersion } from "./review.ts"

/**
 * Who or what wrote one version, in a line, by the names the server gives (the same as the record's
 * "Last changes"): an action by its declared title and its verified actor, an import by its source
 * system. The ids those names stand for are in the version's details, under Technical.
 */
function Writer({ version, actionTitles }: { readonly version: ObjectVersion; readonly actionTitles: ReadonlyMap<string, string> }) {
  const writer = version.writer
  if (writer.kind === "mapping") return <span>Imported from {systemName(writer.lineage.sourceRecordRef.system)}</span>
  if (writer.kind === "action") return <span>{actionTitles.get(writer.actionId) ?? "Ran an action"} <span className="text-muted-foreground">by {writer.actor}</span></span>
  return <HiddenWriter />
}

/** An id that may break only after its separators (`.`, `/`, `_`, `:`, `-`), never mid-token, unless one token is wider than the line. */
function IdText({ value }: { readonly value: string }) {
  return <code className="font-mono [overflow-wrap:break-word]">{value.split(/(?<=[./_:-])/).map((part, index) => <Fragment key={index}>{part}<wbr /></Fragment>)}</code>
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
 * opens to its full change. The timeline reads by knowledge time (when Norn
 * committed it, the server's order) or by valid time (the version's value of the time property the
 * cell places this type in time by); filters keep one kind of writer.
 */
export function History({ objectType, id }: { readonly objectType: string; readonly id: string }) {
  const scope = useStudioScope()
  const boundScope = scope.data ?? null
  const scopeKey = boundScope ? `${boundScope.orgId}:${boundScope.environmentId}` : ""
  const read = useQuery({ queryKey: historyQueryKey(objectType, id, boundScope), queryFn: () => readHistory(objectType, id, boundScope), enabled: scope.isSuccess, retry: false })
  const metrics = useQuery({ queryKey: ["metrics", scopeKey], queryFn: () => readMetrics(boundScope), enabled: scope.isSuccess, retry: false, staleTime: 60_000 })
  // The actions' titles come from the cell's access read for the type, as the rail's "Last changes" reads them.
  const access = useTypeAccess(objectType, true)
  const actionTitles = new Map(access.data?.kind === "read" ? access.data.read.document.roles.flatMap(({ actions }) => actions.map(({ actionId, title }) => [actionId, title] as const)) : [])
  const [time, setTime] = useState<Time>("known")
  const [only, setOnly] = useState<WriterKind | null>(null)
  const [open, setOpen] = useState<ReadonlySet<number>>(new Set())
  // Exact decimals are stored canonically (72.00 as 72): each is shown at its property's declared scale.
  const { catalog } = useCatalog()
  const scales = new Map((catalog?.byName.get(objectType)?.fields ?? []).flatMap(({ id: field, decimal }) => decimal?.scale === undefined ? [] : [[field, decimal.scale] as const]))
  if (read.isPending) return <Panel icon={HistoryIcon} label="History"><Well className="p-3 text-meta text-muted-foreground">Reading the history…</Well></Panel>
  if (read.isError) return <Panel icon={HistoryIcon} label="History"><Well className="p-3 text-meta text-muted-foreground"><span data-history-unavailable="">{historyUnavailable(read.error)}</span></Well></Panel>
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
      className={cn("inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-meta transition-colors max-sm:min-h-[44px]", only === kind ? "border-foreground/20 bg-foreground/5 text-foreground" : "border-node-border text-muted-foreground hover:text-foreground")}>
      {label}<span className="tabular-nums">{count}</span>
    </button>
  )
  return (
    <Panel icon={HistoryIcon} label="History" action={timeProperty === null
      // With no time property there is one order to read by: it is said, not offered as a control that can't be used.
      ? <span data-order="known" title="The cell names no time property for this type, so versions are in the order Norn committed them">By knowledge time</span>
      : (
        <Segmented aria-label="Order by" value={time} onValueChange={(next) => { if (next === "known" || next === "valid") setTime(next) }}>
          <SegmentedItem value="known" title="When Norn committed each version">Knowledge time</SegmentedItem>
          <SegmentedItem value="valid" title={`When each version says it was true (${timeProperty.replaceAll("_", " ")})`}>Valid time</SegmentedItem>
        </Segmented>
      )
    }>
      <Well className="space-y-3 p-3">
        {versions.length === 0 ? <EmptyState icon={HistoryIcon}>No version of this record is stored.</EmptyState> : <>
          <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Show">
            {chip(null, "All", entries.length)}
            {(Object.keys(KIND) as WriterKind[]).filter((kind) => counts[kind] > 0).map((kind) => chip(kind, KIND[kind].label, counts[kind]))}
            {truncated && <span className="text-meta text-muted-foreground">in the newest {versions.length} versions</span>}
          </div>
          <ol className="divide-y divide-border/70 rounded-md border border-border/70 bg-card">
            {shown.map((entry) => <Entry key={entry.version} entry={entry} time={time} history={history} index={indexOf.get(entry.version) ?? 0} truncated={truncated} scales={scales} open={open.has(entry.version)} onToggle={() => toggle(entry.version)} actionTitles={actionTitles} />)}
          </ol>
        </>}
        {truncated && <p className="text-meta text-muted-foreground">Older versions exist; these are the newest the cell lists.</p>}
      </Well>
    </Panel>
  )
}

function Entry({ entry, time, history, index, truncated, scales, open, onToggle, actionTitles }: {
  readonly actionTitles: ReadonlyMap<string, string>
  readonly entry: TimelineEntry
  readonly time: Time
  readonly history: Awaited<ReturnType<typeof readHistory>>
  readonly index: number
  readonly truncated: boolean
  readonly scales: ReadonlyMap<string, number>
  readonly open: boolean
  readonly onToggle: () => void
}) {
  const { view } = useView()
  const versions = history.document.versions
  const version = versions[index]
  if (version === undefined) return null
  const first = index === versions.length - 1 && !truncated
  const changed = first ? [] : changedProperties(history, index)
  const Icon = KIND[entry.kind].icon
  const when = time === "valid" ? entry.valid === null ? "no valid time" : view === "technical" ? entry.valid : readableTime(entry.valid) : view === "technical" ? entry.known : readableTime(entry.known)
  const change = (property: string) => `${property.replaceAll("_", " ")} ${atScale(versionValue(history, index + 1, property), scales.get(property))} → ${atScale(versionValue(history, index, property), scales.get(property))}`
  const writer = version.writer
  return (
    <li>
      <button type="button" aria-expanded={open} onClick={onToggle} className="flex w-full items-start gap-2.5 px-3 py-2 text-start text-body outline-none hover:bg-(--well-hover) focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring focus-visible:outline-solid max-sm:min-h-[44px]">
        <ChevronRight aria-hidden className={cn("mt-[3px] size-4 shrink-0 text-muted-foreground transition-transform rtl:rotate-180", open && "rotate-90 rtl:rotate-90")} />
        <Icon aria-hidden strokeWidth={1.6} className="mt-[3px] size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span className="block text-pretty"><Writer version={version} actionTitles={actionTitles} /></span>
          {/* A change line wraps rather than losing its end on a phone. */}
          <span className="block text-meta text-pretty text-muted-foreground [overflow-wrap:break-word]">{first ? "Created" : changed.length === 0 ? "No value changed" : `${change(changed[0] ?? "")}${changed.length > 1 ? ` and ${changed.length - 1} more` : ""}`}</span>
        </span>
        {/* Dates read as the rail's do: the shared readable format, mono only in Technical (where they are raw). */}
        <span className="shrink-0 text-end text-meta text-muted-foreground">
          <span className={cn("block tabular-nums", view === "technical" && "font-mono", time === "valid" && entry.valid === null && "italic")}>{when}</span>
          <span className="block tabular-nums">commit {entry.commit}</span>
        </span>
      </button>
      {open && (
        <div className="space-y-2 px-3 pb-3 ps-[3.25rem] text-body">
          {changed.length > 0 && (
            <table className="w-full text-body">
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
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 text-meta text-muted-foreground" aria-label="Technical">
            <dt>Known</dt><dd className="font-mono">{entry.known}</dd>
            <dt>Valid</dt><dd className="font-mono">{entry.valid ?? "—"}</dd>
            {writer.kind === "action" && <><dt>Action</dt><dd><IdText value={writer.actionId} /></dd></>}
            {writer.kind === "mapping" && <>
              <dt>Source record</dt><dd><IdText value={writer.lineage.sourceRecordRef.record} /></dd>
              <dt>Mapping</dt><dd><IdText value={writer.lineage.mappingPin.artifactId} /></dd>
            </>}
            {writer.kind === "mapping" && writer.lineage.documentDigest && <><dt>Source document</dt><dd><CopyId value={writer.lineage.documentDigest} short={12} /></dd></>}
          </dl>
        </div>
      )}
    </li>
  )
}
