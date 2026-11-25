/**
 * The state-matrix harness page (#1762 milestone C): one admitted Widget UX page drawn by
 * `<NornAppRenderer>` outside the Studio shell, so the generated matrix (`matrix.e2e.ts`) drives
 * every reachable state against a stand-in cell.
 *
 * It is Studio's real path, not a mock of it: the workspace is decided from the mode read
 * (`WorkspaceProvider`), the page document is the widget artifact the cell's definitions read
 * answered (`?widget=<artifactId>`), reads go through the workspace's reader, calls through its
 * scope, and the expression engine loads lazily (`sharedEngine`). UI SDK's page agent is created
 * before the page renders, so the matrix drives the page through `createAgent` as a browser agent
 * would. `?theme=dark`, `?dir=rtl` and `?locale=ar` pick the variant.
 */
import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query"
import { createAgent } from "@ui-sdk/react/agent"
import { checkPattern } from "@ui-sdk/core/testing"
import { StrictMode, useMemo } from "react"
import { createRoot } from "react-dom/client"
import { NornAppRenderer, type ActionEffects, type WidgetUx } from "../../../../packages/norn-react/src/index.ts"
import { sharedEngine } from "../../src/lib/canvas/checker.ts"
import { scopedHostFetch, defaultFetch } from "../../src/lib/definitions.ts"
import { useMode } from "../../src/lib/live-transport.ts"
import { widgetCalls } from "../../src/lib/widget-calls.ts"
import { useWorkspace, WorkspaceProvider } from "../../src/lib/workspace.tsx"
import { matrixIslands } from "./islands.tsx"
import "./matrix.css"

// Before the first render: the agent hears components as they are made.
const agent = createAgent()
Object.assign(window, { nornAgent: agent, nornCheckPattern: checkPattern })

const params = new URLSearchParams(location.search)
const locale = params.get("locale") ?? "en"
const dir = params.get("dir") === "rtl" ? "rtl" : "ltr"
document.documentElement.classList.toggle("dark", params.get("theme") === "dark")
document.documentElement.dir = dir
document.documentElement.lang = locale

type Envelope = { readonly artifactId?: unknown; readonly kind?: unknown; readonly payload?: unknown }
const record = (value: unknown): Record<string, unknown> => (typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {})

function MatrixPage() {
  const workspace = useWorkspace()
  const mode = useMode()
  const artifacts: readonly Envelope[] = mode.data?.kind === "live" ? (mode.data.catalog.document as readonly Envelope[]) : []
  const widgetId = params.get("widget") ?? ""
  const ux = record(artifacts.find((artifact) => artifact.kind === "widget" && artifact.artifactId === widgetId)?.payload).ux as WidgetUx | undefined
  // From the admitted definitions: each entity artifact's object type, and which actions are ledgered.
  const { objectTypes, ledgered } = useMemo(() => {
    const types = new Map<string, string>()
    const books = new Set<string>()
    for (const artifact of artifacts) {
      const payload = record(artifact.payload)
      const name = record(payload.objectType).name
      if (artifact.kind === "entity" && typeof artifact.artifactId === "string" && typeof name === "string") types.set(artifact.artifactId, name)
      if (artifact.kind === "action" && typeof artifact.artifactId === "string" && payload.ledgered === true) books.add(artifact.artifactId)
    }
    return { objectTypes: types, ledgered: books }
  }, [artifacts])
  // The cell's action catalog: what each action's caller must handle (`uiEffects`).
  const live = workspace.kind === "live" ? workspace : null
  const catalog = useQuery({
    queryKey: ["matrix-actions", live?.scope.orgId, live?.scope.environmentId],
    enabled: live !== null,
    queryFn: async () => {
      const response = await scopedHostFetch(defaultFetch, "/api/host", async () => live!.scope)("/v1/actions", { headers: { accept: "application/json" } })
      const items = response.ok ? record(await response.json()).items : []
      return new Map((Array.isArray(items) ? items : []).map((item) => [String(record(item).actionId), record(item).uiEffects as ActionEffects]))
    },
  })
  const calls = useMemo(() => (workspace.kind === "live" || workspace.kind === "recorded" ? widgetCalls(workspace) : null), [workspace])
  const actionIdOf = (artifactId: string) => String(record(artifacts.find((artifact) => artifact.artifactId === artifactId)?.payload).actionId ?? "")
  if (ux === undefined || calls === null) return <p role="status" data-matrix="waiting" className="text-meta text-muted-foreground">Reading the page document…</p>
  return (
    <NornAppRenderer
      ux={ux}
      workspace={workspace}
      calls={calls}
      engine={sharedEngine}
      islands={matrixIslands}
      objectTypeOf={(artifactId) => objectTypes.get(artifactId)}
      actionIdOf={(artifactId) => actionIdOf(artifactId) || undefined}
      actionEffects={(artifactId) => {
        const effects = catalog.data?.get(actionIdOf(artifactId))
        const books = ledgered.has(artifactId)
        return effects === undefined && !books ? undefined : { irreversible: false, destructive: false, money: [], ...effects, ledgered: books }
      }}
      locale={locale}
      dir={dir}
    />
  )
}

const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <WorkspaceProvider>
        <main className="mx-auto grid max-w-3xl gap-4 p-4 sm:p-8">
          <MatrixPage />
        </main>
      </WorkspaceProvider>
    </QueryClientProvider>
  </StrictMode>,
)
