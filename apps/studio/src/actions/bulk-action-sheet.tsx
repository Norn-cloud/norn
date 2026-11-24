import { useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { CheckCircle2, CircleAlert, Clock, HelpCircle, Play, RotateCcw } from "lucide-react"
import { useEffect, useMemo, useState, type FormEvent } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { callAction, fieldsOf, newIdempotencyKey, risky, type ActionSummary, type FormField } from "../lib/actions.ts"

export interface BulkTarget {
  readonly id: string
  readonly name: string
}

export interface OpenBulkAction {
  readonly action: ActionSummary
  readonly targets: readonly BulkTarget[]
}

interface BulkOutcome {
  readonly target: BulkTarget
  readonly status: "committed" | "pending" | "refused" | "unknown"
  readonly detail: string
  readonly decisionId?: string
  readonly idempotencyKey: string
  readonly payloadJson: string
}

/** Validate and encode all shared non-target inputs before submitting any bulk call. */
export function encodeBulkSharedInputs(
  fields: readonly FormField[],
  values: Readonly<Record<string, string>>,
): { readonly ok: true; readonly shared: Record<string, unknown> } | { readonly ok: false; readonly error: string } {
  const shared: Record<string, unknown> = {}
  for (const field of fields) {
    const raw = values[field.name] ?? ""
    const trimmed = raw.trim()
    if (field.spec.kind === "boolean") {
      if (raw === "" && !field.required) continue
      if (raw !== "true" && raw !== "false") return { ok: false, error: `${field.label} must be Yes or No.` }
      shared[field.name] = raw === "true"
      continue
    }
    if (trimmed === "") {
      if (field.required) return { ok: false, error: `${field.label} is required.` }
      continue
    }
    switch (field.spec.kind) {
      case "integer": {
        if (!/^-?\d+$/.test(trimmed)) return { ok: false, error: `${field.label} must be a whole number.` }
        const parsed = Number(trimmed)
        if (!Number.isSafeInteger(parsed)) return { ok: false, error: `${field.label} is out of range.` }
        shared[field.name] = parsed
        break
      }
      case "number": {
        if (!/^-?\d+(\.\d+)?$/.test(trimmed)) return { ok: false, error: `${field.label} must be a valid number.` }
        const parsed = Number(trimmed)
        if (!Number.isFinite(parsed)) return { ok: false, error: `${field.label} must be a finite number.` }
        shared[field.name] = parsed
        break
      }
      case "choice": {
        if (!field.spec.values.includes(raw)) return { ok: false, error: `${field.label} must be one of the offered choices.` }
        shared[field.name] = raw
        break
      }
      default:
        shared[field.name] = raw
        break
    }
  }
  return { ok: true, shared }
}

/**
 * Run one server-offered action across several selected records. Each record is sent as its own
 * governed action call with a fresh idempotency key; laws, permissions and approvals apply to each
 * record independently on the cell.
 */
export function BulkActionSheet({ open, onClose, onDone }: {
  readonly open: OpenBulkAction | null
  readonly onClose: () => void
  readonly onDone?: () => void
}) {
  const client = useQueryClient()
  const extraFields = useMemo<readonly FormField[]>(() => {
    if (open === null) return []
    return fieldsOf(open.action.inputSchema).filter((field) => field.name !== open.action.objectIdField)
  }, [open])
  const [values, setValues] = useState<Record<string, string>>({})
  const [validationError, setValidationError] = useState<string | null>(null)
  const [confirming, setConfirming] = useState(false)
  const [running, setRunning] = useState(false)
  const [outcomes, setOutcomes] = useState<readonly BulkOutcome[]>([])

  useEffect(() => {
    if (open === null) {
      setValues({})
      setValidationError(null)
      setConfirming(false)
      setRunning(false)
      setOutcomes([])
      return
    }
    const init: Record<string, string> = {}
    for (const field of extraFields) {
      if (field.spec.kind === "fixed") init[field.name] = field.spec.value
      else if (field.spec.kind === "boolean") init[field.name] = "false"
      else init[field.name] = ""
    }
    setValues(init)
    setValidationError(null)
    setConfirming(false)
    setOutcomes([])
  }, [open, extraFields])

  const sendOne = async (
    actionId: string,
    target: BulkTarget,
    payloadJson: string,
    idempotencyKey: string,
  ): Promise<BulkOutcome> => {
    try {
      const res = await callAction(actionId, payloadJson, idempotencyKey)
      if (res.kind === "receipt") {
        return {
          target,
          status: "committed",
          detail: `Committed (commit #${res.receipt.receipt.commitSeq})`,
          idempotencyKey,
          payloadJson,
        }
      }
      if (res.kind === "pending") {
        return {
          target,
          status: "pending",
          detail: "Sent to Inbox for approval",
          decisionId: res.decision.id,
          idempotencyKey,
          payloadJson,
        }
      }
      if (res.kind === "refused") {
        return { target, status: "refused", detail: res.message, idempotencyKey, payloadJson }
      }
      return {
        target,
        status: "unknown",
        detail: "The cell did not return a confirmed answer. Retry safely with the same idempotency key.",
        idempotencyKey,
        payloadJson,
      }
    } catch {
      return {
        target,
        status: "unknown",
        detail: "Connection lost before the cell answered — outcome unknown. Retry uses the same idempotency key so this record will not run twice.",
        idempotencyKey,
        payloadJson,
      }
    }
  }

  const refreshQueries = (objectKind: string) => {
    void client.invalidateQueries({ queryKey: ["records", objectKind] })
    void client.invalidateQueries({ queryKey: ["actions", objectKind] })
    void client.invalidateQueries({ queryKey: ["decisions"] })
  }

  const runAll = async (event: FormEvent) => {
    event.preventDefault()
    if (open === null || running) return
    const encoded = encodeBulkSharedInputs(extraFields, values)
    if (!encoded.ok) {
      setValidationError(encoded.error)
      return
    }
    setValidationError(null)
    if (risky(open.action) && !confirming) {
      setConfirming(true)
      return
    }
    setRunning(true)
    setConfirming(false)
    const results: BulkOutcome[] = []
    let attempted = 0
    try {
      for (const target of open.targets) {
        const payload: Record<string, unknown> = { ...encoded.shared, [open.action.objectIdField]: target.id }
        const payloadJson = JSON.stringify(payload)
        const idempotencyKey = newIdempotencyKey()
        attempted += 1
        const outcome = await sendOne(open.action.actionId, target, payloadJson, idempotencyKey)
        results.push(outcome)
        setOutcomes([...results])
      }
    } finally {
      setRunning(false)
      if (attempted > 0) {
        refreshQueries(open.action.objectKind)
        onDone?.()
      }
    }
  }

  const retryUnknown = async (targetId: string) => {
    if (open === null || running) return
    const current = outcomes.find((item) => item.target.id === targetId && item.status === "unknown")
    if (!current) return
    setRunning(true)
    try {
      // Reuse the original idempotency key so an earlier unseen commit replays instead of running twice.
      const next = await sendOne(open.action.actionId, current.target, current.payloadJson, current.idempotencyKey)
      setOutcomes((list) => list.map((item) => item.target.id === targetId ? next : item))
    } finally {
      setRunning(false)
      refreshQueries(open.action.objectKind)
      onDone?.()
    }
  }

  const finished = !running && outcomes.length > 0
  const committed = outcomes.filter((o) => o.status === "committed").length
  const pending = outcomes.filter((o) => o.status === "pending").length
  const refused = outcomes.filter((o) => o.status === "refused").length
  const unknown = outcomes.filter((o) => o.status === "unknown").length

  return (
    <Sheet open={open !== null} onOpenChange={(next) => { if (!next && !running) onClose() }}>
      <SheetContent className="w-[min(36rem,100vw)] overflow-y-auto border-node-border shadow-float sm:max-w-xl">
        {open !== null && (
          <>
            <SheetHeader className="px-6 pt-6 pb-4">
              <SheetTitle>{open.action.title}</SheetTitle>
              <SheetDescription>
                Runs on {open.targets.length} selected record{open.targets.length === 1 ? "" : "s"}. Each record is checked individually by the cell.
              </SheetDescription>
            </SheetHeader>
            <div className="grid gap-4 px-6 pb-6">
              {outcomes.length === 0 ? (
                <form onSubmit={(event) => void runAll(event)} className="grid gap-4">
                  <div className="grid gap-1.5">
                    <span className="text-sm font-medium">Target records ({open.targets.length})</span>
                    <ul className="max-h-40 divide-y overflow-y-auto rounded-lg border bg-muted/30 text-sm">
                      {open.targets.map((target) => (
                        <li key={target.id} className="flex items-center justify-between gap-2 px-3 py-1.5">
                          <span className="truncate font-medium">{target.name}</span>
                          <span className="font-mono text-xs text-muted-foreground">{target.id}</span>
                        </li>
                      ))}
                    </ul>
                  </div>

                  {extraFields.map((field) => (
                    <label key={field.name} className="grid gap-1 text-sm">
                      <span className="font-medium">
                        {field.label}{field.required && <span className="ms-0.5 text-destructive">*</span>}
                      </span>
                      {field.spec.kind === "choice" ? (
                        <select
                          aria-label={field.label}
                          className="h-9 rounded-md border bg-background px-2.5 text-sm"
                          value={values[field.name] ?? ""}
                          onChange={(e) => { setValidationError(null); setValues((cur) => ({ ...cur, [field.name]: e.target.value })) }}
                        >
                          <option value="">Choose…</option>
                          {field.spec.values.map((choice) => <option key={choice} value={choice}>{choice}</option>)}
                        </select>
                      ) : field.spec.kind === "boolean" ? (
                        <select
                          aria-label={field.label}
                          className="h-9 rounded-md border bg-background px-2.5 text-sm"
                          value={values[field.name] ?? "false"}
                          onChange={(e) => { setValidationError(null); setValues((cur) => ({ ...cur, [field.name]: e.target.value })) }}
                        >
                          <option value="true">Yes</option>
                          <option value="false">No</option>
                        </select>
                      ) : (
                        <Input
                          aria-label={field.label}
                          readOnly={field.spec.kind === "fixed"}
                          inputMode={field.spec.kind === "integer" || field.spec.kind === "number" ? "decimal" : undefined}
                          value={values[field.name] ?? ""}
                          onChange={(e) => { setValidationError(null); setValues((cur) => ({ ...cur, [field.name]: e.target.value })) }}
                        />
                      )}
                      {field.description && <span className="text-meta text-muted-foreground">{field.description}</span>}
                    </label>
                  ))}

                  {validationError && (
                    <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                      {validationError}
                    </p>
                  )}

                  {confirming && (
                    <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm">
                      This action is marked irreversible or destructive. Submit again to run it on all {open.targets.length} records.
                    </p>
                  )}

                  <div className="flex items-center justify-end gap-2 pt-2">
                    <Button type="button" variant="ghost" onClick={onClose} disabled={running}>Cancel</Button>
                    <Button type="submit" variant={confirming ? "destructive" : "default"} disabled={running || open.targets.length === 0}>
                      <Play /> {confirming ? `Confirm ${open.action.title} (${open.targets.length})` : `Run on ${open.targets.length} record${open.targets.length === 1 ? "" : "s"}`}
                    </Button>
                  </div>
                </form>
              ) : (
                <div className="grid gap-4">
                  <div role="status" className="flex flex-wrap items-center gap-2 text-sm">
                    <Badge variant="outline">{committed} committed</Badge>
                    {pending > 0 && <Badge variant="secondary">{pending} sent to Inbox</Badge>}
                    {refused > 0 && <Badge variant="destructive">{refused} refused</Badge>}
                    {unknown > 0 && <Badge variant="outline">{unknown} outcome unknown</Badge>}
                  </div>
                  <ol className="divide-y rounded-lg border bg-card text-sm" aria-label="Bulk action results">
                    {outcomes.map((item) => (
                      <li key={item.target.id} className="flex items-start gap-2.5 px-3 py-2">
                        {item.status === "committed" && <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-[var(--tint-outcome)]" aria-hidden />}
                        {item.status === "pending" && <Clock className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />}
                        {item.status === "refused" && <CircleAlert className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />}
                        {item.status === "unknown" && <HelpCircle className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />}
                        <div className="min-w-0 flex-1">
                          <div className="flex items-baseline gap-2">
                            <span className="truncate font-medium">{item.target.name}</span>
                            <span className="font-mono text-xs text-muted-foreground">{item.target.id}</span>
                          </div>
                          <p className={item.status === "refused" ? "text-meta text-destructive" : "text-meta text-muted-foreground"}>
                            {item.detail}
                            {item.decisionId && (
                              <> · <Link to="/decisions" search={{ open: item.decisionId } as never} className="underline underline-offset-4" onClick={onClose}>Open in Inbox</Link></>
                            )}
                          </p>
                          {item.status === "unknown" && (
                            <Button
                              type="button"
                              size="sm"
                              variant="outline"
                              className="mt-1.5 h-7"
                              disabled={running}
                              onClick={() => void retryUnknown(item.target.id)}
                            >
                              <RotateCcw />Retry {item.target.name}
                            </Button>
                          )}
                        </div>
                      </li>
                    ))}
                  </ol>
                  <div className="flex justify-end">
                    <Button type="button" onClick={onClose} disabled={!finished}>Done</Button>
                  </div>
                </div>
              )}
            </div>
          </>
        )}
      </SheetContent>
    </Sheet>
  )
}
