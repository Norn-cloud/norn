import { useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { CheckCircle2, CircleAlert, Clock, Play } from "lucide-react"
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
  readonly status: "committed" | "pending" | "refused"
  readonly detail: string
  readonly decisionId?: string
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
  const [confirming, setConfirming] = useState(false)
  const [running, setRunning] = useState(false)
  const [outcomes, setOutcomes] = useState<readonly BulkOutcome[]>([])

  useEffect(() => {
    if (open === null) {
      setValues({})
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
    setConfirming(false)
    setOutcomes([])
  }, [open, extraFields])

  const encodeValue = (field: FormField, raw: string): unknown => {
    if (raw === "" && !field.required) return undefined
    switch (field.spec.kind) {
      case "integer":
      case "number": {
        const parsed = Number(raw)
        return Number.isNaN(parsed) ? raw : parsed
      }
      case "boolean":
        return raw === "true"
      default:
        return raw
    }
  }

  const runAll = async (event: FormEvent) => {
    event.preventDefault()
    if (open === null || running) return
    if (risky(open.action) && !confirming) {
      setConfirming(true)
      return
    }
    setRunning(true)
    setConfirming(false)
    const results: BulkOutcome[] = []
    for (const target of open.targets) {
      const payload: Record<string, unknown> = { [open.action.objectIdField]: target.id }
      for (const field of extraFields) {
        const encoded = encodeValue(field, values[field.name] ?? "")
        if (encoded !== undefined) payload[field.name] = encoded
      }
      const res = await callAction(open.action.actionId, JSON.stringify(payload), newIdempotencyKey())
      if (res.kind === "receipt") {
        results.push({
          target,
          status: "committed",
          detail: `Committed (commit #${res.receipt.receipt.commitSeq})`,
        })
      } else if (res.kind === "pending") {
        results.push({
          target,
          status: "pending",
          detail: "Sent to Inbox for approval",
          decisionId: res.decision.id,
        })
      } else if (res.kind === "refused") {
        results.push({ target, status: "refused", detail: res.message })
      } else {
        results.push({ target, status: "refused", detail: "The cell did not accept this call." })
      }
      setOutcomes([...results])
    }
    setRunning(false)
    void client.invalidateQueries({ queryKey: ["records", open.action.objectKind] })
    void client.invalidateQueries({ queryKey: ["actions", open.action.objectKind] })
    void client.invalidateQueries({ queryKey: ["decisions"] })
    onDone?.()
  }

  const finished = !running && outcomes.length > 0
  const committed = outcomes.filter((o) => o.status === "committed").length
  const pending = outcomes.filter((o) => o.status === "pending").length
  const refused = outcomes.filter((o) => o.status === "refused").length

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
                          onChange={(e) => setValues((cur) => ({ ...cur, [field.name]: e.target.value }))}
                        >
                          <option value="">Choose…</option>
                          {field.spec.values.map((choice) => <option key={choice} value={choice}>{choice}</option>)}
                        </select>
                      ) : field.spec.kind === "boolean" ? (
                        <select
                          aria-label={field.label}
                          className="h-9 rounded-md border bg-background px-2.5 text-sm"
                          value={values[field.name] ?? "false"}
                          onChange={(e) => setValues((cur) => ({ ...cur, [field.name]: e.target.value }))}
                        >
                          <option value="true">Yes</option>
                          <option value="false">No</option>
                        </select>
                      ) : (
                        <Input
                          aria-label={field.label}
                          readOnly={field.spec.kind === "fixed"}
                          value={values[field.name] ?? ""}
                          onChange={(e) => setValues((cur) => ({ ...cur, [field.name]: e.target.value }))}
                        />
                      )}
                      {field.description && <span className="text-meta text-muted-foreground">{field.description}</span>}
                    </label>
                  ))}

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
                  </div>
                  <ol className="divide-y rounded-lg border bg-card text-sm" aria-label="Bulk action results">
                    {outcomes.map((item) => (
                      <li key={item.target.id} className="flex items-start gap-2.5 px-3 py-2">
                        {item.status === "committed" && <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-[var(--tint-outcome)]" aria-hidden />}
                        {item.status === "pending" && <Clock className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />}
                        {item.status === "refused" && <CircleAlert className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />}
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
