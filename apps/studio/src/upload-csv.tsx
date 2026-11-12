import { useMutation, useQueryClient } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { FileUp, Upload } from "lucide-react"
import { useState } from "react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { refreshMode } from "@/lib/live-transport"
import { load, upload, type Completed, type Inferred, type LoadColumn } from "@/lib/uploads"

const TYPES: readonly Inferred[] = ["string", "decimal", "integer", "boolean", "date", "datetime"]

/** One column's choices: kept or left out, and the kind of value it holds. */
interface Choice { readonly include: boolean; readonly type: Inferred; readonly key: boolean }

/**
 * Upload a CSV into the cell: the file goes to storage under a URL the cell signs, the cell reads
 * it back and proposes each column's kind, and loading asks the cell to admit a type for it and
 * load every row through its mapping path. The cell checks every choice; nothing is checked here.
 */
export function UploadCsv() {
  const [open, setOpen] = useState(false)
  const [file, setFile] = useState<File | null>(null)
  const [typeName, setTypeName] = useState("")
  const [choices, setChoices] = useState<Readonly<Record<string, Choice>>>({})
  const client = useQueryClient()
  const navigate = useNavigate()
  const sent = useMutation({
    mutationFn: (chosen: File) => upload(chosen),
    onSuccess: ({ completed }) => setChoices(Object.fromEntries(completed.columns.map(({ name, inferred }, index) => [name, { include: true, type: inferred, key: index === 0 }]))),
  })
  const loaded = useMutation({
    mutationFn: ({ uploadId, columns }: { readonly uploadId: string; readonly columns: readonly LoadColumn[] }) =>
      load(uploadId, typeName.trim(), Object.entries(choices).filter(([, { key }]) => key).map(([name]) => name), columns),
    onSuccess: async ({ objectType }) => {
      // The load installed a new revision: read the catalog again before opening the new type.
      refreshMode()
      await client.invalidateQueries()
      setOpen(false)
      await navigate({ to: "/objects/$type", params: { type: objectType } })
    },
  })
  const completed: Completed | undefined = sent.data?.completed
  const choose = (name: string, change: Partial<Choice>) => setChoices((current) => ({ ...current, [name]: { ...current[name] ?? { include: true, type: "string", key: false }, ...change } }))
  const reset = () => { setFile(null); setTypeName(""); setChoices({}); sent.reset(); loaded.reset() }
  return (
    <Dialog open={open} onOpenChange={(next) => { setOpen(next); if (!next) reset() }}>
      <DialogTrigger asChild><Button type="button" size="sm" variant="outline"><Upload />Upload a CSV</Button></DialogTrigger>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Upload a CSV</DialogTitle>
          <DialogDescription>The cell stores the file, reads it back and proposes a type for its rows. You name the type and its key; the cell checks it before any row lands.</DialogDescription>
        </DialogHeader>
        {!completed && (
          <div className="space-y-3">
            <Input type="file" accept=".csv,text/csv" aria-label="CSV file" onChange={(event) => setFile(event.target.files?.[0] ?? null)} />
            <Button type="button" disabled={file === null || sent.isPending} onClick={() => file && sent.mutate(file)}><FileUp />{sent.isPending ? "Uploading…" : "Upload"}</Button>
            {sent.isError && <p role="alert" className="text-sm text-destructive">{sent.error.message}</p>}
          </div>
        )}
        {completed && sent.data && (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground tabular-nums">{completed.rows} rows, {completed.columns.length} columns, {completed.size} bytes · sha256 <code className="font-mono text-xs">{completed.sha256.slice(0, 16)}…</code></p>
            <label className="block space-y-1 text-sm">
              <span className="font-medium">Type name</span>
              <Input value={typeName} placeholder="e.g. upload.Sale" onChange={(event) => setTypeName(event.target.value)} />
            </label>
            <TableCard><Table>
              <TableHeader><TableRow><TableHead>Keep</TableHead><TableHead>Column</TableHead><TableHead>Key</TableHead><TableHead>Kind</TableHead><TableHead>Samples</TableHead></TableRow></TableHeader>
              <TableBody>
                {completed.columns.map(({ name, nullable, samples }) => {
                  const choice = choices[name]
                  return (
                    <TableRow key={name}>
                      <TableCell><input type="checkbox" aria-label={`Keep ${name}`} checked={choice?.include ?? true} onChange={(event) => choose(name, { include: event.target.checked })} /></TableCell>
                      <TableCell className="font-mono text-xs">{name}{nullable && <span className="ms-1 font-sans text-muted-foreground">(has empties)</span>}</TableCell>
                      <TableCell><input type="checkbox" aria-label={`${name} is part of the key`} checked={choice?.key ?? false} onChange={(event) => choose(name, { key: event.target.checked })} /></TableCell>
                      <TableCell>
                        <select aria-label={`Kind of ${name}`} className="h-8 rounded-md border bg-background px-2 text-sm" value={choice?.type ?? "string"} onChange={(event) => choose(name, { type: event.target.value as Inferred })}>
                          {TYPES.map((type) => <option key={type} value={type}>{type}</option>)}
                        </select>
                      </TableCell>
                      <TableCell className="max-w-56 truncate font-mono text-xs text-muted-foreground">{samples.map((sample) => JSON.stringify(sample)).join(", ")}</TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table></TableCard>
            {loaded.isError && <p role="alert" className="text-sm text-destructive">{loaded.error.message}</p>}
            <DialogFooter>
              <Button type="button" variant="outline" onClick={reset}>Start over</Button>
              <Button type="button" disabled={loaded.isPending} onClick={() => loaded.mutate({ uploadId: sent.data.uploadId, columns: completed.columns.map(({ name }) => ({ name, type: choices[name]?.type ?? "string", include: choices[name]?.include ?? true })) })}>
                {loaded.isPending ? "Loading…" : `Load ${completed.rows} rows`}
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
