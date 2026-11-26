import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Cable, RefreshCw } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { TableCard } from "@/components/system/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import type { Read } from "@/lib/exact"
import { CellRefusal, NotServed, cellRead } from "@/lib/live-transport"
import { transport, type Artifact } from "@/lib/transport"
import { studioScopeKey, useStudioScope } from "./lib/definitions.ts"
import { systemName } from "./lib/catalog.ts"
import { connectionStatus, watermarkText } from "./lib/format.ts"
import { usePinnedScope } from "./lib/session.ts"
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
  const pinnedScope = usePinnedScope(id)
  const sync = useMutation({
    mutationFn: () => cellRead<unknown>(`/v1/sources/${encodeURIComponent(id)}:sync`, "Syncing is", { method: "POST", body: "{}" }, undefined, pinnedScope),
    onSettled: () => client.invalidateQueries({ queryKey: ["source-connections"] }),
  })
  return (
    <div className="space-y-1">
      <Button type="button" size="sm" variant="outline" disabled={sync.isPending} onClick={() => sync.mutate()}><RefreshCw className={sync.isPending ? "animate-spin motion-reduce:animate-none" : undefined} />Sync now</Button>
      {sync.isError && <p role="alert" className="max-w-48 text-meta text-destructive-text">{sync.error.message}</p>}
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

/** Whether the cell's admitted definitions (the catalog Studio already reads) declare the connection type. */
export const declaresConnections = (artifacts: readonly Artifact[]): boolean =>
  artifacts.some((artifact) => artifact.kind === "entity" && "objectType" in artifact.payload && artifact.payload.objectType.name === CONNECTION_TYPE)

export function SourceConnections() {
  const scope = useStudioScope()
  const boundScope = scope.data ?? null
  const scopeKey = studioScopeKey(boundScope)
  // The cell installs its sources pack (and so this type) with the first connection or upload: until then
  // there is nothing to read, and the panel says so instead of asking for a type the cell does not have.
  const catalog = useQuery({ queryKey: ["catalog", scopeKey], queryFn: transport.catalog, enabled: scope.isSuccess })
  const installed = catalog.data === undefined ? undefined : declaresConnections(catalog.data.document)
  const connections = useQuery({
    enabled: scope.isSuccess && installed === true,
    queryKey: ["source-connections", scopeKey],
    queryFn: ({ signal }) => cellRead<Page>(
      `/v1/objects/${CONNECTION_TYPE}?limit=200`,
      "Source connections are",
      { signal: AbortSignal.any([signal, AbortSignal.timeout(READ_DEADLINE_MS)]) },
      undefined,
      boundScope,
    ),
    retry: false,
    // While a sync runs, read again until it lands.
    refetchInterval: (query) => query.state.data?.document.records.some(({ values }) => values.status === "syncing") ? 3000 : false,
  })
  const notInstalled = connections.error instanceof CellRefusal && connections.error.status === 404
  const refused = connections.error instanceof CellRefusal
  return (
    <Panel icon={Cable} label="Connections" action={<UploadCsv />}>
      {installed === false && <Well><EmptyState icon={Cable} title="No connections yet" next={<UploadCsv />}>No system is connected to this cell yet. Upload a CSV to start, or ask an admin to connect a system.</EmptyState></Well>}
      {catalog.isError && (
        <div role="status" data-connections="unread" className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
          <p className="min-w-0 text-body"><span>Couldn't check the cell's catalog.</span> <span className="text-muted-foreground">{failure(catalog.error)}</span></p>
          <Button type="button" size="sm" variant="outline" disabled={catalog.isFetching} onClick={() => void catalog.refetch()}><RefreshCw />Read again</Button>
        </div>
      )}
      {!catalog.isError && (scope.isPending || installed === undefined || (installed && connections.isPending)) && <p className="px-4 py-3 text-body text-muted-foreground">Reading connections…</p>}
      {connections.isError && (connections.error instanceof NotServed || notInstalled
        ? <Well className="flex items-center gap-3 px-4 py-3 text-body"><span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-border bg-surface-subtle"><Cable aria-hidden className="size-4 text-muted-foreground" /></span><span className="min-w-0"><span className="block">No connections</span><span className="block text-meta text-muted-foreground">{notInstalled ? "This cell has no source connections installed." : connections.error.message}</span></span></Well>
        : (
          // Refusals are never retried (DESIGN.md 12.3); transient failures offer Read again.
          <div role="status" data-connections="unread" className="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
            <p className="min-w-0 text-body"><span>{refused ? "The cell refused to read the connections." : "Couldn't read the connections."}</span> <span className="text-muted-foreground">{failure(connections.error)}</span></p>
            {!refused && <Button type="button" size="sm" variant="outline" disabled={connections.isFetching} onClick={() => void connections.refetch()}><RefreshCw />Read again</Button>}
          </div>
        ))}
      {connections.data && (connections.data.document.records.length === 0
        ? <Well><EmptyState icon={Cable} title="No connections" next={<UploadCsv />}>No system is connected to this cell yet. Upload a CSV to start, or ask an admin to connect a system.</EmptyState></Well>
        : <TableCard><Table>
          <TableHeader><TableRow>
            <TableHead>Connection</TableHead><TableHead>Status</TableHead><TableHead>Last sync</TableHead><TableHead>Up to</TableHead>
            <TableHead className="text-end">Read</TableHead><TableHead className="text-end">Landed</TableHead><TableHead className="text-end">Quarantined</TableHead><TableHead />
          </TableRow></TableHeader>
          <TableBody>
            {connections.data.document.records.map(({ id, values }, index) => {
              const status = connectionStatus(values)
              const read = connections.data
              return (
                <TableRow key={id}>
                  <TableCell><div>{values.label ?? id}</div><div className="text-meta text-muted-foreground">{systemName(values.system ?? "")} · <span className="font-mono"><bdi dir="ltr">{values.target}</bdi></span></div></TableCell>
                  <TableCell>
                    <Badge variant={status.variant}>{status.label}</Badge>
                    {status.note && <div className="max-w-64 pt-1 text-meta whitespace-normal text-muted-foreground">{status.note}</div>}
                    {status.error && <div className="max-w-64 pt-1 text-meta whitespace-normal text-destructive-text" title={status.error.message ?? undefined}><span className="font-mono"><bdi dir="ltr">{status.error.code}</bdi></span>{status.error.message ? `: ${status.error.message}` : ""}</div>}
                  </TableCell>
                  <TableCell className="text-body whitespace-nowrap">{values.status === "syncing" ? <>started <Instant iso={values.last_sync_started_at} /></> : <Instant iso={values.last_sync_ended_at} />}</TableCell>
                  <TableCell className="max-w-48 truncate font-mono text-meta" title={values.watermark ?? "The source's own modified-since mark after the last sync"}><bdi dir="ltr">{watermarkText(values.watermark)}</bdi></TableCell>
                  <TableCell className="text-end tabular-nums">{count(read, index, "rows_read", values.rows_read)}</TableCell>
                  <TableCell className="text-end tabular-nums">{count(read, index, "rows_landed", values.rows_landed)}</TableCell>
                  <TableCell className="text-end tabular-nums" title={values.quarantine_reasons ?? undefined}>{count(read, index, "rows_quarantined", values.rows_quarantined)}{values.quarantine_reasons ? <div className="font-mono text-meta text-muted-foreground">{values.quarantine_reasons}</div> : null}</TableCell>
                  <TableCell>{values.system !== "upload" && status.syncable && <SyncNow id={id} />}</TableCell>
                </TableRow>
              )
            })}
          </TableBody>
        </Table></TableCard>)}
    </Panel>
  )
}
