import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Cable, RefreshCw } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { EmptyState, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import type { Read } from "@/lib/exact"
import { CellRefusal, NotServed, cellRead } from "@/lib/live-transport"
import { systemName } from "./lib/catalog.ts"
import { watermarkText } from "./lib/format.ts"
import { UploadCsv } from "./upload-csv.tsx"

/** The platform sources pack's connection type: one record per connected system or upload. */
const CONNECTION_TYPE = "source.Connection"

interface Connection {
  readonly id: string
  readonly values: {
    readonly system?: string; readonly label?: string; readonly target?: string; readonly status?: string
    readonly last_sync_started_at?: string | null; readonly last_sync_ended_at?: string | null; readonly watermark?: string | null
    readonly rows_read?: number | null; readonly rows_landed?: number | null; readonly rows_quarantined?: number | null
    readonly last_error_code?: string | null; readonly last_error_message?: string | null
    readonly quarantine_reasons?: string | null
  }
}

interface Page { readonly records: readonly Connection[] }

const STATUS: Record<string, { readonly label: string; readonly variant: "secondary" | "outline" | "destructive" }> = {
  ok: { label: "Synced", variant: "secondary" },
  syncing: { label: "Syncing", variant: "outline" },
  partial: { label: "Partly synced", variant: "outline" },
  failed: { label: "Failed", variant: "destructive" },
  never: { label: "Not synced yet", variant: "outline" },
}

/** An instant the cell sent, in the reader's own words; the exact text stays on hover. */
function Instant({ iso }: { readonly iso: string | null | undefined }) {
  if (!iso) return <>—</>
  const at = new Date(iso)
  return Number.isNaN(at.getTime()) ? <>{iso}</> : <time dateTime={iso} title={iso}>{at.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}</time>
}

/** A count as the cell sent it, by its exact text. */
const count = (read: Read<Page>, index: number, key: string, value: unknown) =>
  value === null || value === undefined ? "—" : read.numbers.get(`/records/${index}/values/${key}`) ?? String(value)

function SyncNow({ id }: { readonly id: string }) {
  const client = useQueryClient()
  const sync = useMutation({
    mutationFn: () => cellRead<unknown>(`/v1/sources/${encodeURIComponent(id)}:sync`, "Syncing is", { method: "POST", body: "{}" }),
    onSettled: () => client.invalidateQueries({ queryKey: ["source-connections"] }),
  })
  return (
    <div className="space-y-1">
      <Button type="button" size="sm" variant="outline" disabled={sync.isPending} onClick={() => sync.mutate()}><RefreshCw className={sync.isPending ? "animate-spin" : undefined} />Sync now</Button>
      {sync.isError && <p role="alert" className="max-w-48 text-xs text-destructive">{sync.error.message}</p>}
    </div>
  )
}

/**
 * The cell's source connections, read like any other records (`source.Connection`, one per
 * connected system or upload): what each reads, when it last synced, how far, and what landed
 * or was held back. Sync now asks the cell; the cell decides and reports the run.
 */
/** How long the connections read may take before the panel says the cell did not answer. */
const READ_DEADLINE_MS = 20_000

/** What a failed read says: the deadline in words, or the cell's own refusal. */
const failure = (error: Error) => error.name === "TimeoutError" || error.name === "AbortError"
  ? `The cell did not answer within ${READ_DEADLINE_MS / 1000} seconds.`
  : error.message

export function SourceConnections() {
  const connections = useQuery({
    queryKey: ["source-connections"],
    queryFn: () => cellRead<Page>(`/v1/objects/${CONNECTION_TYPE}?limit=200`, "Source connections are", { signal: AbortSignal.timeout(READ_DEADLINE_MS) }),
    retry: false,
    // While a sync runs, read again until it lands.
    refetchInterval: (query) => query.state.data?.document.records.some(({ values }) => values.status === "syncing") ? 3000 : false,
  })
  const notInstalled = connections.error instanceof CellRefusal && connections.error.status === 404
  return (
    <Panel icon={Cable} label="Connections" action={<UploadCsv />}>
      {connections.isPending && <p className="px-4 py-3 text-sm text-muted-foreground">Reading connections…</p>}
      {connections.isError && (connections.error instanceof NotServed || notInstalled
        ? <Well className="flex items-center gap-3 px-4 py-3 text-sm"><span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-border bg-surface-subtle"><Cable aria-hidden className="size-4 text-muted-foreground" /></span><span className="min-w-0"><span className="block font-medium">No connections</span><span className="block text-meta text-muted-foreground">{notInstalled ? "This cell has no source connections installed." : connections.error.message}</span></span></Well>
        : (
          <div className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
            <p role="alert" className="text-sm text-destructive">The connections could not be read. {failure(connections.error)}</p>
            <Button type="button" size="sm" variant="outline" disabled={connections.isFetching} onClick={() => void connections.refetch()}><RefreshCw />Read again</Button>
          </div>
        ))}
      {connections.data && (connections.data.document.records.length === 0
        ? <Well><EmptyState icon={Cable} title="No connections" next={<UploadCsv />}>No system is connected to this cell yet. Upload a CSV to start, or ask an admin to connect a system.</EmptyState></Well>
        : <div className="p-1"><TableCard><Table>
          <TableHeader><TableRow>
            <TableHead>Connection</TableHead><TableHead>Status</TableHead><TableHead>Last sync</TableHead><TableHead>Up to</TableHead>
            <TableHead className="text-right">Read</TableHead><TableHead className="text-right">Landed</TableHead><TableHead className="text-right">Quarantined</TableHead><TableHead />
          </TableRow></TableHeader>
          <TableBody>
            {connections.data.document.records.map(({ id, values }, index) => {
              const status = STATUS[values.status ?? ""] ?? { label: values.status ?? "—", variant: "outline" as const }
              const read = connections.data
              return (
                <TableRow key={id}>
                  <TableCell><div className="font-medium">{values.label ?? id}</div><div className="text-xs text-muted-foreground">{systemName(values.system ?? "")} · <span className="font-mono">{values.target}</span></div></TableCell>
                  <TableCell>
                    <Badge variant={status.variant}>{status.label}</Badge>
                    {values.last_error_code && <div className="max-w-64 pt-1 text-xs whitespace-normal text-destructive" title={values.last_error_message ?? undefined}><span className="font-mono">{values.last_error_code}</span>{values.last_error_message ? `: ${values.last_error_message}` : ""}</div>}
                  </TableCell>
                  <TableCell className="text-sm whitespace-nowrap">{values.status === "syncing" ? <>started <Instant iso={values.last_sync_started_at} /></> : <Instant iso={values.last_sync_ended_at} />}</TableCell>
                  <TableCell className="max-w-48 truncate font-mono text-xs" title={values.watermark ?? "The source's own modified-since mark after the last sync"}>{watermarkText(values.watermark)}</TableCell>
                  <TableCell className="text-right tabular-nums">{count(read, index, "rows_read", values.rows_read)}</TableCell>
                  <TableCell className="text-right tabular-nums">{count(read, index, "rows_landed", values.rows_landed)}</TableCell>
                  <TableCell className="text-right tabular-nums" title={values.quarantine_reasons ?? undefined}>{count(read, index, "rows_quarantined", values.rows_quarantined)}{values.quarantine_reasons ? <div className="font-mono text-xs text-muted-foreground">{values.quarantine_reasons}</div> : null}</TableCell>
                  <TableCell>{values.system !== "upload" && <SyncNow id={id} />}</TableCell>
                </TableRow>
              )
            })}
          </TableBody>
        </Table></TableCard></div>)}
    </Panel>
  )
}
