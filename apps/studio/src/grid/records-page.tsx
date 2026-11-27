import { useQueryClient } from "@tanstack/react-query"
import { Link, useParams, useSearch } from "@tanstack/react-router"
import { Shapes } from "lucide-react"
import { useState } from "react"
import { LoadState } from "@/components/load-state"
import { PageHeader } from "@/components/panel"
import { EmptyState } from "@/components/system/panel"
import { Button } from "@/components/ui/button"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import type { Concept } from "../lib/catalog.ts"
import { studioScopeKey } from "../lib/definitions.ts"
import { retryWorkspace } from "../lib/live-transport.ts"
import { useWorkspace, type ScopedReader } from "../lib/workspace.tsx"
import { useCatalog } from "../sources.tsx"
import { LawsPanel } from "../laws.tsx"
import { ProfilePanel } from "./profile-panel.tsx"
import { QuarantinePanel } from "./quarantine-panel.tsx"
import type { StoredView } from "../lib/views.ts"
import { DecidedRecordsGrid, RecordsGrid, type DisplayedGridQuery } from "./records-grid.tsx"
import { offersTimeline } from "../lib/timeline.ts"
import { ViewsBar, openedQuery } from "./views-bar.tsx"

/**
 * A live type's records, profile, quarantine and rules, all bound to the workspace the page's mode
 * decided (#1754): never the session's scope, which can still answer after the mode read failed.
 */
function ScopedRecordsTabs({ concept, reader, initialTab, timeline }: {
  readonly concept: Concept
  readonly reader: ScopedReader
  readonly initialTab: string
  readonly timeline: boolean
}) {
  const { scope } = reader
  const scopeKey = studioScopeKey(scope)
  // The open saved view (none: every record) and the query the grid shows, for saving.
  // Held above `<Tabs>` so switching Records -> Profile -> Records in the same workspace keeps the
  // selected view, while keying this component by `(concept.name, orgId, environmentId)` resets it
  // whenever the active workspace changes.
  const [view, setView] = useState<StoredView | null>(null)
  const [query, setQuery] = useState<DisplayedGridQuery>()
  return (
    <Tabs defaultValue={initialTab}>
      <TabsList><TabsTrigger value="records">Records</TabsTrigger><TabsTrigger value="profile">Profile</TabsTrigger><TabsTrigger value="quarantine">Quarantine</TabsTrigger><TabsTrigger value="rules">Rules</TabsTrigger></TabsList>
      <TabsContent value="records" className="grid gap-3 pt-3">
        <ViewsBar objectType={concept.name} scope={scope} open={view} query={query} onOpen={setView} />
        <RecordsGrid key={`${concept.name}:${scopeKey}:${view?.id ?? "all"}:${view?.version ?? 0}`} concept={concept} reader={reader} initial={view ? openedQuery(view) : undefined} onQuery={setQuery} timeline={timeline} />
      </TabsContent>
      <TabsContent value="profile" className="pt-3"><ProfilePanel objectType={concept.name} scope={scope} scales={new Map(concept.fields.flatMap(({ id, decimal }) => decimal?.scale === undefined ? [] : [[id, decimal.scale] as const]))} /></TabsContent>
      <TabsContent value="quarantine" className="pt-3"><QuarantinePanel objectType={concept.name} scope={scope} /></TabsContent>
      <TabsContent value="rules" className="pt-3"><LawsPanel concept={concept} label="Rules its records must keep" /></TabsContent>
    </Tabs>
  )
}

/** `/objects/$type`: one object type's records in the grid, full width. Its definition is on `/sources/$type`. */
export function RecordsPage() {
  const { type = "" } = useParams({ strict: false })
  const search = useSearch({ strict: false }) as { population?: string }
  const queries = useQueryClient()
  const { read, catalog } = useCatalog()
  // The records are bound to the workspace the page's mode decided, as `/sources/$type`'s are: a live
  // workspace's tabs read there; a recorded one's grid reads the recording; until the mode is decided
  // (or after it failed) the Records section shows its skeleton or Workspace unavailable (#1740, #1745, #1754).
  const workspace = useWorkspace()
  const concept = catalog?.byName.get(type)
  // A `core.TimeInterval` (through any parent) can be read as a timeline too; the catalog says which types are.
  const timeline = catalog !== undefined && concept !== undefined && offersTimeline(catalog, concept)
  const initialTab = search.population === "held_back" ? "quarantine" : "records"
  return (
    <div className="space-y-4">
      <LoadState
        status={read.status}
        data={catalog}
        error={read.error}
        isFetching={read.isFetching}
        // Route pending/refusal Retry through retryWorkspace so a wedged mode probe or unavailable
        // cell recovers in one click without refetching against a dead cell (#1774).
        onRetry={() => void retryWorkspace(queries).then((ok) => { if (ok) void read.refetch({ cancelRefetch: false }) })}
        bone="records-table"
        skeleton="table"
        loadingLabel="Reading records"
      >
        {() => (
          <>
            {concept === undefined && <EmptyState icon={Shapes} title="Type not found" next={<Button asChild size="sm" variant="outline"><Link to="/sources">See all types</Link></Button>}>No object type named {type} is installed.</EmptyState>}
            {concept !== undefined && concept.definition !== "object type" && <EmptyState icon={Shapes} title="Shared interface" next={<Button asChild size="sm" variant="outline"><Link to="/sources/$concept" params={{ concept: concept.name }}>View definition</Link></Button>}>{concept.title} is a shared interface; it has no records of its own.</EmptyState>}
            {concept?.definition === "object type" && <>
              <PageHeader title={concept.title} lang={concept.titleLang} meta={<span className="font-mono">{concept.name}</span>}
                actions={<Button asChild variant="ghost" size="sm"><Link to="/sources/$concept" params={{ concept: concept.name }}>Definition</Link></Button>} />
              {workspace.kind === "live"
                ? <ScopedRecordsTabs key={`${concept.name}:${studioScopeKey(workspace.scope)}:${initialTab}`} concept={concept} reader={workspace.reader} initialTab={initialTab} timeline={timeline} />
                : <DecidedRecordsGrid concept={concept} timeline={timeline} />}
            </>}
          </>
        )}
      </LoadState>
    </div>
  )
}
