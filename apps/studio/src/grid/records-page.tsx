import { Link, useParams, useSearch } from "@tanstack/react-router"
import { Shapes } from "lucide-react"
import { EmptyState, PageHeader } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { useMode } from "@/lib/live-transport"
import { useCatalog } from "../sources.tsx"
import { LawsPanel } from "../laws.tsx"
import { ProfilePanel } from "./profile-panel.tsx"
import { QuarantinePanel } from "./quarantine-panel.tsx"
import { useState } from "react"
import type { StoredView } from "../lib/views.ts"
import { RecordsGrid, type GridQuery } from "./records-grid.tsx"
import { offersTimeline } from "../lib/timeline.ts"
import { ViewsBar, openedQuery } from "./views-bar.tsx"

/** `/objects/$type`: one object type's records in the grid, full width. Its definition is on `/sources/$type`. */
export function RecordsPage() {
  const { type = "" } = useParams({ strict: false })
  const search = useSearch({ strict: false }) as { population?: string }
  const { read, catalog } = useCatalog()
  const concept = catalog?.byName.get(type)
  // A `core.TimeInterval` (through any parent) can be read as a timeline too; the catalog says which types are.
  const timeline = catalog !== undefined && concept !== undefined && offersTimeline(catalog, concept)
  // A profile is the connected cell's; recordings carry none.
  const live = useMode().data?.kind === "live"
  // The open saved view (none: every record) and the query the grid shows, for saving.
  const [view, setView] = useState<StoredView | null>(null)
  const [query, setQuery] = useState<GridQuery>()
  const initialTab = search.population === "held_back" ? "quarantine" : "records"
  return (
    <div className="space-y-4">
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <p role="alert" className="text-destructive">{read.error.message}</p>}
      {catalog && concept === undefined && <EmptyState icon={Shapes}>No object type named {type} is installed.</EmptyState>}
      {catalog && concept !== undefined && concept.definition !== "object type" && <EmptyState icon={Shapes}>{concept.title} is a shared interface; it has no records of its own.</EmptyState>}
      {concept?.definition === "object type" && <>
        <PageHeader title={concept.title} lang={concept.titleLang} meta={<span className="font-mono">{concept.name}</span>}
          actions={<Button asChild variant="ghost" size="sm"><Link to="/sources/$concept" params={{ concept: concept.name }}>Definition</Link></Button>} />
        {live
          ? <Tabs key={`${concept.name}:${initialTab}`} defaultValue={initialTab}>
            <TabsList><TabsTrigger value="records">Records</TabsTrigger><TabsTrigger value="profile">Profile</TabsTrigger><TabsTrigger value="quarantine">Quarantine</TabsTrigger><TabsTrigger value="rules">Rules</TabsTrigger></TabsList>
            <TabsContent value="records" className="grid gap-3 pt-3">
              <ViewsBar objectType={concept.name} open={view} query={query} onOpen={setView} />
              <RecordsGrid key={`${concept.name}:${view?.id ?? "all"}:${view?.version ?? 0}`} concept={concept} initial={view ? openedQuery(view) : undefined} onQuery={setQuery} timeline={timeline} />
            </TabsContent>
            <TabsContent value="profile" className="pt-3"><ProfilePanel objectType={concept.name} scales={new Map(concept.fields.flatMap(({ id, decimal }) => decimal?.scale === undefined ? [] : [[id, decimal.scale] as const]))} /></TabsContent>
            <TabsContent value="quarantine" className="pt-3"><QuarantinePanel objectType={concept.name} /></TabsContent>
            <TabsContent value="rules" className="pt-3"><LawsPanel concept={concept} label="Rules its records must keep" /></TabsContent>
          </Tabs>
          : <RecordsGrid key={concept.name} concept={concept} timeline={timeline} />}
      </>}
    </div>
  )
}
