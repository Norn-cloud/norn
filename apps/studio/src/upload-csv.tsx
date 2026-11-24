import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { AlertTriangle, ChartLine, Database, FileUp, Rows3, Upload } from "lucide-react"
import { useRef, useState } from "react"
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
  AMBIGUOUS_LOAD_MESSAGE,
  KINDS,
  columnId,
  createSingleFlight,
  define,
  isAmbiguousLoadError,
  kindOf,
  loadDataset,
  loadRecords,
  upload,
  uploadSourceId,
  type Completed,
  type DatasetLoaded,
  type DefineColumn,
  type Kind,
  type RecordsLoaded,
} from "@/lib/uploads"

/** One column's choices: kept or left out, part of the key, its kind, and whether its values add up. */
export interface Choice { readonly include: boolean; readonly key: boolean; readonly type: Kind; readonly measure: boolean }

const numeric = (type: Kind) => type === "integer" || type === "decimal"

/** What loading came to: rows in the analytical engine, rows as governed records, a cell with no engine, or an ambiguous transport/gateway failure. */
type Outcome =
  | { readonly kind: "dataset"; readonly objectType: string; readonly loaded: DatasetLoaded }
  | { readonly kind: "records"; readonly objectType: string; readonly loaded: RecordsLoaded }
  | { readonly kind: "no-engine"; readonly objectType: string }
  | { readonly kind: "ambiguous"; readonly objectType: string; readonly connectionId: string | null; readonly error: unknown }

/** Honest notice for ambiguous CSV load failures (network error or 502/503/504): presentation only, no one-click retry. */
export function AmbiguousLoadNotice({
  objectType,
  connectionId,
  error,
  onOpenSources,
  onClose,
}: {
  readonly objectType: string
  readonly connectionId: string | null
  readonly error: unknown
  readonly onOpenSources?: () => void
  readonly onClose?: () => void
}) {
  const { locale } = useLocale()
  const { view } = useView()
  const explained = explainError(error, locale, view)
  const sourceLabel = connectionId ?? (objectType || null)
  return (
    <div role="alert" data-load-outcome="ambiguous" className="space-y-4">
      <div className="space-y-2 rounded-lg border border-warning/40 bg-warning/5 p-4 text-sm">
        <div className="flex items-center gap-2 font-medium text-foreground">
          <AlertTriangle aria-hidden className="size-4 shrink-0 text-warning" />
          Load status unknown
        </div>
        <p className="text-foreground">{AMBIGUOUS_LOAD_MESSAGE}</p>
        {sourceLabel && (
          <p className="text-meta text-muted-foreground">
            Source: <code className="font-mono">{sourceLabel}</code>
          </p>
        )}
        {explained.incidentId && (
          <p className="font-mono text-xs text-muted-foreground">Incident {explained.incidentId}</p>
        )}
        {view === "technical" && (
          <p className="font-mono text-xs text-muted-foreground">
            {[explained.status ? `HTTP ${explained.status}` : null, explained.code, explained.headline].filter(Boolean).join(" · ")}
          </p>
        )}
      </div>
      <DialogFooter className="flex-wrap gap-2">
        {onClose && <Button type="button" variant="outline" onClick={onClose}>Close</Button>}
        <Button asChild>
          <Link
            to="/sources"
            {...(connectionId ? { hash: connectionId } : {})}
            onClick={onOpenSources}
          >
            {connectionId ? `Check ${connectionId} in Sources` : "Open Sources"}
          </Link>
        </Button>
      </DialogFooter>
    </div>
  )
}

function RefusalAlert({ error }: { readonly error: unknown }) {
  const { locale } = useLocale()
  const { view } = useView()
  const explained = explainError(error, locale, view)
  return (
    <div role="alert" className="space-y-1 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
      <p className="font-medium">{explained.headline}</p>
      {explained.detail && <p className="text-meta text-foreground">{explained.detail}</p>}
      {explained.incidentId && <p className="font-mono text-xs text-muted-foreground">Incident {explained.incidentId}</p>}
      {view === "technical" && explained.code && <p className="font-mono text-xs text-muted-foreground">{explained.code}</p>}
    </div>
  )
}

/** Configure columns and submit a staged CSV; blocks duplicate submission while a load is in flight and keeps retry for clear refusals. */
export function UploadCsvConfigurePanel({
  completed,
  typeName,
  choices,
  pending,
  problem,
  onTypeNameChange,
  onChoose,
  onReset,
  onSubmitRecords,
  onSubmitDataset,
}: {
  readonly completed: Completed
  readonly typeName: string
  readonly choices: Readonly<Record<string, Choice>>
  readonly pending: "dataset" | "records" | null
  readonly problem: unknown
  readonly onTypeNameChange: (value: string) => void
  readonly onChoose: (id: string, change: Partial<Choice>) => void
  readonly onReset: () => void
  readonly onSubmitRecords: () => void
  readonly onSubmitDataset: () => void
}) {
  const busy = pending !== null
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground tabular-nums">{completed.rows} rows, {completed.columns.length} columns, {completed.size} bytes · sha256 <code className="font-mono text-xs">{completed.sha256.slice(0, 16)}…</code></p>
      <label className="block space-y-1 text-sm">
        <span className="font-medium">Type name</span>
        <Input value={typeName} placeholder="e.g. Sale" disabled={busy} onChange={(event) => onTypeNameChange(event.target.value)} />
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
                <TableCell><input type="checkbox" aria-label={`Keep ${column.name}`} disabled={busy} checked={choice.include} onChange={(event) => onChoose(id, { include: event.target.checked })} /></TableCell>
                <TableCell className="font-mono text-xs">{column.name}{column.nullable && <span className="ms-1 font-sans text-muted-foreground">(has empties)</span>}</TableCell>
                <TableCell><input type="checkbox" aria-label={`${column.name} is part of the key`} disabled={busy} checked={choice.key} onChange={(event) => onChoose(id, { key: event.target.checked, ...event.target.checked ? { measure: false } : {} })} /></TableCell>
                <TableCell>
                  <select aria-label={`Kind of ${column.name}`} disabled={busy} className="h-8 rounded-md border bg-background px-2 text-sm" value={choice.type} onChange={(event) => onChoose(id, { type: event.target.value as Kind })}>
                    {KINDS.map((kind) => <option key={kind} value={kind}>{kind}</option>)}
                  </select>
                </TableCell>
                <TableCell>
                  <label className="flex items-center gap-1 text-meta text-muted-foreground" title="A measure: questions may add its values up. Other columns describe a row.">
                    <input type="checkbox" aria-label={`${column.name} adds up`} disabled={busy || !numeric(choice.type) || choice.key} checked={numeric(choice.type) && !choice.key && choice.measure} onChange={(event) => onChoose(id, { measure: event.target.checked })} />
                    measure
                  </label>
                </TableCell>
                <TableCell className="max-w-56 truncate font-mono text-xs text-muted-foreground">{column.samples.map((sample) => JSON.stringify(sample)).join(", ")}</TableCell>
              </TableRow>
            )
          })}
        </TableBody>
      </Table></TableCard>
      {problem !== null && problem !== undefined && <RefusalAlert error={problem} />}
      <DialogFooter className="flex-wrap gap-2">
        <Button type="button" variant="outline" onClick={onReset} disabled={busy}>Start over</Button>
        <Button type="button" variant="outline" disabled={busy} onClick={onSubmitRecords} title="Each row is written through the cell and checked against its laws, one at a time.">
          <Rows3 />{pending === "records" ? "Loading…" : "As governed records"}
        </Button>
        <Button type="button" disabled={busy} onClick={onSubmitDataset} title="Every row goes into the cell's analytical engine at once; questions answer over it.">
          <Database />{pending === "dataset" ? "Loading…" : `Load ${completed.rows} rows as a dataset`}
        </Button>
      </DialogFooter>
    </div>
  )
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
  const generationRef = useRef(0)
  const flightRef = useRef(createSingleFlight())
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

  const asDataset = useMutation({
    mutationFn: async () => {
      if (!sent.data || !completed) throw new Error("Upload a file first.")
      const generation = generationRef.current
      const trimmed = typeName.trim()
      const { objectType } = await define(sent.data.uploadId, trimmed, key(), columns(), completed.sha256)
      try {
        const loaded = await loadDataset(sent.data.uploadId, objectType)
        const result: Outcome = loaded === "no-engine" ? { kind: "no-engine", objectType } : { kind: "dataset", objectType, loaded }
        return { generation, result }
      } catch (error) {
        if (isAmbiguousLoadError(error)) {
          return {
            generation,
            result: { kind: "ambiguous", objectType, connectionId: uploadSourceId(objectType), error } as const,
          }
        }
        throw error
      }
    },
    onSuccess: async ({ generation, result }) => {
      if (result.kind !== "ambiguous") await settle()
      if (generation !== generationRef.current) return
      setOutcome(result)
    },
  })
  const asRecords = useMutation({
    mutationFn: async () => {
      if (!sent.data || !completed) throw new Error("Upload a file first.")
      const generation = generationRef.current
      const trimmed = typeName.trim()
      const objectType = trimmed === "" ? "" : `upload.${trimmed}`
      try {
        const loaded = await loadRecords(sent.data.uploadId, trimmed, key(), columns(), completed.sha256)
        return { generation, result: { kind: "records", objectType: loaded.objectType, loaded } as const }
      } catch (error) {
        if (isAmbiguousLoadError(error)) {
          return {
            generation,
            result: {
              kind: "ambiguous",
              objectType,
              connectionId: trimmed === "" ? null : uploadSourceId(trimmed),
              error,
            } as const,
          }
        }
        throw error
      }
    },
    onSuccess: async ({ generation, result }) => {
      if (result.kind !== "ambiguous") await settle()
      if (generation !== generationRef.current) return
      setOutcome(result)
    },
  })
  const choose = (id: string, change: Partial<Choice>) => setChoices((current) => ({ ...current, [id]: { ...current[id] ?? { include: true, key: false, type: "text", measure: false }, ...change } }))
  const reset = () => {
    generationRef.current += 1
    flightRef.current = createSingleFlight()
    setFile(null); setTypeName(""); setChoices({}); setOutcome(null)
    sent.reset(); asDataset.reset(); asRecords.reset()
  }
  const close = () => { setOpen(false); reset() }
  const pending: "dataset" | "records" | null = asDataset.isPending ? "dataset" : asRecords.isPending ? "records" : null
  const busy = pending !== null
  const submitDataset = () => {
    if (busy || flightRef.current.inFlight) return
    asRecords.reset()
    void flightRef.current.run(() => asDataset.mutateAsync())?.catch(() => {})
  }
  const submitRecords = () => {
    if (busy || flightRef.current.inFlight) return
    asDataset.reset()
    void flightRef.current.run(() => asRecords.mutateAsync())?.catch(() => {})
  }
  const problem = asDataset.error ?? asRecords.error
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
            {sent.isError && <RefusalAlert error={sent.error} />}
          </div>
        )}
        {completed && outcome === null && (
          <UploadCsvConfigurePanel
            completed={completed}
            typeName={typeName}
            choices={choices}
            pending={pending}
            problem={problem}
            onTypeNameChange={setTypeName}
            onChoose={choose}
            onReset={reset}
            onSubmitRecords={submitRecords}
            onSubmitDataset={submitDataset}
          />
        )}
        {outcome !== null && outcome.kind === "ambiguous" && (
          <AmbiguousLoadNotice
            objectType={outcome.objectType}
            connectionId={outcome.connectionId}
            error={outcome.error}
            onOpenSources={() => { close(); void settle() }}
            onClose={close}
          />
        )}
        {outcome !== null && outcome.kind !== "ambiguous" && (
          <div className="space-y-4">
            {outcome.kind === "dataset" && (
              <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
                <p className="font-medium">{outcome.loaded.rows} rows loaded into <code className="font-mono">{outcome.objectType}</code> in {outcome.loaded.millis} ms.</p>
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
                {asRecords.isError && <RefusalAlert error={asRecords.error} />}
                <Button type="button" variant="outline" size="sm" disabled={busy} onClick={submitRecords}><Rows3 />{pending === "records" ? "Loading…" : "Write them as governed records instead"}</Button>
              </div>
            )}
            <DialogFooter className="flex-wrap gap-2">
              <Button asChild variant="outline"><Link to="/objects/$type" params={{ type: outcome.objectType }} onClick={close}><Rows3 />Open the type</Link></Button>
              <Button asChild><Link to="/metrics" onClick={close}><ChartLine />See the metrics</Link></Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
