import { AtSign, CircleCheck, CreditCard, Hash, Landmark, LoaderCircle, MapPin, Phone, ShieldCheck, Sparkles, UserRound, type LucideIcon } from "lucide-react"
import { Fragment, useState, type CSSProperties } from "react"
import { cn } from "@/lib/utils"
import { cellCall } from "@/lib/cell-read"

/**
 * PRIVACY-GUARD-1 in Studio: what the model was sent and what came back. The cell tokenized the
 * prompt (markings first, then a detector pass) and keeps the token map; this view only shows its
 * documents. "Show original" asks the cell to restore the prompt, which it does only for tokens this
 * reader may reveal, each as a logged reveal; it appears only when some token is revealable.
 */

export interface GuardToken { readonly token: string; readonly kind: string; readonly label: string; readonly source: string; readonly revealable: boolean }
export interface RestoredSegment { readonly text: string; readonly token?: string; readonly kind?: string; readonly state?: "restored" | "projected" | "kept" }
export interface GuardDocument {
  readonly guardId: number
  readonly receiptHash?: string
  readonly safePrompt: string
  readonly tokens: readonly GuardToken[]
  readonly reply: readonly RestoredSegment[]
  readonly model: string
}

const ICONS: Readonly<Record<string, LucideIcon>> = {
  CUSTOMER: UserRound, NAME: UserRound, PERSON: UserRound, EMAIL: AtSign, PHONE: Phone, CARD: CreditCard, ADDRESS: MapPin, ACCOUNT: Landmark,
}
export const iconOf = (kind: string | undefined): LucideIcon => (kind === undefined ? Hash : ICONS[kind] ?? Hash)

/** Each data type's muted tint (the canvas tints, D65), as in the enum chips; the icon tells the type apart too. */
const TINTS: Readonly<Record<string, string>> = {
  CUSTOMER: "--tint-tool", NAME: "--tint-tool", PERSON: "--tint-tool", EMAIL: "--tint-agent", PHONE: "--tint-ask", CARD: "--tint-outcome", ADDRESS: "--tint-route", ACCOUNT: "--tint-wait",
}
export const tintOf = (kind: string | undefined): CSSProperties => ({ "--tint": `var(${(kind !== undefined && TINTS[kind]) || "--tint-state"})` } as CSSProperties)
/** A tinted chip surface: the type's colour at low strength, its text darkened for contrast. */
export const TINTED = "bg-[color-mix(in_oklab,var(--tint)_12%,transparent)] text-[color-mix(in_oklab,var(--tint)_78%,var(--foreground))] ring-1 ring-[color-mix(in_oklab,var(--tint)_24%,transparent)] ring-inset"

/** A placeholder token as a chip: monospace, with its type's icon. Told apart by icon, never by colour. */
export function TokenChip({ token, kind }: { readonly token: string; readonly kind?: string }) {
  const Icon = iconOf(kind)
  return (
    <span data-token={token} style={tintOf(kind)} className={cn("mx-0.5 inline-flex items-center gap-1 rounded-md px-1.5 py-px align-baseline font-mono text-[0.78em] font-medium", TINTED)}>
      <Icon className="size-3 shrink-0" aria-hidden />{token}
    </span>
  )
}

/** Split a text at the guard's tokens. */
export function splitTokens(text: string, tokens: readonly GuardToken[]): readonly RestoredSegment[] {
  const known = new Map(tokens.map((token) => [token.token, token]))
  const out: RestoredSegment[] = []
  let at = 0
  for (const match of text.matchAll(/\[[A-Z0-9_]+_\d+\]/g)) {
    const token = known.get(match[0])
    if (token === undefined || match.index === undefined) continue
    if (match.index > at) out.push({ text: text.slice(at, match.index) })
    out.push({ text: match[0], token: match[0], kind: token.kind, state: "kept" })
    at = match.index + match[0].length
  }
  if (at < text.length) out.push({ text: text.slice(at) })
  return out
}

/** Text in pieces: plain text, kept tokens as chips, restored values set apart by weight. */
export function Segments({ segments }: { readonly segments: readonly RestoredSegment[] }) {
  return segments.map((segment, index) => {
    if (segment.token === undefined) return <Fragment key={index}>{segment.text}</Fragment>
    if (segment.state === "kept" || segment.state === undefined) return <TokenChip key={index} token={segment.token} kind={segment.kind} />
    const Icon = iconOf(segment.kind)
    return (
      <span key={index} data-restored={segment.state} style={tintOf(segment.kind)} title={segment.state === "restored" ? `${segment.token}, restored for you` : `${segment.token}: the pseudonym you may see`}
        className={cn("mx-0.5 inline-flex items-center gap-1 rounded-md px-1 align-baseline", TINTED, segment.state === "restored" ? "font-medium" : "font-mono text-[0.85em]")}>
        <Icon className="size-3 shrink-0" aria-hidden />{segment.text}
      </span>
    )
  })
}

/** The guard's path: you, the cell's privacy guard, the model. */
function Flow({ model }: { readonly model: string }) {
  return (
    <div className="flex items-center gap-2 text-sm" aria-label={`You, then the privacy guard, then ${model}`}>
      <span className="flex size-5 items-center justify-center rounded-full border border-border bg-muted text-[10px] font-semibold">You</span>
      <span className="sr-only">You</span>
      <span aria-hidden className="h-px flex-1 bg-border" />
      <span className="inline-flex items-center gap-1.5 font-medium"><ShieldCheck className="size-4" aria-hidden />Privacy guard</span>
      <span aria-hidden className="h-px flex-1 bg-border" />
      <span className="inline-flex items-center gap-1.5 text-muted-foreground"><Sparkles className="size-4" aria-hidden />{model}</span>
    </div>
  )
}

export function SafePrompt({ guard }: { readonly guard: GuardDocument }) {
  const [original, setOriginal] = useState<{ readonly kind: "idle" } | { readonly kind: "busy" } | { readonly kind: "shown"; readonly segments: readonly RestoredSegment[] } | { readonly kind: "error"; readonly message: string }>({ kind: "idle" })
  const revealable = guard.tokens.some((token) => token.revealable)
  const legend = guard.tokens.filter((token, index, all) => all.findIndex((other) => other.kind === token.kind) === index || all.length <= 6)
  const restored = guard.reply.some((segment) => segment.state === "restored" || segment.state === "projected")
  const toggle = async (on: boolean) => {
    if (!on) { setOriginal({ kind: "idle" }); return }
    setOriginal({ kind: "busy" })
    // Showing the original is itself a reveal: the cell logs one receipt per field it restores.
    const result = await cellCall<{ segments: RestoredSegment[] }>(`/v1/guard/${guard.guardId}:restore`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ text: guard.safePrompt, idempotencyKey: `original-${guard.guardId}-${globalThis.crypto.randomUUID()}` }),
    })
    setOriginal(result.kind === "ok" ? { kind: "shown", segments: result.value.segments } : { kind: "error", message: result.kind === "refused" ? result.refusal.message : "The cell could not restore the prompt." })
  }
  const shown = original.kind === "shown" ? original.segments : splitTokens(guard.safePrompt, guard.tokens)
  return (
    <section data-safe-prompt className="grid gap-3 rounded-2xl border border-border bg-card p-4 shadow-sm" aria-label="Safe prompt">
      <header className="flex items-center justify-between gap-3">
        <h3 className="text-sm font-medium text-muted-foreground">{original.kind === "shown" ? "Original prompt" : "Safe prompt"}</h3>
        {revealable && (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            {original.kind === "busy" && <LoaderCircle className="size-3.5 animate-spin" aria-hidden />}
            Show original
            <button type="button" role="switch" aria-checked={original.kind === "shown"} aria-label="Show original" data-show-original="" disabled={original.kind === "busy"}
              onClick={() => void toggle(original.kind !== "shown")}
              className="relative h-5 w-9 shrink-0 rounded-full border border-border bg-muted transition-colors aria-checked:bg-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none">
              <span aria-hidden className={cn("absolute top-0.5 size-3.5 rounded-full bg-background shadow transition-[left]", original.kind === "shown" ? "left-[1.1rem]" : "left-0.5")} />
            </button>
          </div>
        )}
      </header>
      <p className="rounded-xl bg-muted/60 px-3.5 py-3 text-sm leading-7 whitespace-pre-wrap" data-prompt>
        <Segments segments={shown} />
      </p>
      {original.kind === "error" && <p role="alert" className="text-xs text-destructive">{original.message}</p>}
      {guard.tokens.length > 0 && (
        <ul className="grid grid-cols-[repeat(auto-fill,minmax(9rem,1fr))] gap-x-4 gap-y-2" aria-label="Withheld from the model">
          {legend.map((token) => {
            const Icon = iconOf(token.kind)
            return (
              <li key={token.token} className="grid gap-0.5 text-sm" style={tintOf(token.kind)}>
                <span className="inline-flex items-center gap-1.5 font-medium"><Icon className="size-3.5 text-[color-mix(in_oklab,var(--tint)_80%,var(--foreground))]" aria-hidden />{token.label}</span>
                <span className="ps-5 font-mono text-xs text-[color-mix(in_oklab,var(--tint)_78%,var(--foreground))]">{token.token}</span>
              </li>
            )
          })}
        </ul>
      )}
      <Flow model={guard.model} />
      {guard.reply.length > 0 && <>
        <div className="flex items-center justify-between gap-3 text-sm">
          <span className="inline-flex items-center gap-1.5 text-muted-foreground"><Sparkles className="size-4" aria-hidden />Reply from {guard.model}</span>
          <span className="inline-flex items-center gap-1 text-xs text-muted-foreground" data-reply-state>{restored ? <><CircleCheck className="size-3.5" aria-hidden />Restored</> : "Nothing to restore"}</span>
        </div>
        <p className="rounded-xl bg-muted/60 px-3.5 py-3 text-sm leading-7" data-reply><Segments segments={guard.reply} /></p>
      </>}
      <footer className="flex items-center gap-2 border-t border-border pt-3 text-xs text-muted-foreground">
        <ShieldCheck className="size-3.5" aria-hidden />
        <span>The model saw only the safe prompt. The cell keeps the token map; receipt #{guard.guardId}.</span>
      </footer>
    </section>
  )
}
