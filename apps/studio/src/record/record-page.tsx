/**
 * `/sources/$concept/$`: a record, its id the rest of the path, drawn from the record page the
 * workspace's cell serves (#1762 C2). The splat also matches the object type's own URL with an empty
 * id, which is the type's page.
 *
 * The page is no Studio code: it is the admitted widget `norn.studio.record.widget`
 * (`registry/studio/record-page.ts`), read from the cell's definitions with the rest of the catalog:
 * the workspace's own, or else the one the cell serves built in (`builtins`, norn-api
 * `builtin_pages`). `<NornAppRenderer>` draws it in the shell with Studio's islands (`islands.tsx`). Its machine
 * reads the record through the decided workspace's reader and owns every outcome of that read.
 *
 * Before the renderer: the workspace. Nothing is read until the server's mode answer decided one;
 * while it is read the page shows its skeleton, and when it fails, Workspace unavailable with the
 * mode's own Retry (`WorkspaceUndecided`), the shell's contract (`data-workspace-unavailable`). Then
 * the definitions read that carries the page: its skeleton, its failure with Retry, or, from a cell
 * that serves no record page, that plain fact.
 */
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useParams } from "@tanstack/react-router"
import { Rows3 } from "lucide-react"
import { useMemo } from "react"
import { RevealScope } from "@/components/masked-value"
import { LoadState, WorkspaceUndecided } from "@/components/load-state"
import { EmptyState } from "@/components/system/panel"
import { NornAppRenderer } from "../../../../packages/norn-react/src/renderer.tsx"
import type { WidgetUx } from "../../../../packages/norn-react/src/ux.ts"
import { sharedEngine } from "../lib/canvas/checker.ts"
import { retryWorkspace } from "../lib/live-transport.ts"
import { directionOf, useLocale } from "../lib/locale.tsx"
import { translate, useMessage } from "../lib/translations.ts"
import { transport } from "../lib/transport.ts"
import { useCatalog } from "../lib/use-catalog.ts"
import { widgetCalls } from "../lib/widget-calls.ts"
import { useWorkspace } from "../lib/workspace.tsx"
import { ObjectTypePage } from "../sources.tsx"
import { RECORD_ISLANDS, RECORD_PAGE, RecordRouteContext } from "./islands.tsx"

export function SourcesItemPage() {
  const { _splat: id = "" } = useParams({ strict: false })
  return id === "" ? <ObjectTypePage /> : <RecordPage />
}

type Envelope = { readonly artifactId?: unknown; readonly kind?: unknown; readonly payload?: unknown }

/**
 * The record page among the definitions the cell served, or else among its built-in pages, or
 * `undefined` when it serves none. The cell serves a built-in only while the workspace installed no
 * page with its id, so at most one list holds it; the workspace's own is looked at first all the same.
 */
export function recordPageOf(artifacts: readonly Envelope[] | undefined, builtins: readonly Envelope[] = []): WidgetUx | undefined {
  const page = [...artifacts ?? [], ...builtins].find((artifact) => artifact.kind === "widget" && artifact.artifactId === RECORD_PAGE)
  return (page?.payload as { readonly ux?: WidgetUx } | undefined)?.ux
}

const NO_TYPE = () => undefined

export function RecordPage() {
  // The id is the rest of the path: ids hold `/`, and Cloudflare redirects an encoded `%2F` to a decoded one, so it cannot be one segment.
  const { concept: name = "", _splat: id = "" } = useParams({ strict: false })
  const workspace = useWorkspace()
  const { read } = useCatalog()
  // The cell's built-in pages, carried by the same definitions read: it serves one only while the
  // workspace installed no page with its id, so the two lists never both hold the record page.
  const builtins = useQuery({ queryKey: ["catalog", "builtins"], queryFn: transport.builtins })
  const queries = useQueryClient()
  const { locale } = useLocale()
  const say = useMessage()
  const ux = useMemo(
    () => recordPageOf(read.data?.document as readonly Envelope[] | undefined, builtins.data),
    [read.data, builtins.data])
  const context = useMemo(() => ({ objectType: name, id }), [name, id])
  const calls = useMemo(() => (workspace.kind === "live" || workspace.kind === "recorded" ? widgetCalls(workspace) : null), [workspace])
  const route = useMemo(() => ({ name, id }), [name, id])
  if (workspace.kind === "deciding" || workspace.kind === "unavailable") {
    return <WorkspaceUndecided workspace={workspace} skeleton="detail" loadingLabel={say("Reading the record")} />
  }
  if (read.isPending || read.isError || builtins.isPending || builtins.isError || calls === null) {
    return (
      <LoadState status={read.isError || builtins.isError ? "error" : "pending"} error={read.error ?? builtins.error} isFetching={read.isFetching || builtins.isFetching}
        onRetry={() => { void read.refetch(); void builtins.refetch() }} skeleton="detail" loadingLabel={say("Reading the record")}>
        {() => null}
      </LoadState>
    )
  }
  if (ux === undefined) {
    return (
      <div data-record-page="not-served">
        <EmptyState icon={Rows3} title={say("No record page")}>{say("This workspace's cell serves no record page, so its records cannot be shown here.")}</EmptyState>
      </div>
    )
  }
  return (
    // A record's reveals go to the workspace it was read in.
    <RevealScope.Provider value={workspace.kind === "live" ? workspace.scope : undefined}>
      <RecordRouteContext value={route}>
        {/* One page per record: another record is another page, its own machine and read. */}
        <NornAppRenderer key={`${name}\u0000${id}`} ux={ux} pageId="record" context={context} workspace={workspace} calls={calls}
          engine={sharedEngine} islands={RECORD_ISLANDS} objectTypeOf={NO_TYPE} actionIdOf={NO_TYPE}
          locale={locale} dir={directionOf(locale)} translate={translate}
          onRetryWorkspace={() => void retryWorkspace(queries)} />
      </RecordRouteContext>
    </RevealScope.Provider>
  )
}
