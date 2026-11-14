import { Chip } from "@/components/chip"
import { useView } from "@/lib/view"
import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { FileSearch, PencilLine, ScanText } from "lucide-react"
import { useState, type ReactNode } from "react"
import { CopyId } from "@/components/copy-id"
import { cn } from "@/lib/utils"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { atScale } from "../lib/objects.ts"
import { transport, type Artifact } from "@/lib/transport"
import { readEvidence, readHistory, readScan, type Evidence, type EvidenceRef } from "./review.ts"

/** A fraction of the page as a CSS percentage. */
const percent = (fraction: string) => `${Number(fraction) * 100}%`

/** Why the page itself cannot be shown here, in the cell's words. */
function pageProblem(error: unknown): string {
  if (error instanceof NotServed) return "This cell does not serve scanned pages yet."
  if (error instanceof CellRefusal) return `The cell refused the page: ${error.message}`
  return "The scanned page could not be read."
}

/** The declared decimal scale of a property of an object type, from the cell's catalog. */
function scaleOf(artifacts: readonly Artifact[], objectType: string, property: string): number | undefined {
  for (const artifact of artifacts) {
    if (artifact.kind !== "entity" || !("objectType" in artifact.payload) || artifact.payload.objectType.name !== objectType) continue
    const type = (artifact.payload.objectType.properties ?? []).find((candidate) => candidate.id === property)?.type as { readonly kind?: string; readonly scale?: number } | undefined
    return type?.kind === "decimal" ? type.scale : undefined
  }
  return undefined
}

/**
 * The governed change to the value this evidence was read for: the newest version of the value's
 * subject that an action wrote and that changed that property. It shows what the value became, who
 * changed it (the verified actor), why, and in which commit, whichever action did it (a line's
 * correction restates its document's total too).
 */
function Correction({ evidence, subject }: { readonly evidence: Evidence; readonly subject: EvidenceRef }) {
  const history = useQuery({ queryKey: ["history", subject.objectType, subject.id], queryFn: () => readHistory(subject.objectType, subject.id), retry: false })
  const catalog = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const versions = [...history.data?.document.versions ?? []].sort((a, b) => b.version - a.version)
  const property = evidence.subjectProperty
  const index = versions.findIndex((version, at) => version.writer.kind === "action" && version.values !== null
    && JSON.stringify(version.values[property]) !== JSON.stringify(versions[at + 1]?.values?.[property]))
  const correction = versions[index]
  if (correction === undefined || correction.writer.kind !== "action") return null
  const value = correction.values?.[property]
  if (value === undefined || value === null) return null
  const scale = scaleOf(catalog.data?.document ?? [], subject.objectType, property)
  const reason = correction.writer.input.correction_reason
  return (
    <div className="flex flex-wrap items-baseline gap-x-1.5 rounded-sm bg-emerald-500/10 px-1.5 py-0.5 text-foreground">
      <PencilLine aria-hidden className="size-3.5 self-center text-emerald-700 dark:text-emerald-400" />
      <span>Read <span className="font-mono">“{evidence.text}”</span> → corrected to <span className="font-mono font-medium">{atScale(String(value), scale)}</span></span>
      <span className="text-muted-foreground">by {correction.writer.actor}{typeof reason === "string" && reason !== "" ? <> · “{reason}”</> : null} ·</span>
      <Link to="/sources/$concept/$" params={{ concept: subject.objectType, _splat: subject.id }} hash="lineage" className="underline underline-offset-4" title="The correction's receipt and lineage">commit {correction.commit}</Link>
    </div>
  )
}

/**
 * The region of the scanned page an extracted value was read from, beside the
 * text the extractor read there. The page comes from a short-lived read the
 * cell issues after its policy decides; the box is the evidence's own. A
 * governed correction that cited this evidence is shown with it.
 */
export function EvidenceCrop({ evidence, record, className, actionFor }: { readonly evidence: Evidence; readonly record?: EvidenceRef; readonly className?: string; readonly actionFor?: (evidence: Evidence) => ReactNode }) {
  const subject = evidence.subject ?? record
  // An object-store URI is read only through the cell's short-lived, policy-checked read; a plain
  // web URI the evidence names (a local `norn dev` serving synthetic scans) is shown as it is.
  const direct = /^https?:\/\//.test(evidence.documentUri)
  const scan = useQuery({
    queryKey: ["scan", evidence.documentUri],
    queryFn: () => direct
      ? Promise.resolve({ document: { getUrl: evidence.documentUri, expiresAt: "" }, numbers: new Map<string, string>(), origin: "live" as const })
      : readScan(evidence.documentUri),
    retry: false,
    staleTime: 4 * 60_000,
  })
  const { view } = useView()
  const [aspect, setAspect] = useState<number | null>(null)
  const [whole, setWhole] = useState(false)
  const { x, y, w, h } = evidence.box
  const confidence = evidence.confidenceBp === null ? "no confidence reported" : `confidence ${(evidence.confidenceBp / 100).toFixed(1)}%`
  // The crop window keeps the box's own proportions on the page.
  const cropAspect = aspect === null ? 4 : (Number(w) * aspect) / Number(h)
  return (
    <figure className={cn("space-y-2 rounded-lg border border-border bg-card p-3", className)}>
      {/* The scanned page shows only when the cell serves it; otherwise the read value stands alone. */}
      {(scan.data || scan.isPending) && <div className="overflow-hidden rounded-md border bg-white">
        {scan.data && !whole && (
          <div className="relative w-full" style={{ aspectRatio: String(cropAspect) }}>
            <img
              src={scan.data.document.getUrl}
              alt={`Page ${evidence.page} of the scanned document, at the region read as “${evidence.text}”`}
              onLoad={(event) => setAspect(event.currentTarget.naturalWidth / event.currentTarget.naturalHeight)}
              className="absolute max-w-none"
              style={{ width: `${100 / Number(w)}%`, left: `-${Number(x) / Number(w) * 100}%`, top: `-${Number(y) / Number(h) * 100}%` }}
            />
          </div>
        )}
        {scan.data && whole && (
          <div className="relative">
            <img src={scan.data.document.getUrl} alt={`Page ${evidence.page} of the scanned document`} className="w-full" />
            <div aria-hidden className="absolute rounded-sm ring-2 ring-amber-500" style={{ left: percent(x), top: percent(y), width: percent(w), height: percent(h) }} />
          </div>
        )}
        {scan.isPending && <div className="flex h-16 items-center justify-center text-meta text-muted-foreground">Reading the scanned page…</div>}
      </div>}
      <figcaption className="space-y-2 text-meta text-muted-foreground">
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
          <span className="sr-only">Read as</span>
          <span className="font-mono text-section font-semibold text-foreground tabular-nums">{evidence.text}</span>
          <Chip tone="blue" icon={ScanText}>{evidence.subjectProperty.replaceAll("_", " ")}</Chip>
          {scan.data && <button type="button" className="ms-auto underline underline-offset-4" onClick={() => setWhole((value) => !value)}>{whole ? "Show the region" : "Show the whole page"}</button>}
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Chip tone={evidence.confidenceBp === null ? "gray" : evidence.confidenceBp >= 9000 ? "green" : "amber"} dot title="The extractor's own confidence in this read">{confidence}</Chip>
          <Chip tone="gray">page {evidence.page}</Chip>
          <Chip tone="gray" title="The extractor that read it">{evidence.extractor} {evidence.extractorVersion}</Chip>
          {scan.isError && <span className="inline-flex items-center gap-1" title={pageProblem(scan.error)}><FileSearch aria-hidden className="size-3.5" />no page image</span>}
        </div>
        {view === "technical" && <div className="flex flex-wrap items-center gap-x-1">Document <CopyId value={evidence.documentDigest} short={12} /> <span className="truncate font-mono">{evidence.documentUri}</span></div>}
        {subject && <Correction evidence={evidence} subject={subject} />}
        {actionFor?.(evidence)}
      </figcaption>
    </figure>
  )
}

/** A value's place in its document, in words: "line 1 · amount", or the document's own "total". */
const fieldOf = (evidence: Evidence, record?: EvidenceRef) => {
  const subject = (evidence.subject ?? record)?.id ?? ""
  const part = subject.includes("~") ? subject.slice(subject.lastIndexOf("~") + 1).replace(/-/g, " ") : ""
  const property = evidence.subjectProperty.replaceAll("_", " ")
  return part ? `${part} · ${property}` : property
}

/** One read value as a dense row: where it is, what was read, how sure the reader was, and the governed correction. */
function EvidenceRow({ evidence, record, actionFor }: { readonly evidence: Evidence; readonly record?: EvidenceRef; readonly actionFor?: (evidence: Evidence) => ReactNode }) {
  const { view } = useView()
  const subject = evidence.subject ?? record
  const confidence = evidence.confidenceBp === null ? "—" : `${(evidence.confidenceBp / 100).toFixed(1)}%`
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto_auto_2rem] items-center gap-x-3 px-1 py-1.5 text-meta tabular-nums">
      <span className="min-w-0 truncate text-muted-foreground">{fieldOf(evidence, record)}</span>
      <span className="font-mono text-sm text-foreground" title={`Read as “${evidence.text}”, page ${evidence.page}`}><span className="sr-only">Read as </span>{evidence.text}</span>
      <Chip tone={evidence.confidenceBp === null ? "gray" : evidence.confidenceBp >= 9000 ? "green" : "amber"} title={`The extractor's confidence: ${evidence.extractor} ${evidence.extractorVersion}, page ${evidence.page}`}>{confidence}</Chip>
      <span className="flex justify-end">{actionFor?.(evidence)}</span>
      {subject && <div className="col-span-4 empty:hidden"><Correction evidence={evidence} subject={subject} /></div>}
      {view === "technical" && <div className="col-span-4 flex min-w-0 items-center gap-1 text-muted-foreground">Document <CopyId value={evidence.documentDigest} short={12} /> <span className="truncate font-mono">{evidence.documentUri}</span></div>}
    </li>
  )
}

/**
 * The evidence objects of one value. With the scanned pages served, each shows its crop; without
 * them, the reads are dense rows under one quiet note (never a placeholder per value).
 */
export function EvidenceList({ refs, record, filter, actionFor }: { readonly refs: readonly EvidenceRef[]; readonly record?: EvidenceRef; readonly filter?: (evidence: Evidence) => boolean; readonly actionFor?: (evidence: Evidence) => ReactNode }) {
  const evidence = useQuery({ queryKey: ["evidence", refs.map((ref) => `${ref.objectType}/${ref.id}`)], queryFn: () => Promise.all(refs.map(readEvidence)), enabled: refs.length > 0 })
  // The first page decides: a cell that serves scanned pages serves them all.
  const first = evidence.data?.find((item) => !/^https?:\/\//.test(item.documentUri))
  const page = useQuery({ queryKey: ["scan", first?.documentUri], queryFn: () => readScan(first!.documentUri), retry: false, staleTime: 4 * 60_000, enabled: first !== undefined })
  if (refs.length === 0) return null
  if (evidence.isPending) return <p className="text-meta text-muted-foreground">Reading the evidence…</p>
  if (evidence.isError) return <p className="text-meta text-muted-foreground">The evidence could not be read.</p>
  const shown = filter ? evidence.data.filter(filter) : evidence.data
  if (shown.length === 0) return <p className="text-meta text-muted-foreground">No evidence was recorded for this value.</p>
  if (first !== undefined && page.isError) return (
    <div className="space-y-1">
      <ul className="divide-y divide-border/70">{shown.map((item) => <EvidenceRow key={`${item.ref.objectType}/${item.ref.id}`} evidence={item} {...record ? { record } : {}} {...actionFor ? { actionFor } : {}} />)}</ul>
      <p className="flex items-center gap-1.5 px-1 text-xs text-muted-foreground"><FileSearch aria-hidden className="size-3.5" />{pageProblem(page.error)}</p>
    </div>
  )
  return <div className="grid gap-3 sm:grid-cols-2">{shown.map((item) => <EvidenceCrop key={`${item.ref.objectType}/${item.ref.id}`} evidence={item} {...record ? { record } : {}} {...actionFor ? { actionFor } : {}} />)}</div>
}
