import { useQuery, useQueryClient } from "@tanstack/react-query"
import { MarkingChips, useMarkingCatalog } from "@/components/marking-chip"
import { Link, useParams } from "@tanstack/react-router"
import { LifecycleStepper } from "@/components/lifecycle-stepper"
import { Clock, GitCommitHorizontal, Pause, ShieldX } from "lucide-react"
import { Chip, FactChip } from "@/components/chip"
import { FactStatusBadge, deriveFactStatus } from "@/components/fact-status-badge"
import { Braces, Database, EyeOff, GitBranch, History as HistoryIcon, Link2, RefreshCw, Rows3, ShieldCheck, type LucideIcon } from "lucide-react"
import { Fragment, useEffect, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { CopyId, DigestId, PathText } from "@/components/copy-id"
import { FactProvenance, ProvenanceChain } from "@/components/provenance"
import { useInspector } from "@/lib/inspector"
import { KINDS, STATUS } from "@/lib/kinds"
import { useView } from "@/lib/view"
import { useViewer } from "@/lib/viewer"
import { Code } from "@/components/code"
import { cn } from "@/lib/utils"
import { TableCell, TableHead, TableRow } from "@/components/ui/table"
import { conceptTitle, systemName, type Concept } from "./lib/catalog.ts"
import { useMode } from "./lib/live-transport.ts"
import { LinkedRecords } from "./linked-records.tsx"
import { RecordActions } from "./actions/record-actions.tsx"
import { QuarantinedRecord } from "./quarantined-record.tsx"
import { EvidenceList } from "./evidence/evidence-crop.tsx"
import { Findings } from "./evidence/findings.tsx"
import { History } from "./evidence/history.tsx"
import { readFindings, readHistory, SOURCE_DOCUMENT, type Evidence, type EvidenceRef, type Finding } from "./evidence/review.ts"
import { useEvidenceActions } from "./actions/evidence-actions.tsx"
import { atScale, exactPretty, factDisplay, readProblem, recordName } from "./lib/objects.ts"
import { compared, isList, roleOf } from "./lib/law-values.ts"
import { stamp } from "./lib/format.ts"
import { transport, type ObjectReads, type SourceLineage } from "./lib/transport.ts"
import { countText, linkGroups, type CountedLinkPage, type LinkGroup } from "./lib/record-page.ts"
import { maskedOf } from "./lib/privacy.ts"
import { MaskedValue } from "@/components/masked-value"
import { SourceChip } from "@/components/source-chip"
import { RecentChanges, RulesKept, WhoCanAct, useTypeAccess } from "./record-rail.tsx"
import { RecordLineage } from "./record-lineage.tsx"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { liveAccess } from "./lib/live.ts"
import { RefusedNote } from "./questions.tsx"
import { DenseTable, ObjectTypePage, RECORDED_HINT, useCatalog } from "./sources.tsx"

/**
 * `/sources/$concept/$`: a record, its id the rest of the path. The splat also
 * matches the object type's own URL with an empty id, which is the type's page.
 */
export function SourcesItemPage() {
  const { _splat: id = "" } = useParams({ strict: false })
  return id === "" ? <ObjectTypePage /> : <ObjectPage />
}

/**
 * Everything known about one record. The overview reads like a record sheet: its key facts in the
 * header, its story on the left (summary, findings, every version) and its facts and links on the
 * right. Each value opens where it came from.
 */
export function ObjectPage() {
  // The id is the rest of the path: ids hold `/`, and Cloudflare redirects an encoded `%2F` to a decoded one, so it cannot be one segment.
  const { concept: name = "", _splat: id = "" } = useParams({ strict: false })
  const { read: catalogRead, catalog } = useCatalog()
  const concept = catalog?.byName.get(name)
  const reads = useQuery({ queryKey: ["object", name, id], queryFn: () => transport.object(name, id) })
  // The header names the version this read shows and, from the history, the commit that wrote it: a
  // read again reads both, so neither is left from an earlier read.
  const queries = useQueryClient()
  const readAgain = () => { void reads.refetch(); void queries.invalidateQueries({ queryKey: ["history", name, id] }) }
  const live = useMode().data?.kind === "live"
  // Where each value was read, when a document scan carries it: the evidence the cell links to the record.
  const review = useQuery({ queryKey: ["findings", name, id], queryFn: () => readFindings(name, id), enabled: live, retry: false })
  const evidence = review.data?.document.evidence ?? []
  const broken = review.data?.document.findings.filter(({ status }) => status === "open") ?? []
  const heldBack = review.data?.document.quarantined === true
  const corrections = useEvidenceActions(name, id)
  // `#lineage` (a traced number's record) opens on where it came from.
  const [tab, setTab] = useState(() => typeof location !== "undefined" && location.hash === "#lineage" ? "lineage" : "overview")
  const { view } = useView()
  const data = reads.data
  const lineage = data?.source?.document
  const problem = readProblem(catalogRead, reads)
  const { icon: TypeIcon, text, fill } = KINDS.objectType
  // The cell's own decisions over this type: the reader's Sources grant and each role's actions.
  const typeAccess = useTypeAccess(name, live)
  const access = typeAccess.data?.kind === "read" ? typeAccess.data.read.document : undefined
  const sources = access?.capabilities.sources.decision
  const actionTitles = new Map(access?.roles.flatMap(({ actions }) => actions.map(({ actionId, title }) => [actionId, title] as const)) ?? [])
  const recordMeta = data?.object.document as ({
    readonly withheld?: boolean
    readonly withheldProperties?: readonly string[]
    readonly masked?: Readonly<Record<string, unknown>>
    readonly verificationStatus?: string
  } | undefined)
  const recordWithheld = recordMeta?.withheld === true
    || (Array.isArray(recordMeta?.withheldProperties) && recordMeta.withheldProperties.length > 0)
    || (recordMeta?.masked !== undefined && Object.keys(recordMeta.masked).length > 0)
  // Never infer "Verified fact" from an empty/filtered findings response or an unreadable record.
  const recordStatus = live && reads.isSuccess && data !== null
    ? deriveFactStatus({
      quarantined: heldBack,
      openFindings: broken.length,
      withheld: recordWithheld,
      unverifiedImport: recordMeta?.verificationStatus === "imported_unverified",
      verified: review.isSuccess && recordMeta?.verificationStatus === "verified_fact",
    })
    : live && review.isSuccess && (heldBack || broken.length > 0)
      ? "quarantined"
      : null
  return (
    <section className="space-y-6">
      <header className="flex flex-wrap items-start gap-4">
        <span className={cn("flex size-10 shrink-0 items-center justify-center rounded-lg border border-border/60", fill, text)}><TypeIcon className="size-5" /></span>
        <div className="min-w-0 flex-1 space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="min-w-0 text-title font-semibold tracking-tight [overflow-wrap:break-word]"><PathText value={data ? recordName(data.object.document) : id} /></h1>
            <Chip tone="violet" icon={TypeIcon}>{concept?.title ?? conceptTitle(name)}</Chip>
            {data?.object.origin === "recorded" && <Chip tone="gray" title={RECORDED_HINT}>Recorded</Chip>}
            {recordStatus !== null && <FactStatusBadge status={recordStatus} />}
            {heldBack && <Chip tone="amber" icon={Pause} title="An open finding keeps this record out of answers and totals until a correction satisfies the law.">Held back</Chip>}
            {broken.length > 0 && <Chip tone="red" dot>{broken.length} {broken.length === 1 ? "rule broken" : "rules broken"}</Chip>}
            {data?.source?.document && <SourceChip system={data.source.document.sourceRecordRef.system} title="The connected system this record was read from" />}
          </div>
          {data && <div className="flex flex-wrap items-center gap-1.5 text-muted-foreground">
            <CopyId value={id} />
            <AsOf objectType={name} id={id} version={data.object.document.version} live={data.object.origin === "live"} />
            {data.object.origin === "live" && <Button type="button" variant="ghost" size="sm" disabled={reads.isFetching} onClick={readAgain} title="Read this record from the cell again">
              <RefreshCw className={reads.isFetching ? "animate-spin" : undefined} />Read again
            </Button>}
          </div>}
          {data?.object.origin === "live" && <div className="pt-1"><RecordActions objectType={name} id={id} onDone={readAgain} /></div>}
        </div>
      </header>
      {data && concept && broken[0] && <Broken concept={concept} reads={data} finding={broken[0]} heldBack={heldBack} />}
      {problem !== undefined && <p role="alert" className="text-destructive">{problem}</p>}
      {problem === undefined && (reads.isPending || catalogRead.isPending) && <p className="text-muted-foreground">Reading the record…</p>}
      {reads.isError && <p className="text-destructive">{reads.error.message}</p>}
      {reads.isSuccess && data === null && (live
        ? <QuarantinedRecord objectType={name} id={id} onDone={readAgain} />
        : <p className="text-muted-foreground">This record is not available yet.</p>)}
      {data && concept && (
        <Tabs value={tab} onValueChange={setTab}>
          <TabsList variant="line" className="w-full justify-start overflow-x-auto">
            {TABS.map(([value, label]) => <TabsTrigger key={value} value={value} className="flex-none">{label}</TabsTrigger>)}
          </TabsList>
          <TabsContent value="overview" className="pt-4">
            <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_24rem]">
              <div className="min-w-0 space-y-6">
                {view === "technical" && <p className="text-pretty text-muted-foreground">{summaryOf(concept, data, catalog?.concepts ?? [])}</p>}
                <Facts concept={concept} reads={data} onLineage={() => setTab("lineage")} sources={sources} />
                {concept.lifecycles.map((lifecycle) => (
                  <Panel key={lifecycle.property} icon={GitBranch} label={`Lifecycle · ${lifecycle.property.replaceAll("_", " ")}`}>
                    <Well className="px-4 py-3"><LifecycleStepper lifecycle={lifecycle} record={data.object.document} /></Well>
                  </Panel>
                ))}
                {live && <Findings objectType={name} id={id} actionFor={corrections.actionFor} />}
                {live ? <div id="record-history" className="scroll-mt-6"><History objectType={name} id={id} /></div> : <Panel icon={HistoryIcon} label="History"><Well>
                  <Empty icon={HistoryIcon} action={<Button variant="outline" size="sm" onClick={() => setTab("lineage")}>See where it came from</Button>}>A recorded snapshot keeps no change history. Version {data.object.document.version} is the one stored.</Empty>
                </Well></Panel>}
                {view === "technical" && <Technical name={name} lineage={lineage} />}
              </div>
              <aside className="min-w-0 space-y-6" aria-label="Who can act, rules, changes and links">
                <WhoCanAct objectType={name} live={live} />
                <RulesKept rules={concept.rules} title={concept.title} />
                {live && <RecentChanges objectType={name} id={id} actionTitles={actionTitles} onAll={() => document.getElementById("record-history")?.scrollIntoView({ behavior: "smooth", block: "start" })} />}
                <LinksRail concept={concept} id={id} links={data.links.document} concepts={catalog?.concepts ?? []} live={live} />
              </aside>
            </div>
          </TabsContent>
          <TabsContent value="lineage" className="space-y-6 pt-4">
            {lineage && <RecordLineage root={{ objectType: data.object.document.objectType, key: id, version: data.object.document.version }} lineage={lineage} live={live} />}
            <Lineage lineage={lineage} evidence={evidence} actionFor={corrections.actionFor} />
          </TabsContent>
          <TabsContent value="guarantees" className="pt-4">
            <Panel icon={ShieldCheck} label="Laws over this object type"><Well className="p-3">
              {concept.rules.length === 0
                ? <Empty icon={ShieldCheck} action={<Button asChild variant="outline" size="sm"><Link to="/guarantees">See every guarantee</Link></Button>}>No law is declared over {concept.title.toLowerCase()} records.</Empty>
                : <ul className="space-y-2 text-sm">{concept.rules.map((rule) => <li key={rule} className="flex gap-2"><ShieldCheck className={cn("mt-0.5 size-4 shrink-0", KINDS.guarantee.text)} />{rule}</li>)}</ul>}
            </Well></Panel>
          </TabsContent>
        </Tabs>
      )}
      {corrections.sheet(() => { readAgain(); void review.refetch() })}
    </section>
  )
}

const TABS = [["overview", "Overview"], ["lineage", "Lineage"], ["guarantees", "Guarantees"]] as const

/**
 * Which read this page shows: the version and, live, the commit that wrote it and when, from the
 * record's history (the same read its History panel uses). A recorded snapshot says so.
 */
function AsOf({ objectType, id, version, live }: { readonly objectType: string; readonly id: string; readonly version: number; readonly live: boolean }) {
  const history = useQuery({ queryKey: ["history", objectType, id], queryFn: () => readHistory(objectType, id), enabled: live, retry: false })
  const entry = history.data?.document.versions.find((candidate) => candidate.version === version)
  return (
    <span className="contents" title={entry ? "The commit that wrote this version, and its logical time" : undefined}>
      <FactChip label="version">{version}</FactChip>
      {entry ? <>
        <FactChip icon={GitCommitHorizontal} label="commit">{entry.commit}</FactChip>
        <FactChip icon={Clock} label="as of"><time dateTime={new Date(entry.logicalTime).toISOString()} title={new Date(entry.logicalTime).toISOString()}>{stamp(entry.logicalTime)}</time></FactChip>
      </> : null}
    </span>
  )
}

/**
 * The rule this record breaks, as the one thing on the page: for a law that holds two facts equal,
 * the two values side by side with ≠ between them (the record's own values, as the cell sent them);
 * otherwise the law's sentence. The finding is the cell's: nothing here evaluates the law.
 */
function Broken({ concept, reads, finding, heldBack }: { readonly concept: Concept; readonly reads: ObjectReads; readonly finding: Finding; readonly heldBack: boolean }) {
  const { view } = useView()
  const law = finding.ruleRef.kind === "law" ? concept.laws.find(({ id }) => id === (finding.ruleRef as { lawId: string }).lawId) : undefined
  const fieldOf = (id: string) => concept.fields.find((field) => field.id === id)
  const label = (field: Concept["fields"][number]) => field.label ?? field.id.replaceAll("_", " ")
  // The values the law read, as the finding records them (DIAG-REASON-1); a list is every line it summed.
  const read = finding.values
  const scale = law?.compares ? fieldOf(law.compares[0])?.decimal?.scale : undefined
  const fromRead = law?.compares && read && law.compares.every((path) => Object.hasOwn(read, path))
    ? law.compares.map((path) => {
      const field = fieldOf(path)
      const value = compared(read[path])
      return { key: path, label: field ? label(field) : isList(read[path]) ? `Sum of ${roleOf(path).replaceAll("_", " ")}` : path.replaceAll(".", " ").replaceAll("_", " "),
        text: atScale(value.text, field?.decimal?.scale ?? scale), parts: value.parts?.map((part) => atScale(part, scale)) }
    })
    : undefined
  const fields = law?.compares?.map(fieldOf)
  const pair = fromRead ?? (fields?.every((field) => field !== undefined)
    ? (fields as readonly Concept["fields"][number][]).map((field) => ({ key: field.id, label: label(field), text: factDisplay(field, reads.object.document, reads.object.numbers, "", view), parts: undefined }))
    : undefined)
  return (
    <section aria-label="The rule this record breaks" className="rounded-2xl bg-card p-6 shadow-[var(--elevation-1)] ring-1 ring-foreground/5 sm:px-7">
      {pair ? (
        <div className="grid items-center gap-4 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] sm:gap-6">
          {pair.map((side, index) => <Fragment key={side.key}>
            {index === 1 && <span aria-label="does not equal" className="flex size-11 items-center justify-center rounded-full bg-tone-red/12 text-xl text-tone-red">≠</span>}
            <div className="min-w-0">
              <div className="text-meta text-muted-foreground">{side.label}</div>
              <div className={cn("truncate pt-2 text-[2.75rem] leading-none font-light tracking-[-0.045em] tabular-nums sm:text-[3.25rem]", index === 0 && "text-tone-red")}>{side.text}</div>
              {side.parts && side.parts.length > 1 && <div className="truncate pt-1.5 text-meta text-muted-foreground tabular-nums" aria-label="The lines it summed">{side.parts.join(" + ")}</div>}
            </div>
          </Fragment>)}
        </div>
      ) : <p className="text-section font-medium text-pretty">{law?.rationale ?? finding.message}</p>}
      <div className="mt-5 flex flex-wrap items-start gap-x-3 gap-y-2 border-t border-border pt-4">
        <Chip tone="red" icon={ShieldX}>{heldBack ? "Kept out of answers" : "Rule broken"}</Chip>
        <p className="min-w-0 flex-1 text-pretty">{pair && law ? law.rationale : finding.message}{pair && <span className="block pt-1 text-meta text-muted-foreground">{finding.message}</span>}</p>
      </div>
    </section>
  )
}

/** One fact's exact text, opening its card; an empty fact is a dash with no card. */
function FactValue({ field, reads, onLineage }: { readonly field: Concept["fields"][number]; readonly reads: ObjectReads; readonly onLineage: () => void }) {
  const { view } = useView()
  const { objectType, id, version } = reads.object.document
  // One renderer for every value: grouped digits with their unit, dates and choices in words (Technical: as sent).
  const text = factDisplay(field, reads.object.document, reads.object.numbers, "", view)
  const value = reads.object.document.values[field.id]
  // A marked fact arrives projected for this reader: shown as the server projected it, never as the value.
  const masked = maskedOf(reads.object.document, field.id)
  // A reveal belongs to the record, property, version and projection it was revealed under (MaskedValue keys itself by them).
  if (masked !== null) return <MaskedValue objectType={objectType} id={id} version={version} property={field.id} masked={masked} projected={value} />
  if (value === null || value === undefined) return <span className="text-muted-foreground">{text}</span>
  const label = field.label ?? field.id.replaceAll("_", " ")
  return (
    <FactProvenance objectType={objectType} id={id} version={version} fact={field.id} label={label} value={text} lineage={reads.source?.document ?? null} live={reads.object.origin === "live"}
      trace={<button type="button" className="text-sm font-medium underline-offset-4 hover:underline" onClick={onLineage}>Open the record's lineage</button>}>
      {text}
    </FactProvenance>
  )
}

/**
 * The record's links by link type and direction, with the server's counts (exact, or its lower bound
 * past the count it reads), each group listing the linked records on the first page the server
 * returned. Records that link here open in full, fact by fact.
 */
function LinksRail({ concept, id, links, concepts, live }: { readonly concept: Concept; readonly id: string; readonly links: CountedLinkPage; readonly concepts: readonly Concept[]; readonly live: boolean }) {
  const declared = linkGroups(concept, incomingOf(concept, concepts), id, links)
  // A link type with nothing stored folds into one line under the ones that hold records.
  const groups = declared.filter((group) => group.ids.length > 0 || (group.count ?? 0) > 0)
  const empty = declared.length - groups.length
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set())
  const [table, setTable] = useState<LinkGroup | null>(null)
  const from = table ? concepts.find(({ name }) => name === table.otherType) : undefined
  const none = links.hasMore ? "None on the first page the cell returned" : "None stored"
  return (
    <Panel icon={Link2} label="Links" action={links.counts ? (links.counts.complete ? "Counted by the cell" : `Counted to ${links.counts.bound.toLocaleString("en")}`) : undefined}>
      <Well className="p-1.5">
        {declared.length === 0 || (groups.length === 0 && links.counts?.complete === true && !links.hasMore)
          ? <p className="p-2 text-sm text-muted-foreground">{declared.length > 0 || links.counts ? "No link touches this record." : "This concept declares no relationships."}</p>
          : <ul className="divide-y divide-border/70">
            {groups.map((group) => {
              const all = expanded.has(group.key)
              const shown = all ? group.ids : group.ids.slice(0, LINKS_SHOWN)
              return (
                <li key={group.key} className="space-y-1 px-2 py-2">
                  <div className="flex min-w-0 items-center gap-2 text-sm">
                    <span className="text-muted-foreground" aria-hidden>{group.direction === "out" ? "→" : "←"}</span>
                    <span className="min-w-0 flex-1 truncate font-medium">{group.label}</span>
                    {group.count !== null && <span className="shrink-0 rounded-md bg-muted px-1.5 text-meta tabular-nums text-muted-foreground" title={group.atLeast ? "At least this many: the cell stopped counting" : "Counted by the cell"}>{countText(group)}</span>}
                  </div>
                  {shown.length === 0
                    ? <p className="ps-5 text-meta text-muted-foreground">{none}</p>
                    : <ul className="space-y-0.5 ps-5 text-sm">{shown.map((other) => <li key={other} className="truncate"><Link to="/sources/$concept/$" params={{ concept: group.otherType, _splat: other }} className="underline-offset-4 hover:underline">{other}</Link></li>)}</ul>}
                  <div className="flex flex-wrap items-center gap-x-3 ps-5 text-meta text-muted-foreground">
                    {group.ids.length > LINKS_SHOWN && <button type="button" className="underline-offset-4 hover:underline" onClick={() => setExpanded((current) => { const next = new Set(current); if (all) next.delete(group.key); else next.add(group.key); return next })}>{all ? "Show fewer" : `Show all ${group.ids.length} on this page`}</button>}
                    {group.count !== null && group.ids.length < group.count && <span>{group.ids.length} of {countText(group)} on the first page</span>}
                    {live && group.direction === "in" && group.ids.length > 0 && <button type="button" className="underline-offset-4 hover:underline" onClick={() => setTable(group)}>See them fact by fact</button>}
                  </div>
                </li>
              )
            })}
            {empty > 0 && <li data-links-empty={empty} className="px-2 py-2 text-meta text-muted-foreground">{groups.length === 0 ? `${empty} link ${empty === 1 ? "type" : "types"}` : `${empty} more`}, {none.charAt(0).toLowerCase()}{none.slice(1)}</li>}
          </ul>}
      </Well>
      <Sheet open={table !== null} onOpenChange={(next) => { if (!next) setTable(null) }}>
        <SheetContent className="w-full overflow-y-auto sm:max-w-3xl">
          <SheetHeader><SheetTitle>{table?.label}</SheetTitle><SheetDescription>The records on the first page the cell returned, read one by one.</SheetDescription></SheetHeader>
          <div className="px-4 pb-6">{table && from && <LinkedRecords from={from} ids={table.ids} parent={id} />}</div>
        </SheetContent>
      </Sheet>
    </Panel>
  )
}

/** An empty tab: a picture, one sentence and one way forward. */
function Empty({ icon: Icon, action, children }: { readonly icon: LucideIcon; readonly action: ReactNode; readonly children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 py-10 text-center">
      <span className="flex size-12 items-center justify-center rounded-full bg-muted"><Icon aria-hidden className="size-6 text-muted-foreground" /></span>
      <p className="max-w-sm text-sm text-muted-foreground">{children}</p>
      {action}
    </div>
  )
}

/** Linked records a group lists before "Show all". */
const LINKS_SHOWN = 5

function Lineage({ lineage, evidence = [], actionFor }: { readonly lineage: SourceLineage | undefined; readonly evidence?: readonly EvidenceRef[]; readonly actionFor?: (evidence: Evidence) => ReactNode }) {
  const { inspect } = useInspector()
  useEffect(() => () => inspect(), [lineage, inspect])
  return (
    <Panel icon={Database} label="Where it came from"><Well className="space-y-3 p-3">
      {!lineage && <EmptyState icon={Database}>No source record is retained for this record.</EmptyState>}
      {lineage && <>
        <div>
          <div className="text-panel font-medium">{systemName(lineage.sourceRecordRef.system)}</div>
          <p className="text-meta text-muted-foreground">Source record <code>{lineage.documentRef.record}</code>, mapped by <code>{lineage.mappingPin.artifactId}</code> version {lineage.mappingPin.version}</p>
        </div>
        <DenseTable head={<TableRow><TableHead>Fact</TableHead><TableHead>Read from</TableHead><TableHead>Rule</TableHead><TableHead>Recorded</TableHead></TableRow>}>
          {Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => (
            <TableRow key={fact}>
              <TableCell className="font-medium"><button type="button" className="underline decoration-dotted underline-offset-4" onClick={() => inspect({ title: fact.replaceAll("_", " "), body: <ProvenanceChain lineage={lineage} fact={fact} /> })}>{fact.replaceAll("_", " ")}</button></TableCell>
              <TableCell><code className="text-xs">{(provenance.sourceFields ?? []).join(", ") || "—"}</code></TableCell>
              <TableCell><code className="text-xs">{provenance.transform}</code></TableCell>
              <TableCell>{provenance.recordedAt}</TableCell>
            </TableRow>
          ))}
        </DenseTable>
        {evidence.length > 0 && Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => {
          const fields = provenance.sourceFields ?? []
          return (
            <div key={`evidence-${fact}`} className="space-y-2 pt-2">
              <div className="text-sm font-medium">{fact.replaceAll("_", " ")}, as read from the scan</div>
              <EvidenceList refs={evidence} {...actionFor ? { actionFor } : {}} filter={(item) => fields.includes(item.subjectProperty) || fields.some((field) => item.subjectProperty.startsWith(`${field}.`))} />
            </div>
          )
        })}
        <p className="text-meta text-muted-foreground">The source record's own field values are not served yet.{lineage.documentDigest ? <> The cell keeps its digest, <CopyId value={lineage.documentDigest} short={16} />, so a copy of the row can be checked against it.</> : null}</p>
      </>}
    </Well></Panel>
  )
}

/**
 * The record's facts in words, each with the source system the cell says it was read from (its
 * field provenance) and, for a marked fact, the server's projection and a tag saying it is hidden from
 * this reader's role. In the technical view, the exact document the server sent.
 */
function Facts({ concept, reads, onLineage, sources }: {
  readonly concept: Concept
  readonly reads: ObjectReads
  readonly onLineage: () => void
  /** The reader's Sources decision on this type, as the cell made it; undefined when not read. */
  readonly sources: "permit" | "deny" | undefined
}) {
  const { view } = useView()
  const { catalog } = useCatalog()
  const markings = useMarkingCatalog()
  // Scan provenance (the bytes a record was read from and the extractor) is not what the record
  // says: the evidence beside each value and the Technical view carry it.
  const provenance = new Set(concept.parents.includes(SOURCE_DOCUMENT) ? catalog?.byName.get(SOURCE_DOCUMENT)?.fields.map((field) => field.id) ?? [] : [])
  if (view === "technical") {
    return <Panel icon={Braces} label="Record document"><Well className="p-3"><Code code={exactPretty(reads.object.document, reads.object.numbers, "")} /></Well></Panel>
  }
  const lineage = reads.source?.document
  const systemOf = (field: string): string | undefined => lineage?.fieldProvenance[field]?.source.system
  const fields = concept.fields.filter((field) => !provenance.has(field.id))
  const systems = [...new Set(fields.flatMap((field) => systemOf(field.id) ?? []))]
  const unsourced = lineage === undefined
    ? sources === "deny" ? "Your role does not see where these values came from." : "No source record is kept for this version: it was written in Norn, or your role may not see its source."
    : null
  return (
    <Panel icon={Rows3} label="Facts" action={systems.length > 0 ? <span className="flex flex-wrap items-center gap-1.5" aria-label="Read from">{systems.map((system) => <SourceChip key={system} system={system} />)}</span> : undefined}>
      <Well className="px-1.5 pb-1.5">
        <dl className="divide-y divide-border/60 text-sm" aria-label="Facts">
          {fields.map((field) => {
            const masked = maskedOf(reads.object.document, field.id)
            const system = systemOf(field.id)
            return (
              <div key={field.id} data-fact={field.id} className="grid min-h-row-read grid-cols-[minmax(0,11rem)_minmax(0,1fr)_auto] items-center gap-3 px-3">
                <dt className="flex min-w-0 items-center gap-1.5 text-muted-foreground" title={field.id}><span className="truncate">{field.label ?? field.id.replaceAll("_", " ")}</span><MarkingChips markings={field.markings} catalog={markings} /></dt>
                <dd className={cn("flex min-w-0 items-center gap-2", field.decimal && "tabular-nums")}>
                  <span className="min-w-0 truncate font-medium"><FactValue field={field} reads={reads} onLineage={onLineage} /></span>
                  {masked !== null && <HiddenTag revealable={masked.revealable} />}
                </dd>
                <dd className="flex justify-end">{system !== undefined && <SourceChip system={system} />}</dd>
              </div>
            )
          })}
        </dl>
        {unsourced !== null && <p className="px-3 pt-2 pb-1 text-meta text-muted-foreground">{unsourced}</p>}
      </Well>
    </Panel>
  )
}

/** A marked fact's tag: hidden from this reader's role, or masked with a reveal the server allows. */
function HiddenTag({ revealable }: { readonly revealable: boolean }) {
  return (
    <span data-hidden-tag="" className="inline-flex h-6 shrink-0 items-center gap-1.5 rounded-md bg-foreground/[0.06] px-2 text-xs font-medium text-muted-foreground"
      title={revealable ? "Masked for everyone; your role may reveal it, and the cell records a receipt" : "The server never sent this value to your role"}>
      <EyeOff aria-hidden className="size-3.5" />{revealable ? "Masked · you may reveal" : "Hidden from your role"}
    </span>
  )
}

/** A labelled id row. */
const Id = ({ label, value, short }: { readonly label: string; readonly value: string | null | undefined; readonly short?: number }) => (
  <div className="flex min-w-0 flex-col gap-x-3 sm:flex-row sm:items-baseline"><dt className="shrink-0 text-meta text-muted-foreground sm:w-32">{label}</dt><dd className="min-w-0 flex-1"><DigestId value={value} short={short} /></dd></div>
)

/** Only Norn artefacts: the typed definition and its laws, the mapping, the ids that pin them, and what a recorded snapshot cannot hold. */
function Technical({ name, lineage }: { readonly name: string; readonly lineage: SourceLineage | undefined }) {
  const { read } = useCatalog()
  const artifacts = read.data?.document ?? []
  const index = artifacts.findIndex((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === name)
  const entity = artifacts[index]
  const laws = artifacts.filter((artifact) => artifact.kind === "law" && artifact.payload.scope.objectTypes.includes(name))
  return <>
    {entity && "objectType" in entity.payload && (
      <Panel icon={KINDS.objectType.icon} label="Definition"><Well className="space-y-3 p-3">
        <dl className="space-y-1">
          <Id label="Artefact" value={`${entity.artifactId}@${entity.version}`} />
          {laws.map((law) => <Id key={law.artifactId} label="Law" value={`${law.artifactId}@${law.version}`} />)}
        </dl>
        <Code code={exactPretty(entity.payload.objectType, read.data?.numbers ?? new Map(), `/${index}/payload/objectType`)} />
      </Well></Panel>
    )}
    {lineage && (
      <Panel icon={KINDS.mapping.icon} label="Mapping and lineage"><Well className="space-y-3 p-3">
        <dl className="space-y-1">
          <Id label="Mapping" value={`${lineage.mappingPin.artifactId}@${lineage.mappingPin.version}`} />
          <Id label="Mapping sha256" value={lineage.mappingPin.sha256} short={16} />
          <Id label="Source record" value={lineage.sourceRecordRef.record} />
          <Id label="Bundle" value={lineage.bundleDigest} short={16} />
          <Id label="Document" value={lineage.documentDigest} short={16} />
        </dl>
        <DenseTable head={<TableRow><TableHead>Property</TableHead><TableHead>Rule</TableHead><TableHead>Source fields</TableHead></TableRow>}>
          {Object.entries(lineage.fieldProvenance).map(([fact, provenance]) => (
            <TableRow key={fact}><TableCell className="font-mono text-xs">{fact}</TableCell><TableCell className="font-mono text-xs">{provenance.transform}</TableCell><TableCell className="font-mono text-xs">{(provenance.sourceFields ?? []).join(", ")}</TableCell></TableRow>
          ))}
        </DenseTable>
      </Well></Panel>
    )}
    <DecisionAndReceipt />
  </>
}

/** Other object types' declared relations that point at this one: `Holding.item → Item` is "held by" from the item's side. */
const incomingOf = (concept: Concept, concepts: readonly Concept[]) =>
  concepts.flatMap((other) => other.relationships.filter(({ target }) => target === concept.name).map((relationship) => ({ from: other, relationship })))

/** One plain sentence from what the server sent: the record, its type, where it was read from and what its stored links say. */
export function summaryOf(concept: Concept, reads: ObjectReads, concepts: readonly Concept[]): string {
  const source = reads.source?.document
  const { id, objectType } = reads.object.document
  const counts = reads.links.document.counts
  // The server's count when it sends one; else the first page's links, a floor when more may exist.
  const stored = counts ? counts.groups.reduce((sum, { count }) => sum + count, 0)
    : reads.links.document.items.filter((edge) => (edge.source === id && edge.sourceType === objectType) || (edge.target === id && edge.targetType === objectType)).length
  const declared = concept.relationships.length + incomingOf(concept, concepts).length
  const from = source ? `, read from ${systemName(source.sourceRecordRef.system)} by mapping ${source.mappingPin.artifactId}` : ""
  const more = counts ? !counts.complete : reads.links.document.hasMore
  const links = declared === 0 ? "Its type declares no relations."
    : stored === 0 ? (more ? "No stored link appears on the first page the server returned; more may exist." : declared === 1 ? "Its one declared relation has no stored link in this snapshot." : `None of the ${declared} relations its type declares has a stored link in this snapshot.`)
    : `${more ? "At least " : ""}${stored} stored ${stored === 1 ? "link connects" : "links connect"} it to other records.`
  return `${recordName(reads.object.document)} is ${/^[aeiou]/i.test(concept.title) ? "an" : "a"} ${concept.title.toLowerCase()} record${from}. ${links}`
}

const CAPABILITIES = [["population", "Population: which records exist"], ["values", "Values: their amounts and facts"], ["sources", "Sources: where they came from"]] as const

/**
 * The host's access decision for the signed-in account, read live (`/v1/access`) and shown as the
 * server wrote it: its principal and, per capability, permit or deny with the rule. Nothing is decided here.
 */
function DecisionAndReceipt() {
  const viewer = useViewer()
  const access = useQuery({ queryKey: ["live-access"], queryFn: () => liveAccess(), retry: false, staleTime: 60_000 })
  const row = (label: string, value: ReactNode) => <div className="flex flex-col gap-x-3 sm:flex-row"><dt className="shrink-0 text-meta text-muted-foreground sm:w-32">{label}</dt><dd className="min-w-0">{value}</dd></div>
  const unknown = (text: string) => <span className="flex items-center gap-2"><span className={cn("size-2 shrink-0 rounded-full", STATUS.unknown.dot)} />{text}</span>
  const data = access.data
  return (
    <Panel icon={KINDS.policy.icon} label="Decision and receipt"><Well className="space-y-3 p-3">
      {data?.kind === "read"
        ? <p className="text-sm">The access decision is read live from the cell for your account.</p>
        : <p className="text-sm">This page shows a recorded snapshot. Once live reads are available, they will return the access decision and receipt for your account.</p>}
      {data?.kind === "refused" && <RefusedNote refusal={data.refusal} />}
      <dl className="space-y-1 text-sm">
        {data?.kind === "read"
          ? <>
            {row("Principal", <span><CopyId value={data.read.document.principal.subjectId} /> <span className="text-muted-foreground">· {data.read.document.principal.roles.join(", ")} ({data.read.document.principal.roleTable})</span></span>)}
            {row("Policy", <span className="font-mono text-xs">{data.read.document.policy} · {data.read.document.grantTable}</span>)}
            {CAPABILITIES.map(([capability, label]) => {
              const { decision, rule } = data.read.document.capabilities[capability]
              return <div key={capability}>{row(label, <span className="flex flex-wrap items-center gap-2"><span className={cn("size-2 shrink-0 rounded-full", decision === "permit" ? STATUS.held.dot : STATUS.refused.dot)} />{decision === "permit" ? "Permit" : "Deny"}<span className="font-mono text-xs text-muted-foreground">{rule ?? "no rule grants it"}</span></span>)}</div>
            })}
          </>
          : <>
            {row("Viewing as", <>{viewer.name}{viewer.role !== null && <span className="text-muted-foreground"> · {viewer.role}</span>}</>)}
            {data?.kind !== "refused" && row("Access decision", unknown("Not in the recorded snapshot"))}
          </>}
        {row("Receipt", unknown("Not in the recorded snapshot"))}
      </dl>
    </Well></Panel>
  )
}
