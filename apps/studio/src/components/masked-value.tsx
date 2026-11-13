import { Eye, EyeOff, Fingerprint, Loader2, ShieldCheck } from "lucide-react"
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { cn } from "@/lib/utils"
import { CopyId } from "./copy-id.tsx"
import { TINTED, tintOf } from "./safe-prompt.tsx"
import { maskHint, revealProperty, type Masked, type RevealReceipt } from "../lib/privacy.ts"

/** The projection as text: what the server sent in the value's place. */
const shown = (masked: Masked, projected: unknown): string =>
  masked.mode === "refuse" || projected === undefined ? "Withheld" : projected === null ? "—" : typeof projected === "string" ? projected : JSON.stringify(projected)

const valueText = (value: unknown): string => (typeof value === "string" ? value : JSON.stringify(value))

/**
 * A marked property as the server projected it. A reader who may reveal sees the projection
 * blurred with "Reveal": the blur is presentation only, since the stored value is not in the page.
 * Revealing asks the cell, which records a receipt (who, what, when, why) and only then answers.
 */
export function MaskedValue({ objectType, id, property, masked, projected, className }: {
  readonly objectType: string
  readonly id: string
  readonly property: string
  readonly masked: Masked
  readonly projected: unknown
  readonly className?: string
}) {
  const [revealed, setRevealed] = useState<{ readonly value: unknown; readonly receipt: RevealReceipt } | null>(null)
  const [open, setOpen] = useState(false)
  const [purpose, setPurpose] = useState("")
  const [state, setState] = useState<{ readonly kind: "idle" | "busy" } | { readonly kind: "error"; readonly message: string }>({ kind: "idle" })

  if (revealed !== null) {
    const text = valueText(revealed.value)
    return (
      <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)} data-revealed="" onClick={(event) => event.stopPropagation()} title={`Revealed to you; receipt ${revealed.receipt.commitSeq}${revealed.receipt.purpose ? ` (${revealed.receipt.purpose})` : ""}`}>
        <Eye className="size-3 shrink-0 text-muted-foreground" aria-hidden />
        <CopyId value={text} className="min-w-0" />
        <span className="shrink-0 font-mono text-[10px] text-muted-foreground">receipt #{revealed.receipt.commitSeq}</span>
      </span>
    )
  }

  const Icon = masked.mode === "pseudonym" ? Fingerprint : masked.mode === "refuse" ? EyeOff : ShieldCheck
  const chip = (
    <span title={maskHint(masked)} data-masked={masked.mode} style={tintOf(masked.mode === "pseudonym" ? "CUSTOMER" : undefined)}
      className={cn("inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-md px-1.5 py-0.5 text-xs", TINTED)}>
      <Icon className="size-3 shrink-0" aria-hidden />
      <span className={cn("truncate font-mono", masked.revealable && "blur-[3px] select-none")} aria-hidden={masked.revealable || undefined}>{shown(masked, projected)}</span>
      {masked.revealable && <span className="sr-only">Masked; you may reveal it</span>}
    </span>
  )
  if (!masked.revealable) return <span className={cn("inline-flex min-w-0", className)}>{chip}</span>

  const reveal = async () => {
    setState({ kind: "busy" })
    const result = await revealProperty(objectType, id, property, purpose)
    if (result.kind === "revealed") { setRevealed(result); setOpen(false); setState({ kind: "idle" }); return }
    setState({ kind: "error", message: result.kind === "refused" ? result.message : "The cell is not reachable." })
  }

  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)} onClick={(event) => event.stopPropagation()} onKeyDown={(event) => event.stopPropagation()}>
      {chip}
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button variant="outline" size="xs" data-reveal="">Reveal</Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-72 space-y-2 text-sm">
          <p className="font-medium">Reveal {property.replaceAll("_", " ")}</p>
          <p className="text-xs text-muted-foreground">The cell records who revealed it, when and why, before it shows the value.</p>
          <Input value={purpose} onChange={(event) => setPurpose(event.target.value)} placeholder="Purpose (optional)" maxLength={500} aria-label="Purpose" />
          {state.kind === "error" && <p role="alert" className="text-xs text-destructive">{state.message}</p>}
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
