import { Link, useParams, useSearch } from "@tanstack/react-router"
import { Shapes } from "lucide-react"
import { useState } from "react"
import { LoadState } from "@/components/load-state"
import { EmptyState, PageHeader } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { refreshMode } from "@/lib/live-transport"
import type { Concept } from "../lib/catalog.ts"
import { studioScopeKey, useStudioScope, type StudioScope } from "../lib/definitions.ts"
import { useCatalog } from "../sources.tsx"
import { LawsPanel } from "../laws.tsx"
import { ProfilePanel } from "./profile-panel.tsx"
import { QuarantinePanel } from "./quarantine-panel.tsx"
import type { StoredView } from "../lib/views.ts"
import { RecordsGrid, type DisplayedGridQuery } from "./records-grid.tsx"
import { offersTimeline } from "../lib/timeline.ts"
import { ViewsBar, openedQuery } from "./views-bar.tsx"

function ScopedRecordsContent({ concept, scope, timeline }: {
  readonly concept: Concept
  readonly scope: StudioScope | undefined
  readonly timeline: boolean
}) {
  // The open saved view (none: every record) and the query the grid shows, for saving.
  // Keyed by `(concept.name, orgId, environmentId)` from the parent so a workspace switch never
  // carries one workspace's open view or filter query into another workspace.
  const [view, setView] = useState<StoredView | null>(null)
  const [query, setQuery] = useState<DisplayedGridQuery>()
  return (
    <>
      <ViewsBar objectType={concept.name} scope={scope} open={view} query={query} onOpen={setView} />
      <RecordsGrid key={`${concept.name}:${studioScopeKey(scope)}:${view?.id ?? "all"}:${view?.version ?? 0}`} concept={concept} scope={scope} initial={view ? openedQuery(view) : undefined} onQuery={setQuery} timeline={timeline} />
    </>
  )
}

/** `/objects/$type`: one object type's records in the grid, full width. Its definition is on `/sources/$type`. */
export function RecordsPage() {
  const { type = "" } = useParams({ strict: false })
  const search = useSearch({ strict: false }) as { population?: string }
  const { read, catalog } = useCatalog()
  const scopeRead = useStudioScope()
  const activeScope = scopeRead.data ?? undefined
  const scopeKey = studioScopeKey(activeScope)
  const concept = catalog?.byName.get(type)
  // A `core.TimeInterval` (through any parent) can be read as a timeline too; the catalog says which types are.
  const timeline = catalog !== undefined && concept !== undefined && offersTimeline(catalog, concept)
  // Use the same read for the catalog and its layout so mode discovery cannot remount an active grid.
  const live = read.data?.origin === "live"
  const initialTab = search.population === "held_back" ? "quarantine" : "records"
  return (
    <div className="space-y-4">
      <LoadState
        status={read.status}
        data={catalog}
        error={read.error}
        isFetching={read.isFetching}
        onRetry={() => { refreshMode(); void read.refetch() }}
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
              {live
                ? <Tabs key={`${concept.name}:${scopeKey}:${initialTab}`} defaultValue={initialTab}>
                  <TabsList><TabsTrigger value="records">Records</TabsTrigger><TabsTrigger value="profile">Profile</TabsTrigger><TabsTrigger value="quarantine">Quarantine</TabsTrigger><TabsTrigger value="rules">Rules</TabsTrigger></TabsList>
                  <TabsContent value="records" className="grid gap-3 pt-3">
                    <ScopedRecordsContent key={`${concept.name}:${scopeKey}`} concept={concept} scope={activeScope} timeline={timeline} />
                  </TabsContent>
                  <TabsContent value="profile" className="pt-3"><ProfilePanel objectType={concept.name} scales={new Map(concept.fields.flatMap(({ id, decimal }) => decimal?.scale === undefined ? [] : [[id, decimal.scale] as const]))} /></TabsContent>
                  <TabsContent value="quarantine" className="pt-3"><QuarantinePanel objectType={concept.name} /></TabsContent>
                  <TabsContent value="rules" className="pt-3"><LawsPanel concept={concept} label="Rules its records must keep" /></TabsContent>
                </Tabs>
                : <RecordsGrid key={concept.name} concept={concept} timeline={timeline} />}
            </>}
          </>
        )}
      </LoadState>
    </div>
  )
}
