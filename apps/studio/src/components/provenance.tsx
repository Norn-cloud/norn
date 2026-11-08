import { ArrowDown, GitCommitHorizontal } from "lucide-react"
import type { ReactNode } from "react"
import { CopyId } from "@/components/copy-id"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { systemName } from "@/lib/catalog"
import { KINDS, type Kind } from "@/lib/kinds"
import type { SourceLineage } from "@/lib/transport"
import { cn } from "@/lib/utils"

/** One link in the chain, in its kind's colour; the recorded step has no kind and stays neutral. */
function Step({ kind, title, children }: { readonly kind?: Kind; readonly title: string; readonly children: ReactNode }) {
  const { icon: Icon, text, fill } = kind ? KINDS[kind] : { icon: GitCommitHorizontal, text: "text-muted-foreground", fill: "bg-muted" }
  return (
    <li className="flex gap-3">
      <span className={cn("flex size-7 shrink-0 items-center justify-center rounded-md", fill, text)}><Icon className="size-4" /></span>
      <div className="min-w-0 space-y-0.5 text-sm"><div className="font-medium">{title}</div>{children}</div>
    </li>
  )
}

const Arrow = () => <li aria-hidden className="ms-2 text-muted-foreground"><ArrowDown className="size-3" /></li>

/**
 * Where one value came from, as the server recorded it: the source record and
 * fields, the mapping rule that produced it, and the digests that pin both.
 */
export function ProvenanceChain({ lineage, fact }: { readonly lineage: SourceLineage; readonly fact: string }) {
  const field = lineage.fieldProvenance[fact]
  if (field === undefined) return <p className="text-sm text-muted-foreground">The server recorded no source for this value.</p>
  return (
    <ol className="space-y-2">
      <Step kind="source" title={`Source row · ${systemName(field.source.system)}`}>
        <CopyId value={field.source.record} />
        <div className="text-xs text-muted-foreground">Fields <span className="font-mono">{(field.sourceFields ?? []).join(", ") || "—"}</span></div>
      </Step>
      <Arrow />
      <Step kind="mapping" title="Mapping">
        <CopyId value={field.mappingVersion} />
        <div className="text-xs text-muted-foreground">Rule <span className="font-mono">{field.transform}</span></div>
        <CopyId value={lineage.mappingPin.sha256} short={12} className="text-muted-foreground" />
      </Step>
      <Arrow />
      <Step title={`Recorded · ${field.recordedAt}`}>
        <div className="text-xs text-muted-foreground">Bundle <CopyId value={lineage.bundleDigest} short={12} /></div>
        <div className="text-xs text-muted-foreground">Document <CopyId value={lineage.documentDigest} short={12} /></div>
      </Step>
    </ol>
  )
}

/** A value that opens its provenance; plain text when the server recorded none. */
export function ProvenanceValue({ lineage, fact, children }: { readonly lineage: SourceLineage | undefined; readonly fact: string; readonly children: ReactNode }) {
  if (lineage?.fieldProvenance[fact] === undefined) return <>{children}</>
  return (
    <Popover>
      <PopoverTrigger className="rounded-sm text-start underline decoration-dotted decoration-muted-foreground/60 underline-offset-4 outline-none hover:decoration-foreground focus-visible:ring-2 focus-visible:ring-ring/40">{children}</PopoverTrigger>
      <PopoverContent align="start" className="w-80">
        <div className="pb-3 text-xs text-muted-foreground">Where this value came from</div>
        <ProvenanceChain lineage={lineage} fact={fact} />
      </PopoverContent>
    </Popover>
  )
}
