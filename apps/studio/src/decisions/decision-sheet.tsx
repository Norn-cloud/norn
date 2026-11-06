import { Link } from "@tanstack/react-router"
import { AlertTriangle, CheckCircle2, Clock, Hourglass, Sparkles, UserCheck } from "lucide-react"
import { useEffect, useId, useState, type CSSProperties } from "react"
import { CopyId } from "@/components/copy-id"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { Textarea } from "@/components/ui/textarea"
import { useView } from "@/lib/view"
import { cn } from "@/lib/utils"
import { newIdempotencyKey } from "../lib/actions.ts"
import type { Diagnostic } from "../lib/cell-read.ts"
import { decide as decideCall, flowTitle, needsConfirm, remaining, stepTitle, urgent, DECISIONS_UNAVAILABLE, type DecideResult, type Decided, type Decision, type Resolution, type Risk } from "../lib/decisions.ts"

type Decide = (id: string, body: { readonly choice: string; readonly note?: string }, key: string) => Promise<DecideResult>

const RISK_TINT: Readonly<Record<Risk, string>> = { low: "--tint-route", medium: "--tint-wait", high: "--tint-law" }
const RISK_LABEL: Readonly<Record<Risk, string>> = { low: "Low risk", medium: "Medium risk", high: "High risk" }

export function RiskChip({ risk }: { readonly risk: Risk }) {
  return (
    <span style={{ "--tint": `var(${RISK_TINT[risk]})` } as CSSProperties}
      className="inline-flex items-center rounded-md bg-[color-mix(in_oklab,var(--tint)_14%,transparent)] px-1.5 py-0.5 text-xs text-[color-mix(in_oklab,var(--tint)_80%,var(--foreground))] ring-1 ring-[color-mix(in_oklab,var(--tint)_22%,transparent)] ring-inset">
      {RISK_LABEL[risk]}
    </span>
  )
}

/** "45 min left", urgent under an hour; "Expired" once the cell's clock passes the deadline. */
export function Countdown({ expiresAtMs, now }: { readonly expiresAtMs: number; readonly now: number | undefined }) {
  if (now === undefined) return null
  const left = remaining(expiresAtMs, now)
  const at = new Date(expiresAtMs)
  return (
    <span title={`Decide by ${at.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}`}
      className={cn("inline-flex items-center gap-1 text-meta tabular-nums", left === null ? "text-destructive" : urgent(expiresAtMs, now) ? "font-medium text-destructive" : "text-muted-foreground")}>
      <Clock className="size-3" aria-hidden />{left === null ? "Expired" : `${left} left`}
    </span>
  )
}

type Phase =
  | { readonly kind: "choosing" }
  | { readonly kind: "confirm" }
  | { readonly kind: "sending" }
  | { readonly kind: "decided"; readonly decided: Decided }
  | { readonly kind: "resolved"; readonly resolution: Resolution }
  | { readonly kind: "expired"; readonly message: string }
  | { readonly kind: "gone"; readonly message: string }
  | { readonly kind: "refused"; readonly message: string; readonly diagnostics: readonly Diagnostic[] }
  | { readonly kind: "failed"; readonly message: string }

const at = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })

/**
 * One decision: its question, how much is at stake, its options (the agent's pick marked as a
 * suggestion), an optional note, and Decide. The cell decides who may decide, and when; the sheet
 * shows what it answers, including another decider getting there first.
 */
export function DecisionSheet({ decision, now, onClose, onSettled, call = decideCall }: {
  readonly decision: Decision | null
  readonly now: number | undefined
  readonly onClose: () => void
  /** The inbox should read again (decided, resolved elsewhere, expired or gone). */
  readonly onSettled: () => void
  readonly call?: Decide
}) {
  const [asking, setAsking] = useState(false)
  const [dirty, setDirty] = useState(false)
  useEffect(() => { if (decision === null) { setDirty(false); setAsking(false) } }, [decision])
  const request = () => (dirty ? setAsking(true) : onClose())
  return (
    <>
      <Sheet open={decision !== null} onOpenChange={(open) => { if (!open) request() }}>
        <SheetContent className="w-[min(34rem,100vw)] gap-0 overflow-y-auto border-node-border shadow-float sm:max-w-lg">
          {decision !== null && <DecisionBody key={decision.id} decision={decision} now={now} onDirty={setDirty} onClose={onClose} onSettled={onSettled} call={call} />}
        </SheetContent>
      </Sheet>
      <Dialog open={asking} onOpenChange={setAsking}>
        <DialogContent className="rounded-node border-node-border shadow-float">
          <DialogHeader>
            <DialogTitle>Leave without deciding?</DialogTitle>
            <DialogDescription>Nothing has been sent. Your choice and note will be cleared; the decision stays in your inbox.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setAsking(false)}>Keep deciding</Button>
            <Button variant="outline" className="border-node-border" onClick={() => { setAsking(false); setDirty(false); onClose() }}>Leave</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  )
}

function DecisionBody({ decision, now, onDirty, onClose, onSettled, call }: {
  readonly decision: Decision; readonly now: number | undefined; readonly onDirty: (dirty: boolean) => void
  readonly onClose: () => void; readonly onSettled: () => void; readonly call: Decide
}) {
  const { view } = useView()
  const technical = view === "technical"
  const id = useId()
  const [choice, setChoice] = useState<string | null>(null)
  const [note, setNote] = useState("")
  const [key, setKey] = useState(newIdempotencyKey)
  const [phase, setPhase] = useState<Phase>({ kind: "choosing" })
  const option = decision.options.find((o) => o.id === choice)
  const open = now === undefined || remaining(decision.expiresAtMs, now) !== null
  const settled = phase.kind === "decided" || phase.kind === "resolved" || phase.kind === "expired" || phase.kind === "gone"
  const diagnostics = phase.kind === "refused" ? phase.diagnostics : []
  const at_ = (pointer: string) => diagnostics.filter((d) => d.pointer === pointer)
  const top = diagnostics.filter((d) => d.pointer !== "/choice" && d.pointer !== "/note")

  // The deadline passing while the sheet is open: nothing can be sent any more.
  useEffect(() => { if (!open && !settled) { setPhase({ kind: "expired", message: "This decision expired before it was decided." }); onDirty(false); onSettled() } }, [open, settled])

  const edit = (next: { choice?: string; note?: string }) => {
    if (next.choice !== undefined) setChoice(next.choice)
    if (next.note !== undefined) setNote(next.note)
    setKey(newIdempotencyKey())
    if (phase.kind === "confirm" || phase.kind === "failed") setPhase({ kind: "choosing" })
    onDirty(true)
  }

  const send = async () => {
    if (choice === null) return
    setPhase({ kind: "sending" })
    try {
      const result = await call(decision.id, { choice, ...note.trim() ? { note } : {} }, key)
      switch (result.kind) {
        case "decided": setPhase({ kind: "decided", decided: result.decided }); onDirty(false); onSettled(); break
        case "resolved": setPhase({ kind: "resolved", resolution: result.resolution }); onDirty(false); onSettled(); break
        case "expired": setPhase({ kind: "expired", message: result.message }); onDirty(false); onSettled(); break
        case "not-found": setPhase({ kind: "gone", message: result.message }); onDirty(false); onSettled(); break
        case "refused": setPhase({ kind: "refused", message: result.message, diagnostics: result.diagnostics }); break
        default: setPhase({ kind: "failed", message: result.kind === "no-cell" ? "This workspace has no data cell yet." : DECISIONS_UNAVAILABLE })
      }
    } catch (error) {
      setPhase({ kind: "failed", message: error instanceof Error ? error.message : "The cell did not answer; nothing was decided." })
    }
  }

  return (
    <>
      <SheetHeader className="px-6 pt-6 pb-4">
        <SheetTitle className="text-base">{decision.question}</SheetTitle>
        <SheetDescription className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span>{flowTitle(decision)} · {stepTitle(decision)}</span>
        </SheetDescription>
        <div className="flex flex-wrap items-center gap-2 pt-1">
          <RiskChip risk={decision.risk} />
          {!settled && <Countdown expiresAtMs={decision.expiresAtMs} now={now} />}
          <span className="flex-1" />
          <Link to="/runs/$id" params={{ id: decision.run.id }} className="text-meta underline-offset-4 hover:underline">Open run</Link>
        </div>
      </SheetHeader>
      <div className="grid gap-5 px-6">
        {decision.context.subjects.length > 0 && (
          <ul aria-label="About" className="flex flex-wrap gap-2 text-sm">
            {decision.context.subjects.map((s) => (
              <li key={`${s.objectType}:${s.id}`}><Link to="/sources/$concept/$" params={{ concept: s.objectType, _splat: s.id }} className="rounded-md border border-node-border px-2 py-0.5 underline-offset-4 hover:underline">{s.label ?? s.id}</Link></li>
            ))}
          </ul>
        )}
        {settled ? <Outcome phase={phase} decision={decision} technical={technical} onClose={onClose} /> : (
          <form noValidate className="grid gap-5" aria-label="Decide"
            onSubmit={(event) => { event.preventDefault(); if (phase.kind === "sending" || option === undefined) return; if (needsConfirm(decision, option) && phase.kind !== "confirm") setPhase({ kind: "confirm" }); else void send() }}>
            {phase.kind === "refused" && (
              <div role="alert" className="grid gap-1 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                <span className="font-medium">{phase.message}</span>
                {top.filter((d) => d.message !== phase.message).map((d) => <span key={d.code + d.message} className="text-meta">{d.message}</span>)}
              </div>
            )}
            {phase.kind === "failed" && (
              <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                <span>{phase.message}</span><span className="flex-1" />
                <Button type="button" variant="outline" size="sm" onClick={() => void send()}>Retry</Button>
              </div>
            )}
            <fieldset className="grid gap-2" aria-describedby={at_("/choice").length > 0 ? `${id}-choice` : undefined}>
              <legend className="mb-2 text-sm font-medium">Choose</legend>
              {decision.options.map((o) => {
                const chosen = o.id === choice
                const suggested = o.id === decision.agentPick
                return (
                  <label key={o.id} className={cn("grid cursor-pointer gap-1 rounded-node border bg-card p-3 shadow-[var(--node-highlight),var(--elevation-node)] transition-colors",
                    chosen ? "border-primary ring-2 ring-primary/30" : "border-node-border hover:bg-muted/40")}>
                    <span className="flex items-center gap-2">
                      <input type="radio" name={`${id}-choice`} value={o.id} checked={chosen} onChange={() => edit({ choice: o.id })} className="size-4 accent-[var(--primary)]" />
                      <span className="font-medium">{o.label}</span>
                      {suggested && <span className="inline-flex items-center gap-1 rounded-md bg-[color-mix(in_oklab,var(--tint-agent)_14%,transparent)] px-1.5 py-0.5 text-xs text-[color-mix(in_oklab,var(--tint-agent)_80%,var(--foreground))]" title="The agent's recommendation; you decide."><Sparkles className="size-3" aria-hidden />Suggested</span>}
                    </span>
                    <span className="ps-6 text-meta text-muted-foreground">{o.scope}</span>
                    <span className="ps-6 text-meta text-muted-foreground">
                      {"action" in o.effect ? "Starts an action" : "Records the choice"}
                      {technical && <code className="ms-1 text-xs">{"action" in o.effect ? o.effect.action : o.effect.binding}</code>}
                    </span>
                  </label>
                )
              })}
              {at_("/choice").length > 0 && <p id={`${id}-choice`} role="alert" className="text-meta text-destructive">{at_("/choice").map((d) => d.message).join(" ")}</p>}
            </fieldset>
            <div className="grid gap-1.5">
              <label htmlFor={`${id}-note`} className="text-sm font-medium">Note <span className="font-normal text-muted-foreground">(optional)</span></label>
              <Textarea id={`${id}-note`} value={note} onChange={(e) => edit({ note: e.target.value })} className="min-h-20"
                aria-invalid={at_("/note").length > 0 || undefined} aria-describedby={at_("/note").length > 0 ? `${id}-note-problem` : undefined} placeholder="Why, for whoever reads the run later" />
              {at_("/note").length > 0 && <p id={`${id}-note-problem`} role="alert" className="text-meta text-destructive">{at_("/note").map((d) => d.message).join(" ")}</p>}
            </div>
            {phase.kind === "confirm" && option !== undefined && (
              <div role="alertdialog" aria-labelledby={`${id}-confirm`} className="grid gap-1 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
                <p id={`${id}-confirm`} className="flex items-center gap-2 font-medium text-destructive"><AlertTriangle className="size-4" aria-hidden />
                  {decision.risk === "high" ? "This is a high-risk decision." : "This starts an action."}</p>
                <p className="text-muted-foreground">You're choosing "{option.label}" ({option.scope}). The run goes on with it as soon as the cell records it.</p>
              </div>
            )}
            <div className="sticky bottom-0 z-10 -mx-6 flex flex-wrap items-center gap-2 border-t border-node-border bg-card/95 px-6 py-3 backdrop-blur-sm">
              {option === undefined && <span className="text-meta text-muted-foreground">Choose an option</span>}
              <span className="flex-1" />
              {phase.kind === "confirm" && <Button type="button" variant="ghost" onClick={() => setPhase({ kind: "choosing" })}>Back</Button>}
              <Button type="submit" variant={phase.kind === "confirm" ? "destructive" : "default"} disabled={option === undefined || phase.kind === "sending"}>
                {phase.kind === "sending" ? "Sending…" : phase.kind === "confirm" ? `Yes, ${option?.label.toLowerCase() ?? "decide"}` : option ? `Decide: ${option.label}` : "Decide"}
              </Button>
            </div>
          </form>
        )}
      </div>
    </>
  )
}

function Outcome({ phase, decision, technical, onClose }: { readonly phase: Phase; readonly decision: Decision; readonly technical: boolean; readonly onClose: () => void }) {
  const card = "grid gap-3 rounded-node border border-node-border bg-card p-4 shadow-[var(--node-highlight),var(--elevation-node)]"
  const done = <div className="flex justify-end pb-6"><Button type="button" onClick={onClose}>Back to inbox</Button></div>
  if (phase.kind === "decided") {
    const { decided } = phase
    const label = decision.options.find((o) => o.id === decided.decision.choice)?.label ?? decided.decision.choice
    const recorded = new Date(decided.receipt.recordedAtMs)
    return (
      <section aria-label="Decided" className="grid gap-4">
        <div className={card}>
          <p role="status" className="flex items-start gap-2 text-sm font-medium"><CheckCircle2 className="mt-0.5 size-4 shrink-0 text-[var(--tint-outcome)]" aria-hidden />
            <span>Decided: {label} · {decided.decision.decidedBy === "you" ? "by you" : `by ${decided.decision.decidedBy}`}{decided.outcome === "replayed" && <span className="font-normal text-muted-foreground"> (already recorded; the cell returned the first receipt)</span>}</span></p>
          {decided.decision.note && <p className="text-sm text-muted-foreground">“{decided.decision.note}”</p>}
          <Link to="/runs/$id" params={{ id: decided.run.id }} className="justify-self-start text-sm underline-offset-4 hover:underline">See the run go on</Link>
          <details open={technical} className="text-sm">
            <summary className="cursor-pointer text-meta text-muted-foreground select-none hover:text-foreground">Details</summary>
            <dl className="mt-3 grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-2">
              <dt className="text-muted-foreground">Receipt</dt><dd className="min-w-0"><CopyId value={decided.receipt.receiptHash} short={16} /></dd>
              <dt className="text-muted-foreground">Commit</dt><dd className="tabular-nums">{decided.receipt.commitSeq}</dd>
              <dt className="text-muted-foreground">Recorded</dt><dd><time dateTime={recorded.toISOString()} className="whitespace-nowrap">{recorded.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })}</time></dd>
              <dt className="text-muted-foreground">Decision</dt><dd><code className="text-xs">{decision.id}</code></dd>
            </dl>
          </details>
        </div>
        {done}
      </section>
    )
  }
  const [icon, title, body] = phase.kind === "resolved"
    ? phase.resolution.resolution === "decided"
      ? [<UserCheck key="i" className="size-4" aria-hidden />, `Decided by ${phase.resolution.by ?? "another decider"} at ${at(phase.resolution.atMs)}`, "Someone else got there first. Nothing you chose was sent."]
      : phase.resolution.resolution === "withdrawn"
        ? [<Hourglass key="i" className="size-4" aria-hidden />, `Withdrawn at ${at(phase.resolution.atMs)}`, "The run no longer needs this decision."]
        : [<Clock key="i" className="size-4" aria-hidden />, `Expired at ${at(phase.resolution.atMs)}`, "The deadline passed before anyone decided."]
    : phase.kind === "expired" ? [<Clock key="i" className="size-4" aria-hidden />, "Expired", phase.message]
      : [<Hourglass key="i" className="size-4" aria-hidden />, "No longer here", (phase as { message: string }).message]
  return (
    <section aria-label="Resolved" className="grid gap-4">
      <div className={card}>
        <p role="status" className="flex items-center gap-2 text-sm font-medium">{icon}{title}</p>
        <p className="text-sm text-muted-foreground">{body}</p>
      </div>
      {done}
    </section>
  )
}
