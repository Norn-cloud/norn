import { useQuery } from "@tanstack/react-query"
import { Link } from "@tanstack/react-router"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import type { Read } from "@/lib/exact"
import { cellRead } from "@/lib/live-transport"
import type { Concept } from "./lib/catalog.ts"
import { factText } from "./lib/objects.ts"
import type { ObjectRecord } from "./lib/transport.ts"

/** At most this many linked records are read and shown in full; the rest stay links. */
const SHOWN = 60

/**
 * The records linking to one record, fact by fact: every fact their type declares, read from the
 * connected cell one record at a time. An enum fact shows as a badge with its stored value.
 */
export function LinkedRecords({ from, ids }: { readonly from: Concept; readonly ids: readonly string[] }) {
  const shown = ids.slice(0, SHOWN)
  const records = useQuery({
    queryKey: ["linked-records", from.name, shown],
    queryFn: () => Promise.all(shown.map((id) => cellRead<ObjectRecord>(`/v1/objects/${encodeURIComponent(from.name)}/${encodeURIComponent(id)}`, "Records are"))),
    retry: false,
  })
  if (records.isPending) return <p className="text-sm text-muted-foreground">Reading {ids.length} {from.title.toLowerCase()} records…</p>
  if (records.isError) return <p role="alert" className="text-sm text-destructive">{records.error.message}</p>
  return (
    <div className="space-y-1">
      <TableCard><Table>
        <TableHeader><TableRow>
          <TableHead>Record</TableHead>
          {from.fields.map((field) => <TableHead key={field.id} lang={field.labelLang}>{field.label ?? field.id}</TableHead>)}
        </TableRow></TableHeader>
        <TableBody>
          {records.data.map((read: Read<ObjectRecord>) => (
            <TableRow key={read.document.id}>
              <TableCell className="font-mono text-xs"><Link to="/sources/$concept/$" params={{ concept: from.name, _splat: read.document.id }} className="underline-offset-4 hover:underline">{read.document.id}</Link></TableCell>
              {from.fields.map((field) => {
                const text = factText(field, read.document, read.numbers, "")
                return <TableCell key={field.id} className={field.decimal ? "text-right tabular-nums" : "text-sm"}>
                  {field.scalar.kind === "enum" && text !== "—" ? <Badge variant="outline">{field.valueLabels?.get(text) ?? text}</Badge> : text}
                </TableCell>
              })}
            </TableRow>
          ))}
        </TableBody>
      </Table></TableCard>
      {ids.length > SHOWN && <p className="text-meta text-muted-foreground">The first {SHOWN} of {ids.length} are shown in full.</p>}
    </div>
  )
}
