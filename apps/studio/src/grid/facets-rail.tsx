import { useQueries, useQuery } from "@tanstack/react-query"
import { Check, ListFilter, X } from "lucide-react"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import type { Field } from "../lib/catalog.ts"
import { FacetsTimedOut, chosen, countText, readFacets, toggled, without, type Facet } from "../lib/facets.ts"
import { displayOf } from "../lib/objects.ts"
import type { Clause, Predicate } from "../lib/records-query.ts"

const label = (field: Field | undefined, property: string) => field?.label ?? property.replaceAll("_", " ")

/** A value as the reader reads it: a choice by its declared label, a yes/no in words, text as stored. */
function valueText(field: Field | undefined, value: string | boolean): string {
  if (typeof value === "boolean") return value ? "Yes" : "No"
  return field ? displayOf(field, value, value) : value
}

/**
 * The type's categorical properties with the cell's exact count of each value over the rows the
 * grid's filter lists, most rows first. Choosing a value narrows the grid; a property's own counts
 * leave out its own choice, so a second value can be added beside the first.
 */
export function FacetsRail({ objectType, fields, filter, onFilter }: {
  readonly objectType: string
  readonly fields: readonly Field[]
  readonly filter: Predicate | null
  readonly onFilter: (next: Predicate | null) => void
}) {
  const byId = new Map(fields.map((field) => [field.id, field]))
  const base = useQuery({ queryKey: ["facets", objectType, JSON.stringify(filter)], queryFn: () => readFacets(objectType, filter), retry: false })
  const narrowed = [...new Set((filter?.all ?? []).filter((clause) => clause.op === "eq" || clause.op === "in").map(({ property }) => property))]
  const own = useQueries({
    queries: narrowed.map((property) => {
      const rest = without(filter, property)
      return { queryKey: ["facets", objectType, JSON.stringify(rest), property], queryFn: () => readFacets(objectType, rest, [property]), retry: false }
    }),
  })
  if (base.isPending) return <p className="px-1 text-meta text-muted-foreground">Counting values…</p>
  if (base.isError) return (
    <div role="status" className="grid gap-1.5 px-1 text-meta text-muted-foreground">
      <p>{base.error instanceof FacetsTimedOut ? "Counts aren't available right now." : base.error.message}</p>
      {base.error instanceof FacetsTimedOut && <p>{base.error.message} The records are still listed.</p>}
      <div><Button type="button" variant="outline" size="xs" disabled={base.isFetching} onClick={() => void base.refetch()}>Count again</Button></div>
    </div>
  )
  const document = base.data.document
  const facetOf = (property: string): Facet | undefined => {
    const at = narrowed.indexOf(property)
    return (at >= 0 ? own[at]?.data?.document.facets[0] : undefined) ?? document.facets.find((facet) => facet.property === property)
  }
  const shown = document.facets.filter(({ values }) => values.length > 0)
  if (shown.length === 0) return <p className="px-1 text-meta text-muted-foreground">No text, choice or yes/no values to narrow by.</p>
  return (
    <div className="grid gap-4" aria-label="Narrow by">
      <div className="flex items-center gap-2 px-1 text-meta text-muted-foreground">
        <ListFilter className="size-3.5" aria-hidden />
        <span className="flex-1">{countText(document.rows, document.lowerBound)} {document.rows === 1 ? "record" : "records"}, counted by the cell</span>
      </div>
      {shown.map(({ property }) => {
        const facet = facetOf(property)
        if (facet === undefined) return null
        const field = byId.get(property)
        const held = chosen(filter, property)
        const most = Math.max(1, ...facet.values.map(({ count }) => count))
        return (
          <section key={property} className="grid gap-0.5">
            <h3 className="px-1 pb-0.5 text-meta font-medium text-muted-foreground">{label(field, property)}</h3>
            {facet.values.map(({ value, count }) => {
              const on = held.includes(value)
              return (
                <button key={String(value)} type="button" aria-pressed={on} onClick={() => onFilter(toggled(filter, property, value))}
                  className={cn("relative flex h-row-scan items-center gap-2 overflow-hidden rounded-md px-2 text-start text-sm outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring/40", on && "bg-primary/8 font-medium")}>
                  <span aria-hidden className="absolute inset-y-1 left-0 rounded-e-sm bg-primary/10" style={{ width: `${(count / most) * 100}%` }} />
                  <span className="relative flex size-3.5 shrink-0 items-center justify-center rounded-[3px] border border-node-border bg-card">{on && <Check className="size-3" aria-hidden />}</span>
                  <span className="relative min-w-0 flex-1 truncate" title={String(value)}>{valueText(field, value)}</span>
                  <span className="relative shrink-0 text-meta tabular-nums text-muted-foreground">{countText(count, document.lowerBound)}</span>
                </button>
              )
            })}
            {(facet.otherRows > 0 || facet.nulls > 0) && (
              <p className="px-2 pt-0.5 text-meta text-muted-foreground">
                {[facet.otherRows > 0 && `${facet.otherRows.toLocaleString("en")} with other values`, facet.nulls > 0 && `${facet.nulls.toLocaleString("en")} with none`].filter(Boolean).join(" · ")}
              </p>
            )}
          </section>
        )
      })}
    </div>
  )
}

/** A clause in words: "status is open", "location contains north", "unit is empty". */
export function clauseText(clause: Clause, field: Field | undefined): string {
  const name = label(field, clause.property)
  const value = (text: string | boolean) => valueText(field, text)
  switch (clause.op) {
    case "eq": return `${name} is ${value(clause.value)}`
    case "ne": return `${name} is not ${value(clause.value)}`
    case "in": return `${name} is ${clause.value.map(value).join(" or ")}`
    case "contains": return `${name} contains ${String(clause.value)}`
    case "prefix": return `${name} starts with ${String(clause.value)}`
    case "lt": return `${name} below ${String(clause.value)}`
    case "lte": return `${name} at most ${String(clause.value)}`
    case "gt": return `${name} above ${String(clause.value)}`
    case "gte": return `${name} at least ${String(clause.value)}`
    case "between": return `${name} from ${clause.value[0]} to ${clause.value[1]}`
    case "empty": return `${name} is empty`
    case "present": return `${name} has a value`
  }
}

/** The filter the cell applied, one chip per clause; removing a chip asks the cell again without it. */
export function FilterChips({ filter, fields, onFilter }: { readonly filter: Predicate | null; readonly fields: readonly Field[]; readonly onFilter: (next: Predicate | null) => void }) {
  const clauses = filter?.all ?? []
  if (clauses.length === 0) return null
  const byId = new Map(fields.map((field) => [field.id, field]))
  const remove = (index: number) => {
    const rest = clauses.filter((_, at) => at !== index)
    onFilter(rest.length === 0 ? null : { all: rest })
  }
  return (
    <div className="flex flex-wrap items-center gap-1.5" aria-label="Applied filter">
      {clauses.map((clause, index) => (
        <span key={`${clause.property}:${clause.op}:${index}`} className="inline-flex h-7 items-center gap-1 rounded-full border border-node-border bg-card ps-2.5 pe-1 text-meta shadow-node">
          {clauseText(clause, byId.get(clause.property))}
          <button type="button" aria-label={`Remove ${clauseText(clause, byId.get(clause.property))}`} onClick={() => remove(index)} className="flex size-5 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><X className="size-3" /></button>
        </span>
      ))}
      <Button type="button" variant="ghost" size="sm" className="h-7 px-2 text-meta" onClick={() => onFilter(null)}>Clear all</Button>
    </div>
  )
}
