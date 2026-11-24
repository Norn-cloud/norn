import { useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { CheckCircle2, CircleAlert, Clock, HelpCircle, Play, RotateCcw } from "lucide-react"
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { callAction, fieldsOf, newIdempotencyKey, risky, type ActionSummary, type CallResult, type FormField } from "../lib/actions.ts"
import { readStudioScope, type StudioScope } from "../lib/definitions.ts"

export interface BulkTarget {
  readonly id: string
  readonly name: string
}

export interface OpenBulkAction {
  readonly action: ActionSummary
  readonly targets: readonly BulkTarget[]
}

export interface BulkOutcome {
  readonly target: BulkTarget
  readonly status: "committed" | "pending" | "refused" | "unknown"
  readonly detail: string
  readonly decisionId?: string
  readonly idempotencyKey: string
  readonly payloadJson: string
  readonly scope: StudioScope
}

export interface BulkRecoveryState {
  readonly key: string
  readonly action: ActionSummary
  readonly targets: readonly BulkTarget[]
  readonly scope: StudioScope
  readonly outcomes: readonly BulkOutcome[]
}

export type BulkDispatch = (
  actionId: string,
  payloadJson: string,
  idempotencyKey: string,
  scope: StudioScope,
) => Promise<CallResult>

const SCOPE_CHANGED_MESSAGE =
  "The active workspace scope changed. Dispatch stopped so no record runs in another environment."

export function sameScope(a: StudioScope | null, b: StudioScope | null): boolean {
  return a !== null && b !== null && a.orgId === b.orgId && a.environmentId === b.environmentId
}

export function bulkRecoveryKey(
  scope: StudioScope | null,
  actionId: string,
  targets: readonly BulkTarget[],
): string {
  const scopePart = scope === null ? "" : `${scope.orgId}/${scope.environmentId}`
  const ids = [...targets.map((target) => target.id)].sort().join("\0")
  return `${scopePart}\0${actionId}\0${ids}`
}

export function matchesBulkRecovery(
  recovery: BulkRecoveryState | null | undefined,
  open: OpenBulkAction | null,
): recovery is BulkRecoveryState {
  if (!recovery || open === null) return false
  return recovery.key === bulkRecoveryKey(recovery.scope, open.action.actionId, open.targets)
}

export function unknownRecoveryBanner(unknownCount: number): string {
  return `${unknownCount} ${unknownCount === 1 ? "result" : "results"} unknown — retry with the same keys`
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

async function sendBulkTarget(
  actionId: string,
  target: BulkTarget,
  payloadJson: string,
  idempotencyKey: string,
  scope: StudioScope,
  dispatch: BulkDispatch,
): Promise<BulkOutcome> {
  try {
    const res = await dispatch(actionId, payloadJson, idempotencyKey, scope)
    if (res.kind === "receipt") {
      return {
        target,
        status: "committed",
        detail: res.receipt.outcome === "replayed"
          ? `Already committed (replayed commit #${res.receipt.receipt.commitSeq})`
          : `Committed (commit #${res.receipt.receipt.commitSeq})`,
        idempotencyKey,
        payloadJson,
        scope,
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
        scope,
      }
    }
    if (res.kind === "refused") {
      return { target, status: "refused", detail: res.message, idempotencyKey, payloadJson, scope }
    }
    return {
      target,
      status: "unknown",
      detail: "The cell did not return a confirmed answer. Retry safely with the same idempotency key.",
      idempotencyKey,
      payloadJson,
      scope,
    }
  } catch {
    return {
      target,
      status: "unknown",
      detail: "Connection lost before the cell answered — outcome unknown. Retry uses the same idempotency key so this record will not run twice.",
      idempotencyKey,
      payloadJson,
      scope,
    }
  }
}

/**
 * Run one bulk action sequentially under a single pinned workspace scope. Before each target,
 * verifies the active session scope still matches; if it moved, stops immediately without sending
 * further targets.
 */
export async function runBulkTargets(options: {
  readonly actionId: string
  readonly objectIdField: string
  readonly targets: readonly BulkTarget[]
  readonly shared: Readonly<Record<string, unknown>>
  readonly scope: StudioScope
  readonly readScope?: () => Promise<StudioScope | null>
  readonly dispatch?: BulkDispatch
  readonly newKey?: () => string
  readonly onProgress?: (outcomes: readonly BulkOutcome[]) => void
}): Promise<{ readonly outcomes: readonly BulkOutcome[]; readonly scopeChanged: boolean }> {
  const readScope = options.readScope ?? (() => readStudioScope())
  const dispatch: BulkDispatch = options.dispatch
    ?? ((actionId, payloadJson, idempotencyKey, scope) => callAction(actionId, payloadJson, idempotencyKey, undefined, scope))
  const newKey = options.newKey ?? newIdempotencyKey
  const results: BulkOutcome[] = []
  for (const target of options.targets) {
    let active: StudioScope | null
    try {
      active = await readScope()
    } catch {
      const payloadJson = JSON.stringify({ ...options.shared, [options.objectIdField]: target.id })
      results.push({
        target,
        status: "unknown",
        detail: "Could not confirm the active workspace scope — outcome unknown. Retry uses the same idempotency key.",
        idempotencyKey: newKey(),
        payloadJson,
        scope: options.scope,
      })
      options.onProgress?.([...results])
      continue
    }
    if (!sameScope(active, options.scope)) {
      return { outcomes: results, scopeChanged: true }
    }
    const payloadJson = JSON.stringify({ ...options.shared, [options.objectIdField]: target.id })
    const idempotencyKey = newKey()
    const outcome = await sendBulkTarget(
      options.actionId,
      target,
      payloadJson,
      idempotencyKey,
      options.scope,
      dispatch,
    )
    results.push(outcome)
    options.onProgress?.([...results])
  }
  return { outcomes: results, scopeChanged: false }
}

/**
 * Retry one unknown outcome under its exact original payload bytes, idempotency key and pinned
 * scope. Refuses to send if the active workspace scope no longer matches.
 */
export async function retryBulkTarget(options: {
  readonly actionId: string
  readonly outcome: BulkOutcome
  readonly readScope?: () => Promise<StudioScope | null>
  readonly dispatch?: BulkDispatch
}): Promise<{ readonly outcome: BulkOutcome; readonly scopeChanged: boolean }> {
  const readScope = options.readScope ?? (() => readStudioScope())
  const dispatch: BulkDispatch = options.dispatch
    ?? ((actionId, payloadJson, idempotencyKey, scope) => callAction(actionId, payloadJson, idempotencyKey, undefined, scope))
  let active: StudioScope | null
  try {
    active = await readScope()
  } catch {
    return {
      outcome: {
        ...options.outcome,
        status: "unknown",
        detail: "Could not confirm the active workspace scope — retry again once connected.",
      },
      scopeChanged: false,
    }
  }
  if (!sameScope(active, options.outcome.scope)) {
    return { outcome: options.outcome, scopeChanged: true }
  }
  const next = await sendBulkTarget(
    options.actionId,
    options.outcome.target,
    options.outcome.payloadJson,
    options.outcome.idempotencyKey,
    options.outcome.scope,
    dispatch,
  )
  return { outcome: next, scopeChanged: false }
}

/**
 * Run one server-offered action across several selected records. Each record is sent as its own
 * governed action call under one pinned scope with a fresh idempotency key; laws, permissions and
 * approvals apply to each record independently on the cell. Unknown outcomes keep their exact
 * payloads, idempotency keys and scope across close and reopen so operators can retry safely.
 */
export function BulkActionSheet({ open, recovery, onRecoveryChange, onClose, onDone }: {
  readonly open: OpenBulkAction | null
  readonly recovery?: BulkRecoveryState | null
  readonly onRecoveryChange?: (next: BulkRecoveryState | null) => void
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
  const [scopeError, setScopeError] = useState<string | null>(null)
  const [confirming, setConfirming] = useState(false)
  const [running, setRunning] = useState(false)
  const [outcomes, setOutcomes] = useState<readonly BulkOutcome[]>([])
  const [runScope, setRunScope] = useState<StudioScope | null>(null)
  const recoveryRef = useRef(recovery)
  recoveryRef.current = recovery

  useEffect(() => {
    if (open === null) {
      setValues({})
      setValidationError(null)
      setScopeError(null)
      setConfirming(false)
      setRunning(false)
      setOutcomes([])
      setRunScope(null)
      return
    }
    const saved = recoveryRef.current
    if (matchesBulkRecovery(saved, open)) {
      setOutcomes(saved.outcomes)
      setRunScope(saved.scope)
      setValidationError(null)
      setScopeError(null)
      setConfirming(false)
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
    setScopeError(null)
    setConfirming(false)
    setOutcomes([])
    setRunScope(null)
  }, [open, extraFields])

  const refreshQueries = (objectKind: string) => {
    void client.invalidateQueries({ queryKey: ["records", objectKind] })
    void client.invalidateQueries({ queryKey: ["actions", objectKind] })
    void client.invalidateQueries({ queryKey: ["decisions"] })
  }

  const syncRecovery = (action: ActionSummary, targets: readonly BulkTarget[], scope: StudioScope, next: readonly BulkOutcome[]) => {
    const hasUnknown = next.some((item) => item.status === "unknown")
    if (hasUnknown) {
      onRecoveryChange?.({
        key: bulkRecoveryKey(scope, action.actionId, targets),
        action,
        targets,
        scope,
        outcomes: next,
      })
    } else {
      if (recovery && recovery.key === bulkRecoveryKey(scope, action.actionId, targets)) {
        onRecoveryChange?.(null)
      }
      onDone?.()
    }
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
    setScopeError(null)
    if (risky(open.action) && !confirming) {
      setConfirming(true)
      return
    }
    setRunning(true)
    setConfirming(false)
    let capturedScope: StudioScope | null = null
    try {
      capturedScope = await readStudioScope()
    } catch {
      setRunning(false)
      setValidationError("Could not verify the active workspace scope. Nothing was sent.")
      return
    }
    if (capturedScope === null) {
      setRunning(false)
      setValidationError("This workspace has no data cell yet. Nothing was sent.")
      return
    }
    setRunScope(capturedScope)
    let finalOutcomes: readonly BulkOutcome[] = []
    try {
      const result = await runBulkTargets({
        actionId: open.action.actionId,
        objectIdField: open.action.objectIdField,
        targets: open.targets,
        shared: encoded.shared,
        scope: capturedScope,
        onProgress: (progress) => {
          finalOutcomes = progress
          setOutcomes(progress)
          if (progress.some((item) => item.status === "unknown")) {
            onRecoveryChange?.({
              key: bulkRecoveryKey(capturedScope!, open.action.actionId, open.targets),
              action: open.action,
              targets: open.targets,
              scope: capturedScope!,
              outcomes: progress,
            })
          }
        },
      })
      finalOutcomes = result.outcomes
      setOutcomes(result.outcomes)
      if (result.scopeChanged) setScopeError(SCOPE_CHANGED_MESSAGE)
    } finally {
      setRunning(false)
      if (finalOutcomes.length > 0) {
        refreshQueries(open.action.objectKind)
        syncRecovery(open.action, open.targets, capturedScope, finalOutcomes)
      }
    }
  }

  const retryTargets = async (targetIds: readonly string[]) => {
    if (open === null || running) return
    const toRetry = outcomes.filter((item) => item.status === "unknown" && targetIds.includes(item.target.id))
    if (toRetry.length === 0) return
    setRunning(true)
    setScopeError(null)
    let latest = outcomes
    const pinned = runScope ?? toRetry[0]!.scope
    try {
      for (const current of toRetry) {
        const res = await retryBulkTarget({ actionId: open.action.actionId, outcome: current })
        if (res.scopeChanged) {
          setScopeError(SCOPE_CHANGED_MESSAGE)
          break
        }
        latest = latest.map((item) => item.target.id === current.target.id ? res.outcome : item)
        setOutcomes(latest)
      }
    } finally {
      setRunning(false)
      refreshQueries(open.action.objectKind)
      syncRecovery(open.action, open.targets, pinned, latest)
    }
  }

  const finished = !running && outcomes.length > 0
  const committed = outcomes.filter((o) => o.status === "committed").length
  const pending = outcomes.filter((o) => o.status === "pending").length
  const refused = outcomes.filter((o) => o.status === "refused").length
  const unknownItems = outcomes.filter((o) => o.status === "unknown")
  const unknown = unknownItems.length

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
                  {scopeError && (
                    <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                      {scopeError}
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
                  {unknown > 0 && (
                    <div role="alert" className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
                      <div className="space-y-0.5">
                        <p className="font-medium">{unknownRecoveryBanner(unknown)}</p>
                        <p className="text-meta text-muted-foreground">Closing keeps these idempotency keys and payloads so you can reopen and retry safely.</p>
                      </div>
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        disabled={running}
                        onClick={() => void retryTargets(unknownItems.map((item) => item.target.id))}
                      >
                        <RotateCcw />Retry all unknown
                      </Button>
                    </div>
                  )}
                  {scopeError && (
                    <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                      {scopeError}
                    </p>
                  )}
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
                              onClick={() => void retryTargets([item.target.id])}
                            >
                              <RotateCcw />Retry {item.target.name}
                            </Button>
                          )}
                        </div>
                      </li>
                    ))}
                  </ol>
                  <div className="flex justify-end">
                    <Button type="button" variant={unknown > 0 ? "outline" : "default"} onClick={onClose} disabled={!finished}>
                      {unknown > 0 ? "Close (keep recovery)" : "Done"}
                    </Button>
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
