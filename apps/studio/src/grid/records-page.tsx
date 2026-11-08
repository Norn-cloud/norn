import { Link, useParams } from "@tanstack/react-router"
import { Shapes } from "lucide-react"
import { EmptyState } from "@/components/panel"
import { useCatalog } from "../sources.tsx"
import { RecordsGrid } from "./records-grid.tsx"

/** `/objects/$type`: one object type's records in the grid, full width. Its definition is on `/sources/$type`. */
export function RecordsPage() {
  const { type = "" } = useParams({ strict: false })
  const { read, catalog } = useCatalog()
  const concept = catalog?.byName.get(type)
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
        <RecordsGrid concept={concept} />
      </>}
    </div>
  )
}
