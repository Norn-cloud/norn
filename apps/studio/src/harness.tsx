/**
 * The read harness (#1374): one record read through `useCellQuery`, outside the Studio shell, so
 * browser rows prove the typed client, the live socket and the cache against a stand-in cell.
 * `?type=<concept>&id=<identity>` names the record. It renders only what the cell sends: the
 * record's values (every number is its exact text, as the SDK reads documents), its version, and
 * whether pushes arrive: once they no longer do, the record is shown as the last answer read, with
 * its time and a way to read again. The workspace comes first: a failed scope discovery is the
 * shared Workspace unavailable panel, and a workspace with no data cell says so.
 * `&next=<identity>` offers "Next record", which reads another record in the same mounted view (a
 * new key), and "Close record" unmounts the read, so browser rows can watch the hook subscribe,
 * reset and unsubscribe.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { Database, RefreshCw } from "lucide-react"
import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import { LoadState, WorkspaceUnavailable } from "@/components/load-state"
import { EmptyState } from "@/components/system/panel"
import { Button } from "@/components/ui/button"
import { useCellQuery } from "./lib/cell-query.ts"
import { matchReadState } from "./lib/state/index.ts"
import { NO_DATA_CELL, useStudioScope, type StudioScope } from "./lib/definitions.ts"
import "./index.css"

/** A value as the cell sent it: text (a number's exact text included) as is, anything else as JSON. */
const shown = (value: unknown): string => typeof value === "string" ? value : JSON.stringify(value)

/** The workspace first: a failed scope discovery is the shared Workspace unavailable panel, with its own Retry; no cell is said so. */
function RecordRead({ type, ids }: { readonly type: string; readonly ids: readonly string[] }) {
  const scope = useStudioScope()
  const [at, setAt] = useState(0)
  const [open, setOpen] = useState(true)
  const id = ids[at % Math.max(ids.length, 1)] ?? ""
  return (
    <section className="space-y-3" data-testid="record-read">
      {ids.length > 1 && (
        <div className="flex flex-wrap gap-2">
          <Button type="button" variant="outline" size="sm" disabled={!open} onClick={() => setAt((n) => n + 1)}>Next record</Button>
          <Button type="button" variant="outline" size="sm" onClick={() => setOpen((shown) => !shown)}>{open ? "Close record" : "Open record"}</Button>
        </div>
      )}
      {!open
        ? null
        : scope.isError
          ? <WorkspaceUnavailable error={scope.error} />
          : scope.isSuccess && scope.data === null
            ? <EmptyState icon={Database} title="No data cell">{NO_DATA_CELL}</EmptyState>
            : scope.isSuccess
              // One mounted view for every record: a new id is a new key for the same hook.
              ? <Record type={type} id={id} scope={scope.data as StudioScope} />
              : <LoadState status="pending" skeleton="detail">{() => null}</LoadState>}
    </section>
  )
}

function Record({ type, id, scope }: { readonly type: string; readonly id: string; readonly scope: StudioScope }) {
  const { read, state, liveRefusal } = useCellQuery("/v1/objects/{type}/{id}", { path: { type, id } }, { scope, what: "This record" })
  // Every read state is one of LoadState's: pending, the document, or an error with the cell's words.
  type Document = Extract<typeof state, { readonly kind: "ok" }>["document"]
  type Load = { readonly status: "pending" | "success" | "error"; readonly data?: Document; readonly error: Error | null }
  const load = matchReadState<Document, Error, Load>(state, {
    pending: () => ({ status: "pending" as const, error: null }),
    ok: ({ document }) => ({ status: "success" as const, data: document, error: null }),
    refused: ({ refusal }) => ({ status: "error" as const, error: refusal }),
    not_served: ({ error }) => ({ status: "error" as const, error }),
    unavailable: ({ error }) => ({ status: "error" as const, error }),
    answer_moved: ({ error }) => ({ status: "error" as const, error }),
    timed_out: ({ error }) => ({ status: "error" as const, error }),
  })
  const stale = state.kind === "ok" && state.freshness === "stale"
  return (
    <div data-testid="record" data-record-id={id} data-live-refusal-code={liveRefusal?.code}>
      <LoadState {...load} isFetching={read.isFetching} onRetry={() => void read.refetch()} skeleton="detail">
        {(doc) => (
          <dl className="grid grid-cols-[max-content_1fr] items-baseline gap-x-6 gap-y-1 text-body">
            <dt className="text-muted-foreground">Version</dt>
            <dd data-testid="record-version"><bdi dir="ltr" className="font-mono">{doc.document.version ?? "—"}</bdi></dd>
            <dt className="text-muted-foreground">Source</dt>
            <dd data-testid="record-source">{doc.digest === null ? "read" : "pushed"}</dd>
            <dt className="text-muted-foreground">Updates</dt>
            <dd data-testid="record-updates" data-stale={stale ? "" : undefined}>
              {stale
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
        <RecordRead type={search.get("type") ?? ""} ids={[search.get("id") ?? "", ...search.getAll("next")]} />
      </main>
    </QueryClientProvider>
  </StrictMode>,
)
