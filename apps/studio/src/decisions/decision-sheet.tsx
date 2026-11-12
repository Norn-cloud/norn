import { useNavigate } from "@tanstack/react-router"
import { useQuery } from "@tanstack/react-query"
import { Clock } from "lucide-react"
import { useEffect, useRef, type CSSProperties } from "react"
import "@ui-sdk/web/radio"
import "@ui-sdk/web/textarea"
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet"
import { useLocale } from "@/lib/locale"
import { cn } from "@/lib/utils"
import { defineDecisionSheet, decideRequest, type NornDecisionSheet, type SheetDocument } from "../../../../packages/norn-demo-ui/src/elements/decision-sheet.ts"
import { fetchTransport, wasmEvaluate } from "../../../../packages/norn-demo-ui/src/machine/index.ts"
import { loadEngine } from "../lib/canvas/checker.ts"
import { defaultFetch, type Fetch } from "../lib/cell-read.ts"
import { scopedHostFetch } from "../lib/definitions.ts"
import { flowTitle, remaining, stepTitle, urgent, type Decision, type Risk } from "../lib/decisions.ts"
import sheetDocument from "./decision-sheet.widget.json"

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

const DOCUMENT = sheetDocument as unknown as SheetDocument
/** States after which the sheet has nothing left to send: closing it needs no question. */
const SETTLED = new Set(["decided", "resolved", "expired", "gone", "closed"])

/**
 * One decision, as the admitted page document `decision-sheet.widget.json` declares it: the
 * framework-free `<norn-decision-sheet>` element runs the page machine (UI-MACHINE-1) with the
 * checker's own engine and draws UI SDK parts. This component only hosts it in Studio's sheet:
 * Escape and the close button ask the machine, which decides whether to close or to ask first.
 * The cell decides who may decide, and when.
 */
export function DecisionSheet({ decision, now, onClose, onSettled, fetcher = defaultFetch }: {
  readonly decision: Decision | null
  readonly now: number | undefined
  readonly onClose: () => void
  /** The inbox should read again (decided, resolved elsewhere, expired or gone). */
  readonly onSettled: () => void
  readonly fetcher?: Fetch
}) {
  const { locale } = useLocale()
  const navigate = useNavigate()
  const engine = useQuery({ queryKey: ["checker-engine"], queryFn: () => loadEngine(), staleTime: Infinity, retry: false })
  const host = useRef<HTMLDivElement>(null)
  const element = useRef<NornDecisionSheet | null>(null)
  const callbacks = useRef({ onClose, onSettled })
  callbacks.current = { onClose, onSettled }

  useEffect(() => {
    const target = host.current
    if (decision === null || engine.data === undefined || target === null) return
    defineDecisionSheet()
    const sheet = document.createElement("norn-decision-sheet") as NornDecisionSheet
    const call = scopedHostFetch(fetcher)
    sheet.options = {
      document: DOCUMENT,
      decision,
      evaluate: wasmEvaluate(engine.data),
      transport: fetchTransport((_, input, key) => decideRequest(decision.id, input, key), call),
      locale: locale === "ar" ? "ar" : "en",
      ...(now === undefined ? {} : { now }),
      navigate: (path) => void navigate({ to: path }),
      onClose: () => callbacks.current.onClose(),
      onSettled: (state) => { if (state !== "closed") callbacks.current.onSettled() },
    }
    target.replaceChildren(sheet)
    element.current = sheet
    return () => { sheet.remove(); element.current = null }
    // A new decision (or the engine arriving) mounts a fresh machine; `now` only repaints.
  }, [decision?.id, engine.data, locale])

  useEffect(() => {
    if (now !== undefined) (element.current as unknown as { setNow?: (ms: number) => void } | null)?.setNow?.(now)
  }, [now])

  const request = () => {
    const sheet = element.current
    if (sheet === null || sheet.request("close-button")) return
    if (sheet.snapshot === null || SETTLED.has(sheet.snapshot.stateId)) onClose()
  }

  return (
    <Sheet open={decision !== null} onOpenChange={(open) => { if (!open) request() }}>
      <SheetContent className="w-[min(34rem,100vw)] gap-0 overflow-y-auto border-node-border p-0 shadow-float sm:max-w-lg">
        <SheetTitle className="sr-only">{decision?.question ?? ""}</SheetTitle>
        <SheetDescription className="sr-only">{decision ? `${flowTitle(decision)} · ${stepTitle(decision)}` : ""}</SheetDescription>
        {engine.isError
          ? <p role="alert" className="p-6 text-sm text-destructive">The decision engine is not part of this build, so nothing can be decided here.</p>
          : <div ref={host} className="contents" />}
      </SheetContent>
    </Sheet>
  )
}
