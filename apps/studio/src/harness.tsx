/**
 * The read harness (#1374): one record read through `useCellQuery`, outside the Studio shell, so
 * browser rows prove the typed client, the live socket and the cache against a stand-in cell.
 * `?type=<concept>&id=<identity>` names the record. It renders only what the cell sends: the
 * record's values (every number is its exact text, as the SDK reads documents), its version, and
 * whether pushes arrive.
 */
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { LoadState } from "@/components/load-state"
import { useCellQuery } from "./lib/cell-query.ts"
import { useStudioScope } from "./lib/definitions.ts"
import "./index.css"

/** A value as the cell sent it: text (a number's exact text included) as is, anything else as JSON. */
const shown = (value: unknown): string => typeof value === "string" ? value : JSON.stringify(value)

function RecordRead({ type, id }: { readonly type: string; readonly id: string }) {
  const scope = useStudioScope()
  const { read, state, liveRefusal } = useCellQuery("/v1/objects/{type}/{id}", { path: { type, id } }, { scope: scope.data ?? null, what: "This record" })
  return (
    <section className="space-y-3" data-testid="record-read" data-live-refusal-code={liveRefusal?.code}>
      <LoadState status={state.status} data={state.data} error={state.error} isFetching={read.isFetching} onRetry={() => void read.refetch()} skeleton="detail">
        {(doc) => (
          <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
            <dt className="text-muted-foreground">Version</dt>
            <dd data-testid="record-version">{doc.document.version ?? "—"}</dd>
            <dt className="text-muted-foreground">Source</dt>
            <dd data-testid="record-source">{doc.digest === null ? "read" : "pushed"}</dd>
            {Object.entries(doc.document.values ?? {}).map(([property, value]) => (
              <div key={property} className="contents">
                <dt className="text-muted-foreground">{property}</dt>
                <dd data-testid={`record-value-${property}`} className="font-mono">{shown(value)}</dd>
              </div>
            ))}
          </dl>
        )}
      </LoadState>
    </section>
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
