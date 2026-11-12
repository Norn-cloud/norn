import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { ChartLine, Database, FileUp, Rows3, Upload } from "lucide-react"
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { refreshMode } from "@/lib/live-transport"
import { KINDS, columnId, define, kindOf, loadDataset, loadRecords, upload, type Completed, type DatasetLoaded, type DefineColumn, type Kind } from "@/lib/uploads"

/** One column's choices: kept or left out, part of the key, its kind, and whether its values add up. */
interface Choice { readonly include: boolean; readonly key: boolean; readonly type: Kind; readonly measure: boolean }

const numeric = (type: Kind) => type === "integer" || type === "decimal"

/** What loading came to: rows in the analytical engine, rows as governed records, or a cell with no engine. */
type Outcome =
  | { readonly kind: "dataset"; readonly objectType: string; readonly loaded: DatasetLoaded }
  | { readonly kind: "records"; readonly objectType: string }
  | { readonly kind: "no-engine"; readonly objectType: string }

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
      const { objectType } = await define(sent.data.uploadId, typeName.trim(), key(), columns(), completed.sha256)
      const loaded = await loadDataset(sent.data.uploadId, objectType)
      return loaded === "no-engine" ? { kind: "no-engine", objectType } as const : { kind: "dataset", objectType, loaded } as const
    },
    onSuccess: async (result) => { setOutcome(result); await settle() },
  })
  const asRecords = useMutation({
    mutationFn: async () => {
      if (!sent.data || !completed) throw new Error("Upload a file first.")
      const { objectType } = await loadRecords(sent.data.uploadId, typeName.trim(), key(), columns(), completed.sha256)
      return { kind: "records", objectType } as const
    },
    onSuccess: async (result) => { setOutcome(result); await settle() },
  })
  const choose = (id: string, change: Partial<Choice>) => setChoices((current) => ({ ...current, [id]: { ...current[id] ?? { include: true, key: false, type: "text", measure: false }, ...change } }))
  const reset = () => { setFile(null); setTypeName(""); setChoices({}); setOutcome(null); sent.reset(); asDataset.reset(); asRecords.reset() }
  const busy = asDataset.isPending || asRecords.isPending
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
            {sent.isError && <p role="alert" className="text-sm text-destructive">{sent.error.message}</p>}
          </div>
        )}
        {completed && outcome === null && (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground tabular-nums">{completed.rows} rows, {completed.columns.length} columns, {completed.size} bytes · sha256 <code className="font-mono text-xs">{completed.sha256.slice(0, 16)}…</code></p>
            <label className="block space-y-1 text-sm">
              <span className="font-medium">Type name</span>
              <Input value={typeName} placeholder="e.g. Sale" onChange={(event) => setTypeName(event.target.value)} />
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
                      <TableCell><input type="checkbox" aria-label={`Keep ${column.name}`} checked={choice.include} onChange={(event) => choose(id, { include: event.target.checked })} /></TableCell>
                      <TableCell className="font-mono text-xs">{column.name}{column.nullable && <span className="ms-1 font-sans text-muted-foreground">(has empties)</span>}</TableCell>
                      <TableCell><input type="checkbox" aria-label={`${column.name} is part of the key`} checked={choice.key} onChange={(event) => choose(id, { key: event.target.checked, ...event.target.checked ? { measure: false } : {} })} /></TableCell>
                      <TableCell>
                        <select aria-label={`Kind of ${column.name}`} className="h-8 rounded-md border bg-background px-2 text-sm" value={choice.type} onChange={(event) => choose(id, { type: event.target.value as Kind })}>
                          {KINDS.map((kind) => <option key={kind} value={kind}>{kind}</option>)}
                        </select>
                      </TableCell>
                      <TableCell>
                        <label className="flex items-center gap-1 text-meta text-muted-foreground" title="A measure: questions may add its values up. Other columns describe a row.">
                          <input type="checkbox" aria-label={`${column.name} adds up`} disabled={!numeric(choice.type) || choice.key} checked={numeric(choice.type) && !choice.key && choice.measure} onChange={(event) => choose(id, { measure: event.target.checked })} />
                          measure
                        </label>
                      </TableCell>
                      <TableCell className="max-w-56 truncate font-mono text-xs text-muted-foreground">{column.samples.map((sample) => JSON.stringify(sample)).join(", ")}</TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table></TableCard>
            {problem && <p role="alert" className="text-sm text-destructive">{problem.message}</p>}
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
                <p className="font-medium">{outcome.loaded.rows} rows loaded into <code className="font-mono">{outcome.objectType}</code> in {outcome.loaded.millis} ms.</p>
                {outcome.loaded.rejected > 0
                  ? <p className="text-muted-foreground">{outcome.loaded.rejected} set aside: {Object.entries(outcome.loaded.reasons).map(([reason, count]) => `${count} ${reason}`).join(", ")}.</p>
                  : <p className="text-muted-foreground">No row was set aside.</p>}
              </div>
            )}
            {outcome.kind === "records" && <p role="status" className="rounded-lg border p-4 text-sm">The rows were written as <code className="font-mono">{outcome.objectType}</code> records, each checked by the cell.</p>}
            {outcome.kind === "no-engine" && (
              <div role="status" className="space-y-2 rounded-lg border p-4 text-sm">
                <p>The cell admitted <code className="font-mono">{outcome.objectType}</code>, but it has no analytical engine to load the rows into.</p>
                <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => asRecords.mutate()}><Rows3 />Write them as governed records instead</Button>
              </div>
            )}
            <DialogFooter className="flex-wrap gap-2">
              <Button asChild variant="outline"><Link to="/objects/$type" params={{ type: outcome.objectType }} onClick={() => setOpen(false)}><Rows3 />Open the type</Link></Button>
              <Button asChild><Link to="/questions" onClick={() => setOpen(false)}><ChartLine />Ask a question</Link></Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
