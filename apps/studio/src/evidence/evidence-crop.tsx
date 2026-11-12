import { useQuery } from "@tanstack/react-query"
import { FileSearch, ScanText } from "lucide-react"
import { useState } from "react"
import { CopyId } from "@/components/copy-id"
import { cn } from "@/lib/utils"
import { CellRefusal, NotServed } from "../lib/live-transport.ts"
import { readEvidence, readScan, type Evidence, type EvidenceRef } from "./review.ts"

/** A fraction of the page as a CSS percentage. */
const percent = (fraction: string) => `${Number(fraction) * 100}%`

/** Why the page itself cannot be shown here, in the cell's words. */
function pageProblem(error: unknown): string {
  if (error instanceof NotServed) return "This cell does not serve scanned pages yet."
  if (error instanceof CellRefusal) return `The cell refused the page: ${error.message}`
  return "The scanned page could not be read."
}

/**
 * The region of the scanned page an extracted value was read from, beside the
 * text the extractor read there. The page comes from a short-lived read the
 * cell issues after its policy decides; the box is the evidence's own.
 */
export function EvidenceCrop({ evidence, className }: { readonly evidence: Evidence; readonly className?: string }) {
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
  const [aspect, setAspect] = useState<number | null>(null)
  const [whole, setWhole] = useState(false)
  const { x, y, w, h } = evidence.box
  const confidence = evidence.confidenceBp === null ? "no confidence reported" : `confidence ${(evidence.confidenceBp / 100).toFixed(1)}%`
  // The crop window keeps the box's own proportions on the page.
  const cropAspect = aspect === null ? 4 : (Number(w) * aspect) / Number(h)
  return (
    <figure className={cn("space-y-2", className)}>
      <div className="overflow-hidden rounded-md border bg-white">
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
        {scan.isError && <div className="flex items-center gap-2 p-3 text-meta text-muted-foreground"><FileSearch aria-hidden className="size-4" />{pageProblem(scan.error)}</div>}
      </div>
      <figcaption className="space-y-1 text-meta text-muted-foreground">
        <div className="flex flex-wrap items-center gap-x-2">
          <ScanText aria-hidden className="size-3.5" />
          <span>Read as <span className="font-mono text-foreground">“{evidence.text}”</span> for <code>{evidence.subjectProperty}</code></span>
          {scan.data && <button type="button" className="underline underline-offset-4" onClick={() => setWhole((value) => !value)}>{whole ? "Show the region" : "Show the whole page"}</button>}
        </div>
        <div>{evidence.extractor} {evidence.extractorVersion} · page {evidence.page} · {confidence}</div>
        <div className="flex flex-wrap items-center gap-x-1">Document <CopyId value={evidence.documentDigest} short={12} /> <span className="truncate font-mono">{evidence.documentUri}</span></div>
      </figcaption>
    </figure>
  )
}

/** The evidence objects of one value, each with its crop. */
export function EvidenceList({ refs, filter }: { readonly refs: readonly EvidenceRef[]; readonly filter?: (evidence: Evidence) => boolean }) {
  const evidence = useQuery({ queryKey: ["evidence", refs.map((ref) => `${ref.objectType}/${ref.id}`)], queryFn: () => Promise.all(refs.map(readEvidence)), enabled: refs.length > 0 })
  if (refs.length === 0) return null
  if (evidence.isPending) return <p className="text-meta text-muted-foreground">Reading the evidence…</p>
  if (evidence.isError) return <p className="text-meta text-muted-foreground">The evidence could not be read.</p>
  const shown = filter ? evidence.data.filter(filter) : evidence.data
  if (shown.length === 0) return <p className="text-meta text-muted-foreground">No evidence was recorded for this value.</p>
  return <div className="grid gap-3 sm:grid-cols-2">{shown.map((item) => <EvidenceCrop key={`${item.ref.objectType}/${item.ref.id}`} evidence={item} />)}</div>
}
