import { Link, useParams } from "@tanstack/react-router"
import { Shapes } from "lucide-react"
import { EmptyState } from "@/components/panel"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { useMode } from "@/lib/live-transport"
import { useCatalog } from "../sources.tsx"
import { ProfilePanel } from "./profile-panel.tsx"
import { RecordsGrid } from "./records-grid.tsx"

/** `/objects/$type`: one object type's records in the grid, full width. Its definition is on `/sources/$type`. */
export function RecordsPage() {
  const { type = "" } = useParams({ strict: false })
  const { read, catalog } = useCatalog()
  const concept = catalog?.byName.get(type)
  // A profile is the connected cell's; recordings carry none.
  const live = useMode().data?.kind === "live"
  return (
    <div className="space-y-4">
      {read.isPending && <p className="text-muted-foreground">Reading definitions…</p>}
      {read.isError && <p role="alert" className="text-destructive">{read.error.message}</p>}
      {catalog && concept === undefined && <EmptyState icon={Shapes}>No object type named {type} is installed.</EmptyState>}
      {catalog && concept !== undefined && concept.definition !== "object type" && <EmptyState icon={Shapes}>{concept.title} is a shared interface; it has no records of its own.</EmptyState>}
      {concept?.definition === "object type" && <>
        <header className="flex flex-wrap items-baseline gap-3">
          <h1 className="text-xl font-medium">{concept.title}</h1>
          <span className="font-mono text-meta text-muted-foreground">{concept.name}</span>
          <Link to="/sources/$concept" params={{ concept: concept.name }} className="text-meta underline-offset-4 hover:underline">Definition</Link>
        </header>
        {live
          ? <Tabs defaultValue="records">
            <TabsList><TabsTrigger value="records">Records</TabsTrigger><TabsTrigger value="profile">Profile</TabsTrigger></TabsList>
            <TabsContent value="records" className="pt-3"><RecordsGrid key={concept.name} concept={concept} /></TabsContent>
            <TabsContent value="profile" className="pt-3"><ProfilePanel objectType={concept.name} /></TabsContent>
          </Tabs>
          : <RecordsGrid key={concept.name} concept={concept} />}
      </>}
    </div>
  )
}
