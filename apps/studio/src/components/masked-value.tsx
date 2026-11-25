import { Eye, EyeOff, Fingerprint, Loader2, ShieldCheck } from "lucide-react"
import { createContext, useContext, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { cn } from "@/lib/utils"
import { CopyId } from "./copy-id.tsx"
import { TINTED, tintOf } from "./safe-prompt.tsx"
import { studioScopeKey, type StudioScope } from "../lib/definitions.ts"
import { maskHint, revealContext, revealKeys, revealProperty, shownReveal, type Masked, type Revealed } from "../lib/privacy.ts"
import { usePinnedScope, useReaderKey } from "../lib/session.ts"

/**
 * The workspace the surrounding records were read from (a scope-bound grid or drawer). A reveal
 * inside it is sent to that workspace, never to whichever one the session shows at click time.
 */
export const RevealScope = createContext<StudioScope | undefined>(undefined)


/** The projection as text: what the server sent in the value's place. */
const projection = (masked: Masked, projected: unknown): string =>
  masked.mode === "refuse" || projected === undefined ? "Withheld" : projected === null ? "—" : typeof projected === "string" ? projected : JSON.stringify(projected)

/** The small word beside a projected value: how it was projected and under which marking ("pseudonymised: pii.contact"). */
export const maskWord = (masked: Masked): string =>
  `${masked.mode === "pseudonym" ? "pseudonymised" : masked.mode === "refuse" ? "withheld" : "masked"}${masked.markings.length > 0 ? `: ${masked.markings.join(", ")}` : ""}`

const valueText = (value: unknown): string => (typeof value === "string" ? value : JSON.stringify(value))

/**
 * A marked property as the server projected it. A reader who may reveal sees the projection
 * blurred with "Reveal": the blur is presentation only, since the stored value is not in the page.
 * Revealing asks the cell, which records a receipt (who, what, when, why) and only then answers.
 * A revealed value is bound to the context the cell revealed it under: the record, the property, the
 * version and this projection. Each context is its own instance (keyed by it), so any change of
 * context (A → B, and back to A) discards the old instance with its revealed value and any reveal
 * still in flight: an answer can only ever reach the instance that asked for it.
 */
export function MaskedValue(props: MaskedValueProps) {
  const { objectType, id, version, property, masked } = props
  const context = revealContext(version, masked)
  const rowScope = useContext(RevealScope)
  // A new reader or workspace is a new instance too: a value revealed to one reader, or an answer
  // still on its way, never shows for the next.
  const reader = useReaderKey()
  return <Reveal key={JSON.stringify([reader, rowScope === undefined ? null : studioScopeKey(rowScope), objectType, id, property, context])} {...props} context={context} rowScope={rowScope} />
}

interface MaskedValueProps {
  readonly objectType: string
  readonly id: string
  /** The record version the cell projected; a reveal belongs to it. */
  readonly version: number | string
  readonly property: string
  readonly masked: Masked
  readonly projected: unknown
  readonly className?: string
  /** Single-line truncation for fixed-height table rows; detail views wrap cleanly instead of clipping. */
  readonly compact?: boolean
}

/** One reveal context's instance; it never outlives its context (see [`MaskedValue`]). */
function Reveal({ objectType, id, property, masked, projected, className, compact = false, context, rowScope }: MaskedValueProps & { readonly context: string; readonly rowScope: StudioScope | undefined }) {
  const pinnedScope = usePinnedScope(`${objectType}:${id}:${property}:${context}`)
  const boundScope = rowScope ?? pinnedScope
  const keys = useRef(revealKeys())
  const [revealed, setRevealed] = useState<Revealed | null>(null)
  const [open, setOpen] = useState(false)
  const [purpose, setPurpose] = useState("")
  const [state, setState] = useState<{ readonly kind: "idle" | "busy" } | { readonly kind: "error"; readonly message: string }>({ kind: "idle" })

  const shown = shownReveal(revealed, masked, context)
  if (shown !== null) {
    const text = valueText(shown.value)
    return (
      <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)} data-revealed="" onClick={(event) => event.stopPropagation()} title={`Revealed to you; receipt ${shown.receipt.commitSeq}${shown.receipt.purpose ? ` (${shown.receipt.purpose})` : ""}`}>
        <Eye className="size-3 shrink-0 text-muted-foreground" aria-hidden />
        <CopyId value={text} className="min-w-0" />
        <span className="shrink-0 font-mono text-meta text-muted-foreground">receipt #{shown.receipt.commitSeq}</span>
      </span>
    )
  }

  const Icon = masked.mode === "pseudonym" ? Fingerprint : masked.mode === "refuse" ? EyeOff : ShieldCheck
  const chip = (
    <span title={maskHint(masked)} data-masked={masked.mode} style={tintOf(masked.mode === "pseudonym" ? "CUSTOMER" : undefined)}
      className={cn(
        "inline-flex min-w-0 max-w-full items-center gap-x-1.5 gap-y-0.5 rounded-md px-1.5 py-0.5 text-meta",
        compact ? "overflow-hidden" : "flex-wrap",
        TINTED,
      )}>
      <Icon className="size-3 shrink-0" aria-hidden />
      <span className={cn("min-w-0 font-mono", compact ? "truncate" : "[overflow-wrap:anywhere]", masked.revealable && "blur-[3px] select-none")} aria-hidden={masked.revealable || undefined}>{projection(masked, projected)}</span>
      {masked.revealable && <span className="sr-only">Masked; you may reveal it</span>}
      <span data-mask-hint="" className={cn("text-meta font-normal opacity-75", compact ? "min-w-0 shrink-[4] truncate" : "whitespace-normal [overflow-wrap:anywhere]")}>{maskWord(masked)}</span>
    </span>
  )
  if (!masked.revealable) return <span className={cn("inline-flex min-w-0 max-w-full", className)}>{chip}</span>

  const reveal = async () => {
    setState({ kind: "busy" })
    // A retry after the cell did not answer is the same request: the same key, so at most one receipt.
    const result = await revealProperty(objectType, id, property, purpose, keys.current.for(purpose), undefined, boundScope)
    // This instance is this context's alone: once the context changes it is unmounted, and its
    // state (this answer included) is gone.
    if (result.kind !== "unavailable") keys.current.settle()
    if (result.kind === "revealed") { setRevealed({ value: result.value, receipt: result.receipt, context }); setOpen(false); setState({ kind: "idle" }); return }
    setRevealed(null)
    setState({ kind: "error", message: result.kind === "refused" ? result.message : "The cell is not reachable. Try again." })
  }

  return (
    <span className={cn("inline-flex min-w-0 max-w-full items-center gap-1.5", !compact && "flex-wrap", className)} onClick={(event) => event.stopPropagation()} onKeyDown={(event) => event.stopPropagation()}>
      {chip}
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button variant="outline" size="xs" data-reveal="">Reveal</Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-72 space-y-2 text-body">
          <p className="">Reveal {property.replaceAll("_", " ")}</p>
          <p className="text-meta text-muted-foreground">The cell records who revealed it, when and why, before it shows the value.</p>
          <Input value={purpose} onChange={(event) => setPurpose(event.target.value)} placeholder="Purpose (optional)" maxLength={500} aria-label="Purpose" />
          {state.kind === "error" && <p role="alert" className="text-meta text-destructive">{state.message}</p>}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" size="sm" onClick={() => setOpen(false)}>Cancel</Button>
            <Button size="sm" onClick={() => void reveal()} disabled={state.kind === "busy"} data-confirm-reveal="">
              {state.kind === "busy" && <Loader2 className="animate-spin" aria-hidden />}Reveal with receipt
            </Button>
          </div>
        </PopoverContent>
      </Popover>
    </span>
  )
}
