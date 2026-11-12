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
import { UploadCsv } from "./upload-csv.tsx"

/** The platform sources pack's connection type: one record per connected system or upload. */
const CONNECTION_TYPE = "source.Connection"

interface Connection {
  readonly id: string
  readonly values: {
    readonly system?: string; readonly label?: string; readonly target?: string; readonly status?: string
    readonly lastSyncStartedAt?: string | null; readonly lastSyncEndedAt?: string | null; readonly watermark?: string | null
    readonly rowsRead?: number | null; readonly rowsLanded?: number | null; readonly rowsQuarantined?: number | null
    readonly lastErrorCode?: string | null; readonly lastErrorMessage?: string | null
  }
}

interface Page { readonly records: readonly Connection[] }

const STATUS: Record<string, { readonly label: string; readonly variant: "secondary" | "outline" | "destructive" }> = {
  ok: { label: "Synced", variant: "secondary" },
  syncing: { label: "Syncing", variant: "outline" },
  failed: { label: "Failed", variant: "destructive" },
  never: { label: "Not synced yet", variant: "outline" },
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
export function SourceConnections() {
  const connections = useQuery({
    queryKey: ["source-connections"],
    queryFn: () => cellRead<Page>(`/v1/objects/${CONNECTION_TYPE}?limit=200`, "Source connections are"),
    retry: false,
    // While a sync runs, read again until it lands.
    refetchInterval: (query) => query.state.data?.document.records.some(({ values }) => values.status === "syncing") ? 3000 : false,
  })
  const notInstalled = connections.error instanceof CellRefusal && connections.error.status === 404
  return (
    <Panel icon={Cable} label="Connections" action={<UploadCsv />}>
      {connections.isPending && <p className="px-4 py-3 text-sm text-muted-foreground">Reading connections…</p>}
      {connections.isError && (connections.error instanceof NotServed || notInstalled
        ? <Well><EmptyState icon={Cable} title="No connections">{notInstalled ? "This cell has no source connections installed." : connections.error.message}</EmptyState></Well>
        : <p role="alert" className="px-4 py-3 text-sm text-destructive">{connections.error.message}</p>)}
      {connections.data && (connections.data.document.records.length === 0
        ? <Well><EmptyState icon={Cable} title="No connections">No system is connected to this cell yet.</EmptyState></Well>
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
                    {values.lastErrorCode && <div className="max-w-56 pt-1 text-xs text-destructive" title={values.lastErrorMessage ?? undefined}><span className="font-mono">{values.lastErrorCode}</span>{values.lastErrorMessage ? `: ${values.lastErrorMessage}` : ""}</div>}
                  </TableCell>
                  <TableCell className="text-sm">{values.status === "syncing" ? <>started {values.lastSyncStartedAt}</> : values.lastSyncEndedAt ?? "—"}</TableCell>
                  <TableCell className="font-mono text-xs" title="The source's own modified-since mark after the last sync">{values.watermark ?? "—"}</TableCell>
                  <TableCell className="text-right tabular-nums">{count(read, index, "rowsRead", values.rowsRead)}</TableCell>
                  <TableCell className="text-right tabular-nums">{count(read, index, "rowsLanded", values.rowsLanded)}</TableCell>
                  <TableCell className="text-right tabular-nums">{count(read, index, "rowsQuarantined", values.rowsQuarantined)}</TableCell>
                  <TableCell>{values.system !== "upload" && <SyncNow id={id} />}</TableCell>
                </TableRow>
              )
            })}
          </TableBody>
        </Table></TableCard></div>)}
    </Panel>
  )
}
