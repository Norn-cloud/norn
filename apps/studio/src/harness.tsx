/**
 * The read harness (#1374): one record read through `useCellQuery`, outside the Studio shell, so
 * browser rows prove the typed client, the live socket and the cache against a stand-in cell.
 * `?type=<concept>&id=<identity>` names the record. It renders only what the cell sends: the
 * record's values (every number is its exact text, as the SDK reads documents), its version, and
 * whether pushes arrive: once they no longer do, the record is shown as the last answer read, with
 * its time and a way to read again. The workspace comes first: a failed scope discovery is the
 * shared Workspace unavailable panel, and a workspace with no data cell says so.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { Database, RefreshCw } from "lucide-react"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { LoadState, WorkspaceUnavailable } from "@/components/load-state"
import { EmptyState } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { useCellQuery } from "./lib/cell-query.ts"
import { NO_DATA_CELL, useStudioScope, type StudioScope } from "./lib/definitions.ts"
import "./index.css"

/** A value as the cell sent it: text (a number's exact text included) as is, anything else as JSON. */
const shown = (value: unknown): string => typeof value === "string" ? value : JSON.stringify(value)

/** The workspace first: a failed scope discovery is the shared Workspace unavailable panel, with its own Retry; no cell is said so. */
function RecordRead({ type, id }: { readonly type: string; readonly id: string }) {
  const scope = useStudioScope()
  return (
    <section className="space-y-3" data-testid="record-read">
      {scope.isError
        ? <WorkspaceUnavailable error={scope.error} isFetching={scope.isFetching} onRetry={() => void scope.refetch()} />
        : scope.isSuccess && scope.data === null
          ? <EmptyState icon={Database} title="No data cell">{NO_DATA_CELL}</EmptyState>
          : scope.isSuccess
            ? <Record type={type} id={id} scope={scope.data as StudioScope} />
            : <LoadState status="pending" skeleton="detail">{() => null}</LoadState>}
    </section>
  )
}

function Record({ type, id, scope }: { readonly type: string; readonly id: string; readonly scope: StudioScope }) {
  const { read, state, liveRefusal } = useCellQuery("/v1/objects/{type}/{id}", { path: { type, id } }, { scope, what: "This record" })
  return (
    <div data-live-refusal-code={liveRefusal?.code}>
      <LoadState status={state.status} data={state.data} error={state.error} isFetching={read.isFetching} onRetry={() => void read.refetch()} skeleton="detail">
        {(doc) => (
          <dl className="grid grid-cols-[max-content_1fr] items-baseline gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">Version</dt>
            <dd data-testid="record-version"><bdi dir="ltr" className="font-mono">{doc.document.version ?? "—"}</bdi></dd>
            <dt className="text-muted-foreground">Source</dt>
            <dd data-testid="record-source">{doc.digest === null ? "read" : "pushed"}</dd>
            <dt className="text-muted-foreground">Updates</dt>
            <dd data-testid="record-updates" data-stale={state.stale ? "" : undefined}>
              {state.stale
                ? (
                  <span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1">
                    <span>Not followed: the last answer read, as of <time dateTime={new Date(read.dataUpdatedAt).toISOString()}><bdi dir="ltr">{new Date(read.dataUpdatedAt).toLocaleTimeString()}</bdi></time></span>
                    <Button type="button" variant="outline" size="sm" disabled={read.isFetching} onClick={() => void read.refetch()}>
                      <RefreshCw className={read.isFetching ? "animate-spin" : undefined} />Read again
                    </Button>
                  </span>
                )
                : "live"}
            </dd>
            {Object.entries(doc.document.values ?? {}).map(([property, value]) => (
              <div key={property} className="contents">
                <dt className="text-muted-foreground">{property}</dt>
                <dd data-testid={`record-value-${property}`}><bdi dir="ltr" className="font-mono">{shown(value)}</bdi></dd>
              </div>
            ))}
          </dl>
        )}
      </LoadState>
    </div>
  )
}

const search = new URLSearchParams(window.location.search)
const client = new QueryClient()
document.documentElement.classList.toggle("dark", window.matchMedia("(prefers-color-scheme: dark)").matches)

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <main className="mx-auto max-w-2xl p-8">
        <RecordRead type={search.get("type") ?? ""} id={search.get("id") ?? ""} />
      </main>
    </QueryClientProvider>
  </StrictMode>,
)
