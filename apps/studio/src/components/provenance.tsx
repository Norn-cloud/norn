import { useQuery } from "@tanstack/react-query"
import { ArrowDown, Clock, GitCommitHorizontal, PencilLine, Waypoints, type LucideIcon } from "lucide-react"
import { useState, type ReactNode, type SyntheticEvent } from "react"
import { CopyId, DigestId } from "@/components/copy-id"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { systemName } from "@/lib/catalog"
import { KINDS, type Kind } from "@/lib/kinds"
import { CellRefusal, NotServed } from "@/lib/live-transport"
import { factProvenance, versionShown, type ValueProvenance } from "@/lib/provenance"
import type { SourceLineage } from "@/lib/transport"
import { cn } from "@/lib/utils"
import { readHistory } from "../evidence/review.ts"

/** One link in the chain, in its kind's icon; receipt and time steps have no kind and use their own. */
function Step({ kind, icon, title, children }: { readonly kind?: Kind; readonly icon?: LucideIcon; readonly title: ReactNode; readonly children?: ReactNode }) {
  const Icon = icon ?? (kind ? KINDS[kind].icon : GitCommitHorizontal)
  return (
    <li className="flex gap-3">
      <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"><Icon aria-hidden className="size-4" /></span>
      <div className="min-w-0 flex-1 space-y-0.5 text-sm"><div className="font-medium [overflow-wrap:anywhere]">{title}</div>{children}</div>
    </li>
  )
}

const Arrow = () => <li aria-hidden className="ms-2 text-muted-foreground"><ArrowDown className="size-3" /></li>
const Line = ({ children }: { readonly children: ReactNode }) => <div className="text-xs text-muted-foreground [overflow-wrap:anywhere]">{children}</div>

/** The chain for one value: source, transform, receipt and times, each step only when the server sent it. */
export function ProvenanceSteps({ provenance }: { readonly provenance: ValueProvenance }) {
  const { sources, transform, receipt, digests, asOf } = provenance
  const steps: ReactNode[] = []
  if (sources.length === 1 && sources[0]?.record !== null) {
    const [source] = sources
    if (source) steps.push(
      <Step key="source" kind="source" title={`Source · ${systemName(source.system)}`}>
        {source.record !== null && <CopyId value={source.record} />}
        <Line>Fields <span className="font-mono">{source.fields.join(", ") || "—"}</span></Line>
      </Step>,
    )
  } else if (sources.length > 0) {
    steps.push(
      <Step key="source" kind="source" title={sources.length === 1 ? "Source" : `Sources · ${sources.length}`}>
        {sources.map((source) => <Line key={source.system}><span className="font-medium text-foreground">{systemName(source.system)}</span>{source.complete === undefined ? "" : source.complete ? " · complete" : " · incomplete"}</Line>)}
      </Step>,
    )
  }
  if (transform?.kind === "mapping") steps.push(
    <Step key="transform" kind="mapping" title={`Mapping · ${transform.artifactId}`}>
      <Line>Version <span className="font-mono">{transform.version.startsWith(`${transform.artifactId}@`) ? transform.version.slice(transform.artifactId.length + 1) : transform.version}</span>{transform.rule !== null && <> · rule <span className="font-mono">{transform.rule}</span></>}</Line>
      <CopyId value={transform.sha256} short={12} className="text-muted-foreground" />
      {transform.upstream > 0 && <Line>Reads {transform.upstream} upstream {transform.upstream === 1 ? "fact" : "facts"}</Line>}
    </Step>,
  )
  else if (transform?.kind === "action") steps.push(
    <Step key="transform" icon={PencilLine} title={<>Action · <span className="font-mono text-xs">{transform.actionId}</span></>}>
      <Line>Committed by <span className="font-medium text-foreground">{transform.actor}</span></Line>
    </Step>,
  )
  else if (transform?.kind === "question") steps.push(
    <Step key="transform" kind="question" title={transform.metrics.join(", ")}>
      <Line>{transform.records} {transform.records === 1 ? "record" : "records"} added up exactly</Line>
      {transform.engine !== null && <Line>{transform.engine}</Line>}
    </Step>,
  )
  else if (transform?.kind === "other") steps.push(<Step key="transform" kind="transform" title="Written by a rule, an import or a migration" />)
  if (receipt || digests.length > 0) steps.push(
    <Step key="receipt" title={receipt === null ? "Pinned by" : receipt.kind === "commit" ? `Receipt · commit ${receipt.commit}` : `Ledger head · commit ${receipt.seq}`}>
      {receipt?.kind === "head" && <CopyId value={receipt.hash} short={12} className="text-muted-foreground" />}
      {digests.map(({ label, value }) => <Line key={label}>{label} <DigestId value={value} short={12} /></Line>)}
    </Step>,
  )
  if (asOf.length > 0) steps.push(
    <Step key="as-of" icon={Clock} title="As of">
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 text-xs">
        {asOf.map(({ label, value }) => <div key={label} className="contents"><dt className="text-muted-foreground">{label}</dt><dd className="min-w-0 truncate font-mono tabular-nums" title={value}>{value}</dd></div>)}
      </dl>
    </Step>,
  )
  if (steps.length === 0) return <p className="text-sm text-muted-foreground">The server recorded no source for this value.</p>
  return <ol className="space-y-2">{steps.flatMap((step, index) => index === 0 ? [step] : [<Arrow key={`arrow-${index}`} />, step])}</ol>
}

/** The card itself: what the value is, its chain, and the way to its full trace. */
export function ProvenanceCard({ label, value, notice, trace, children }: {
  readonly label: ReactNode
  readonly value?: ReactNode
  /** A line above the chain: the record moved on, or the history is shorter than the version shown. */
  readonly notice?: ReactNode
  /** The full trace: the record's lineage, or an answer's records with their receipts. */
  readonly trace?: ReactNode
  readonly children: ReactNode
}) {
  return (
    <div className="space-y-3" data-slot="provenance-card">
      <div className="space-y-0.5">
        <div className="text-xs text-muted-foreground">Where this value came from</div>
        <div className="flex min-w-0 items-baseline justify-between gap-3">
          <span className="min-w-0 truncate text-sm font-medium">{label}</span>
          {value !== undefined && <span className="shrink-0 font-mono text-sm tabular-nums">{value}</span>}
        </div>
      </div>
      {notice && <p role="status" className="rounded-md bg-muted px-2 py-1.5 text-xs text-muted-foreground">{notice}</p>}
      {children}
      {trace && <div className="border-t border-border/70 pt-2">{trace}</div>}
    </div>
  )
}

/** Clicks and keys inside the card stay in it: a grid row or table row under it must not take them. */
const keep = (event: SyntheticEvent) => event.stopPropagation()

const TRIGGER = "rounded-sm text-start underline decoration-dotted decoration-muted-foreground/60 underline-offset-4 outline-none hover:decoration-foreground focus-visible:ring-2 focus-visible:ring-ring/40"

/**
 * A value that opens its card. `inline` underlines the value; `glyph` puts a small button beside it
 * (a grid cell, whose row opens the record); `block` makes the whole element the trigger (a figure tile).
 */
export function ProvenancePopover({ children, variant = "inline", className, label, card, open, onOpenChange }: {
  readonly children: ReactNode
  readonly variant?: "inline" | "glyph" | "block"
  readonly className?: string
  readonly label: string
  readonly card: ReactNode
  readonly open?: boolean
  readonly onOpenChange?: (open: boolean) => void
}) {
  return (
    <Popover {...open === undefined ? {} : { open }} onOpenChange={onOpenChange}>
      {variant === "glyph"
        ? <span className={cn("flex min-w-0 items-center gap-1", className)}>
          <span className="min-w-0 flex-1 truncate">{children}</span>
          <PopoverTrigger tabIndex={-1} onClick={keep} aria-label={`Where ${label} came from`} title={`Where ${label} came from`}
            className="flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 outline-none transition-opacity group-hover/cell:opacity-100 hover:bg-muted hover:text-foreground focus-visible:opacity-100 data-[state=open]:opacity-100">
            <Waypoints className="size-3.5" aria-hidden />
          </PopoverTrigger>
        </span>
        : <PopoverTrigger onClick={keep} className={cn(variant === "inline" && TRIGGER, className)} {...variant === "block" ? { "aria-label": `${label}: where this value came from` } : {}}>{children}</PopoverTrigger>}
      <PopoverContent align="start" className="w-inspector max-w-[calc(100vw-2rem)] rounded-node border border-node-border shadow-float" onClick={keep} onKeyDown={keep}>
        {card}
      </PopoverContent>
    </Popover>
  )
}

/** Where a value of a recorded snapshot came from, from the record's lineage alone (no history is recorded). */
export function ProvenanceChain({ lineage, fact }: { readonly lineage: SourceLineage; readonly fact: string }) {
  if (lineage.fieldProvenance[fact] === undefined) return <p className="text-sm text-muted-foreground">The server recorded no source for this value.</p>
  return <ProvenanceSteps provenance={factProvenance(fact, null, lineage)} />
}

/** The history read behind a live fact's card, read once per record and shared with its History tab. */
function LiveFact({ objectType, id, version, fact, lineage }: { readonly objectType: string; readonly id: string; readonly version: number; readonly fact: string; readonly lineage: SourceLineage | null }) {
  const history = useQuery({ queryKey: ["history", objectType, id], queryFn: () => readHistory(objectType, id), retry: false })
  if (history.isPending) return <p className="text-sm text-muted-foreground">Reading where it came from…</p>
  if (history.isError) {
    const reason = history.error instanceof NotServed ? "This cell does not serve record history yet."
      : history.error instanceof CellRefusal && history.error.status === 404 ? "The cell shows no history of this record to your account."
      : history.error instanceof CellRefusal ? history.error.message : "Where this value came from could not be read."
    return <p className="text-sm text-muted-foreground">{reason}</p>
  }
  const { entry, newer } = versionShown(history.data.document.versions, version)
  const notice = entry === null ? `The cell's history does not list version ${version}${history.data.document.truncated ? "; it lists only the newest versions" : ""}.`
    : newer !== null ? `Version ${newer} has been written since; this is version ${version}.` : undefined
  return <>
    {notice && <p role="status" className="mb-3 rounded-md bg-muted px-2 py-1.5 text-xs text-muted-foreground">{notice}</p>}
    <ProvenanceSteps provenance={factProvenance(fact, entry, entry === null ? lineage : null)} />
  </>
}

/**
 * One fact of a record version, opening its card. Live, the card reads the record's history (the
 * version's commit and what wrote it); a recorded snapshot has no history and shows its lineage.
 */
export function FactProvenance({ objectType, id, version, fact, label, value, lineage, live, trace, variant, children }: {
  readonly objectType: string
  readonly id: string
  readonly version: number
  readonly fact: string
  readonly label: string
  readonly value: string
  readonly lineage: SourceLineage | null
  readonly live: boolean
  readonly trace?: ReactNode
  readonly variant?: "inline" | "glyph"
  readonly children: ReactNode
}) {
  const [open, setOpen] = useState(false)
  if (!live && lineage?.fieldProvenance[fact] === undefined) return <>{children}</>
  return (
    <ProvenancePopover variant={variant} label={label} onOpenChange={setOpen} card={
      <ProvenanceCard label={label} value={value} trace={trace}>
        {open && (live ? <LiveFact objectType={objectType} id={id} version={version} fact={fact} lineage={lineage} /> : lineage && <ProvenanceChain lineage={lineage} fact={fact} />)}
      </ProvenanceCard>
    }>{children}</ProvenancePopover>
  )
}

/** An answer's number that opens its card: the systems read, the metrics, the ledger head and its records. */
export function AnswerProvenance({ label, value, provenance, onTrace, records, variant, className, children }: {
  readonly label: string
  readonly value: string
  readonly provenance: ValueProvenance
  /** Opens the bucket's records with their receipts. */
  readonly onTrace?: () => void
  readonly records: number
  readonly variant?: "inline" | "block"
  readonly className?: string
  readonly children: ReactNode
}) {
  const [open, setOpen] = useState(false)
  const trace = onTrace && <button type="button" onClick={() => { setOpen(false); onTrace() }} className="text-sm font-medium underline-offset-4 hover:underline">See the {records} {records === 1 ? "record" : "records"} and their receipts</button>
  return (
    <ProvenancePopover variant={variant} className={className} label={label} open={open} onOpenChange={setOpen} card={<ProvenanceCard label={label} value={value} trace={trace}><ProvenanceSteps provenance={provenance} /></ProvenanceCard>}>
      {children}
    </ProvenancePopover>
  )
}
