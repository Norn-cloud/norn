import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { ChartLine, Clock, Database, FileUp, RefreshCw, Rows3, Upload } from "lucide-react"
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { refreshMode } from "@/lib/live-transport"
import { explainError } from "@/lib/plain-words"
import { useLocale } from "@/lib/locale"
import { useView } from "@/lib/view"
import {
  KINDS,
  columnId,
  define,
  isUncertainWriteError,
  kindOf,
  loadDataset,
  loadRecords,
  readUploadConnection,
  reconcileUploadConnection,
  upload,
  uploadConnectionId,
  type Completed,
  type DatasetLoaded,
  type DefineColumn,
  type Kind,
  type RecordsLoaded,
  type UploadConnectionSnapshot,
} from "@/lib/uploads"

/** One column's choices: kept or left out, part of the key, its kind, and whether its values add up. */
interface Choice { readonly include: boolean; readonly key: boolean; readonly type: Kind; readonly measure: boolean }

const numeric = (type: Kind) => type === "integer" || type === "decimal"

/** What loading came to: rows in the analytical engine, rows as governed records, or a cell with no engine. */
type Outcome =
  | { readonly kind: "dataset"; readonly objectType: string; readonly loaded: DatasetLoaded; readonly reconciled?: boolean }
  | { readonly kind: "records"; readonly objectType: string; readonly loaded: RecordsLoaded; readonly reconciled?: boolean }
  | { readonly kind: "no-engine"; readonly objectType: string }

/** A write whose client deadline or connection lapsed before the server replied; duplicate submission is blocked until reconciled. */
interface UnknownLoad {
  readonly kind: "dataset" | "records"
  readonly objectType: string
  readonly connectionId: string
  readonly sha256: string
  readonly size: number
  readonly baselineVersion: number
  readonly baselineEndedAt: string | null
  readonly phase: "checking" | "unresolved"
  readonly error: unknown
}

function outcomeFromReconciled(unknown: UnknownLoad, snapshot: UploadConnectionSnapshot): Outcome {
  if (unknown.kind === "dataset") {
    return {
      kind: "dataset",
      objectType: unknown.objectType,
      reconciled: true,
      loaded: {
        objectType: unknown.objectType,
        rows: snapshot.rowsLanded ?? snapshot.rowsRead ?? 0,
        rejected: snapshot.rowsQuarantined ?? 0,
        reasons: snapshot.quarantineReasons,
        bytes: unknown.size,
        millis: 0,
      },
    }
  }
  return {
    kind: "records",
    objectType: unknown.objectType,
    reconciled: true,
    loaded: {
      connectionId: snapshot.id,
      objectType: unknown.objectType,
      definitionsVersion: 0,
      ...snapshot.rowsRead === null ? {} : { rowsRead: snapshot.rowsRead },
      ...snapshot.rowsLanded === null ? {} : { rowsLanded: snapshot.rowsLanded },
      ...snapshot.rowsQuarantined === null ? {} : { rowsQuarantined: snapshot.rowsQuarantined },
      quarantineReasons: snapshot.quarantineReasons,
    },
  }
}

/**
 * Upload a CSV and make it a typed dataset: the file goes to storage under a URL the cell signs, the
 * cell reads it back and proposes each column's kind, the reader names the type, its key and which
 * numbers add up, the cell admits that type, and its rows load into the cell's analytical engine
 * (or, on a cell without one and only when the reader asks, row by row as governed records). The
 * cell checks every choice; nothing is checked here.
 */
export function UploadCsv() {
  const [open, setOpen] = useState(false)
  const [file, setFile] = useState<File | null>(null)
  const [typeName, setTypeName] = useState("")
  const [choices, setChoices] = useState<Readonly<Record<string, Choice>>>({})
  const [outcome, setOutcome] = useState<Outcome | null>(null)
  const [unknownLoad, setUnknownLoad] = useState<UnknownLoad | null>(null)
  const client = useQueryClient()
  const sent = useMutation({
    mutationFn: (chosen: File) => upload(chosen),
    onSuccess: ({ completed }) => setChoices(Object.fromEntries(completed.columns.map((column, index) => {
      const type = kindOf(column.inferred)
      return [columnId(column), { include: true, key: index === 0, type, measure: type === "decimal" && index !== 0 }]
    }))),
  })
  const completed: Completed | undefined = sent.data?.completed
  const columns = (): readonly DefineColumn[] => (completed?.columns ?? []).map((column) => {
    const id = columnId(column)
    const choice = choices[id] ?? { include: true, key: false, type: kindOf(column.inferred), measure: false }
    return { id, type: choice.type, include: choice.include, measure: choice.include && numeric(choice.type) && choice.measure }
  })
  const key = () => Object.entries(choices).filter(([, { key: inKey, include }]) => inKey && include).map(([id]) => id)
  const settle = async () => { refreshMode(); await client.invalidateQueries() }

  const reconcile = async (pending: UnknownLoad) => {
    setUnknownLoad({ ...pending, phase: "checking" })
    const landed = await reconcileUploadConnection({
      connectionId: pending.connectionId,
      sha256: pending.sha256,
      baselineVersion: pending.baselineVersion,
      baselineEndedAt: pending.baselineEndedAt,
    })
    if (landed !== null) {
      setUnknownLoad(null)
      setOutcome(outcomeFromReconciled(pending, landed))
      await settle()
      return
    }
    setUnknownLoad({ ...pending, phase: "unresolved" })
  }

  const asDataset = useMutation({
    mutationFn: async () => {
      if (!sent.data || !completed) throw new Error("Upload a file first.")
      const trimmed = typeName.trim()
      const { objectType } = await define(sent.data.uploadId, trimmed, key(), columns(), completed.sha256)
      const connectionId = uploadConnectionId(objectType)
      const baseline = await readUploadConnection(connectionId)
      const requestKey = crypto.randomUUID()
      try {
        const loaded = await loadDataset(sent.data.uploadId, objectType, requestKey)
        return loaded === "no-engine" ? { kind: "no-engine", objectType } as const : { kind: "dataset", objectType, loaded } as const
      } catch (error) {
        if (isUncertainWriteError(error)) {
          const pending: UnknownLoad = {
            kind: "dataset",
            objectType,
            connectionId,
            sha256: completed.sha256,
            size: completed.size,
            baselineVersion: baseline?.version ?? 0,
            baselineEndedAt: baseline?.endedAt ?? null,
            phase: "checking",
            error,
          }
          void reconcile(pending)
          return null
        }
        throw error
      }
    },
    onSuccess: async (result) => {
      if (result === null) return
      setOutcome(result)
      await settle()
    },
  })
  const { locale } = useLocale()
  const { view } = useView()
  const asRecords = useMutation({
    mutationFn: async () => {
      if (!sent.data || !completed) throw new Error("Upload a file first.")
      const trimmed = typeName.trim()
      const objectType = `upload.${trimmed}`
      const connectionId = uploadConnectionId(trimmed)
      const baseline = await readUploadConnection(connectionId)
      try {
        const loaded = await loadRecords(sent.data.uploadId, trimmed, key(), columns(), completed.sha256)
        return { kind: "records", objectType: loaded.objectType, loaded } as const
      } catch (error) {
        if (isUncertainWriteError(error)) {
          const pending: UnknownLoad = {
            kind: "records",
            objectType,
            connectionId,
            sha256: completed.sha256,
            size: completed.size,
            baselineVersion: baseline?.version ?? 0,
            baselineEndedAt: baseline?.endedAt ?? null,
            phase: "checking",
            error,
          }
          void reconcile(pending)
          return null
        }
        throw error
      }
    },
    onSuccess: async (result) => {
      if (result === null) return
      setOutcome(result)
      await settle()
    },
  })
  const choose = (id: string, change: Partial<Choice>) => setChoices((current) => ({ ...current, [id]: { ...current[id] ?? { include: true, key: false, type: "text", measure: false }, ...change } }))
  const reset = () => { setFile(null); setTypeName(""); setChoices({}); setOutcome(null); setUnknownLoad(null); sent.reset(); asDataset.reset(); asRecords.reset() }
  const busy = asDataset.isPending || asRecords.isPending || unknownLoad !== null
  const problem = asDataset.error ?? asRecords.error
  const explainedUpload = sent.isError ? explainError(sent.error, locale, view) : null
  const explainedProblem = problem ? explainError(problem, locale, view) : null
  return (
    <Dialog open={open} onOpenChange={(next) => { setOpen(next); if (!next) reset() }}>
      <DialogTrigger asChild><Button type="button" size="sm" variant="outline"><Upload />Upload a CSV</Button></DialogTrigger>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Upload a CSV</DialogTitle>
          <DialogDescription>The cell stores the file and reads it back. You name the type, its key and which numbers add up; the cell admits the type and loads every row.</DialogDescription>
        </DialogHeader>
        {!completed && (
          <div className="space-y-3">
            <Input type="file" accept=".csv,text/csv" aria-label="CSV file" onChange={(event) => setFile(event.target.files?.[0] ?? null)} />
            <Button type="button" disabled={file === null || sent.isPending} onClick={() => file && sent.mutate(file)}><FileUp />{sent.isPending ? "Uploading…" : "Upload"}</Button>
            {explainedUpload && (
              <div role="alert" className="space-y-1 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                <p className="font-medium">{explainedUpload.headline}</p>
                {explainedUpload.detail && <p className="text-meta text-foreground">{explainedUpload.detail}</p>}
                {view === "technical" && explainedUpload.code && <p className="font-mono text-xs text-muted-foreground">{explainedUpload.code}</p>}
              </div>
            )}
          </div>
        )}
        {completed && outcome === null && unknownLoad !== null && (
          <div role="status" data-load-outcome="unknown" data-reconcile-state={unknownLoad.phase} className="space-y-4">
            <div className="space-y-2 rounded-lg border border-node-border bg-card p-4 text-sm">
              <div className="flex items-center gap-2 font-medium text-foreground">
                <Clock aria-hidden className="size-4 shrink-0 text-muted-foreground" />
                {unknownLoad.phase === "checking"
                  ? "Outcome unknown — checking whether the workspace finished loading…"
                  : "Outcome unknown — the workspace did not answer in time"}
              </div>
              <p className="text-meta text-muted-foreground">
                The load into <code className="font-mono">{unknownLoad.objectType}</code> may still be running or may have already finished. Re-submitting is blocked here so rows are not loaded twice.
              </p>
            </div>
            <DialogFooter className="flex-wrap gap-2">
              <Button asChild variant="outline"><Link to="/sources" onClick={() => setOpen(false)}>Open Sources</Link></Button>
              <Button type="button" disabled={unknownLoad.phase === "checking"} onClick={() => void reconcile(unknownLoad)}>
                <RefreshCw className={unknownLoad.phase === "checking" ? "animate-spin" : undefined} />
                {unknownLoad.phase === "checking" ? "Checking…" : "Check again"}
              </Button>
            </DialogFooter>
          </div>
        )}
        {completed && outcome === null && unknownLoad === null && (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground tabular-nums">{completed.rows} rows, {completed.columns.length} columns, {completed.size} bytes · sha256 <code className="font-mono text-xs">{completed.sha256.slice(0, 16)}…</code></p>
            <label className="block space-y-1 text-sm">
              <span className="font-medium">Type name</span>
              <Input value={typeName} placeholder="e.g. Sale" disabled={busy} onChange={(event) => setTypeName(event.target.value)} />
              <span className="block text-meta text-muted-foreground">One word starting with a capital letter; the cell refuses any other.</span>
            </label>
            <TableCard className="overflow-x-auto"><Table>
              <TableHeader><TableRow><TableHead>Keep</TableHead><TableHead>Column</TableHead><TableHead>Key</TableHead><TableHead>Kind</TableHead><TableHead>Adds up</TableHead><TableHead>Samples</TableHead></TableRow></TableHeader>
              <TableBody>
                {completed.columns.map((column) => {
                  const id = columnId(column)
                  const choice = choices[id] ?? { include: true, key: false, type: kindOf(column.inferred), measure: false }
                  return (
                    <TableRow key={id}>
                      <TableCell><input type="checkbox" aria-label={`Keep ${column.name}`} disabled={busy} checked={choice.include} onChange={(event) => choose(id, { include: event.target.checked })} /></TableCell>
                      <TableCell className="font-mono text-xs">{column.name}{column.nullable && <span className="ms-1 font-sans text-muted-foreground">(has empties)</span>}</TableCell>
                      <TableCell><input type="checkbox" aria-label={`${column.name} is part of the key`} disabled={busy} checked={choice.key} onChange={(event) => choose(id, { key: event.target.checked, ...event.target.checked ? { measure: false } : {} })} /></TableCell>
                      <TableCell>
                        <select aria-label={`Kind of ${column.name}`} disabled={busy} className="h-8 rounded-md border bg-background px-2 text-sm" value={choice.type} onChange={(event) => choose(id, { type: event.target.value as Kind })}>
                          {KINDS.map((kind) => <option key={kind} value={kind}>{kind}</option>)}
                        </select>
                      </TableCell>
                      <TableCell>
                        <label className="flex items-center gap-1 text-meta text-muted-foreground" title="A measure: questions may add its values up. Other columns describe a row.">
                          <input type="checkbox" aria-label={`${column.name} adds up`} disabled={busy || !numeric(choice.type) || choice.key} checked={numeric(choice.type) && !choice.key && choice.measure} onChange={(event) => choose(id, { measure: event.target.checked })} />
                          measure
                        </label>
                      </TableCell>
                      <TableCell className="max-w-56 truncate font-mono text-xs text-muted-foreground">{column.samples.map((sample) => JSON.stringify(sample)).join(", ")}</TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table></TableCard>
            {explainedProblem && (
              <div role="alert" className="space-y-1 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
                <p className="font-medium">{explainedProblem.headline}</p>
                {explainedProblem.detail && <p className="text-meta text-foreground">{explainedProblem.detail}</p>}
                {explainedProblem.incidentId && <p className="font-mono text-xs text-muted-foreground">Incident {explainedProblem.incidentId}</p>}
                {view === "technical" && explainedProblem.code && <p className="font-mono text-xs text-muted-foreground">{explainedProblem.code}</p>}
              </div>
            )}
            <DialogFooter className="flex-wrap gap-2">
              <Button type="button" variant="outline" onClick={reset} disabled={busy}>Start over</Button>
              <Button type="button" variant="outline" disabled={busy} onClick={() => asRecords.mutate()} title="Each row is written through the cell and checked against its laws, one at a time.">
                <Rows3 />{asRecords.isPending ? "Loading…" : "As governed records"}
              </Button>
              <Button type="button" disabled={busy} onClick={() => asDataset.mutate()} title="Every row goes into the cell's analytical engine at once; questions answer over it.">
                <Database />{asDataset.isPending ? "Loading…" : `Load ${completed.rows} rows as a dataset`}
              </Button>
            </DialogFooter>
          </div>
        )}
        {outcome !== null && (
          <div className="space-y-4">
            {outcome.kind === "dataset" && (
              <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
                <p className="font-medium">
                  {outcome.reconciled
                    ? `${outcome.loaded.rows} rows loaded into `
                    : `${outcome.loaded.rows} rows loaded into `}
                  <code className="font-mono">{outcome.objectType}</code>
                  {outcome.reconciled ? " (confirmed from the workspace)." : ` in ${outcome.loaded.millis} ms.`}
                </p>
                {outcome.loaded.rejected > 0
                  ? <p className="text-muted-foreground">{outcome.loaded.rejected} set aside: {Object.entries(outcome.loaded.reasons).map(([reason, count]) => `${count} ${reason}`).join(", ")}.</p>
                  : <p className="text-muted-foreground">No row was set aside.</p>}
              </div>
            )}
            {outcome.kind === "records" && (() => {
              const { rowsRead, rowsLanded, rowsQuarantined, quarantineReasons } = outcome.loaded
              const reasons = Object.entries(quarantineReasons ?? {}).map(([reason, count]) => `${count} ${reason}`).join(", ")
              const noneLanded = rowsLanded === 0 && (rowsQuarantined ?? 0) > 0
              const partial = (rowsLanded ?? 0) > 0 && (rowsQuarantined ?? 0) > 0
              return (
                <div role={noneLanded ? "alert" : "status"} className={`space-y-2 rounded-lg border p-4 text-sm ${noneLanded ? "border-destructive/40 bg-destructive/5" : ""}`}>
                  {noneLanded ? (
                    <>
                      <p className="font-medium text-destructive">No rows landed in <code className="font-mono">{outcome.objectType}</code>: {rowsQuarantined} of {rowsRead ?? rowsQuarantined} {rowsQuarantined === 1 ? "row was" : "rows were"} held back for review.</p>
                      {reasons && <p className="text-muted-foreground">Reasons: {reasons}.</p>}
                    </>
                  ) : partial ? (
                    <>
                      <p className="font-medium">{rowsLanded} of {rowsRead ?? ((rowsLanded ?? 0) + (rowsQuarantined ?? 0))} rows landed in <code className="font-mono">{outcome.objectType}</code>; {rowsQuarantined} held back for review.</p>
                      {reasons && <p className="text-muted-foreground">Reasons: {reasons}.</p>}
                    </>
                  ) : rowsLanded !== undefined ? (
                    <p className="font-medium">{rowsLanded} {rowsLanded === 1 ? "row was" : "rows were"} written as <code className="font-mono">{outcome.objectType}</code> records, each checked by the cell.</p>
                  ) : (
                    <p>The rows were sent to <code className="font-mono">{outcome.objectType}</code>.</p>
                  )}
                </div>
              )
            })()}
            {outcome.kind === "no-engine" && (
              <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
                <p>The cell admitted <code className="font-mono">{outcome.objectType}</code>, but it has no analytical engine to load the rows into.</p>
                <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => asRecords.mutate()}><Rows3 />Write them as governed records instead</Button>
              </div>
            )}
            <DialogFooter className="flex-wrap gap-2">
              <Button asChild variant="outline"><Link to="/objects/$type" params={{ type: outcome.objectType }} onClick={() => setOpen(false)}><Rows3 />Open the type</Link></Button>
              <Button asChild><Link to="/metrics" onClick={() => setOpen(false)}><ChartLine />See the metrics</Link></Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
