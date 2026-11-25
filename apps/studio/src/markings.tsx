import { Link } from "@tanstack/react-router"
import { Fingerprint, ReceiptText } from "lucide-react"
import { Chip } from "@/components/system/chip"
import { LoadState } from "@/components/load-state"
import { MarkingChip, useMarkingCatalog } from "@/components/marking-chip"
import { PageHeader } from "@/components/panel"
import { Panel } from "@/components/system/panel"
import { TableCard } from "@/components/system/table-card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { projectionWords, revealRoles } from "./lib/markings.ts"
import { useCatalog } from "./sources.tsx"

/**
 * PRIVACY-1/2 in one place, from the server's declarations only: each marking, how a marked value
 * is shown to someone who may not see it, the facts that carry it, and the roles the reveal policy
 * names. Whether a given reader may reveal a given value is the server's answer on each record.
 */
export function MarkingsPage() {
  const { read, catalog } = useCatalog()
  const markings = useMarkingCatalog()
  // Every marked fact, by marking: declared ones first, in catalog order, then any a fact names that the catalog does not declare.
  const carriers = new Map<string, { readonly type: string; readonly typeTitle: string; readonly field: string; readonly label: string }[]>()
  for (const concept of catalog?.concepts ?? []) {
    for (const field of concept.fields) {
      for (const marking of field.markings ?? []) carriers.set(marking, [...carriers.get(marking) ?? [], { type: concept.name, typeTitle: concept.title, field: field.id, label: field.label ?? field.id.replaceAll("_", " ") }])
    }
  }
  const ids = [...new Set([...markings?.declared.keys() ?? [], ...carriers.keys()])]
  return (
    <section className="space-y-6">
      <PageHeader title="Markings" description="Personal and sensitive data, as this workspace declares it: how each marking is shown to someone who may not see it, which facts carry it, and who may reveal it. Every reveal is recorded as a receipt before the value is shown." />
      <LoadState
        status={read.status}
        data={ids}
        error={read.error}
        isFetching={read.isFetching}
        onRetry={() => void read.refetch()}
        skeleton="table"
        loadingLabel="Reading markings"
        isEmpty={(list) => list.length === 0}
        empty={{ icon: Fingerprint, title: "No markings declared", description: "No fact in this workspace carries a marking, so every value is shown as stored to whoever may read it." }}
      >
        {(list) => (
          <TableCard><Table>
            <TableHeader><TableRow><TableHead>Marking</TableHead><TableHead>Shown to others as</TableHead><TableHead>Carried by</TableHead><TableHead>May reveal</TableHead></TableRow></TableHeader>
            <TableBody>
              {list.map((id) => {
                const decl = markings?.declared.get(id)
                const roles = revealRoles(markings, id)
                const fields = carriers.get(id) ?? []
                return (
                  <TableRow key={id} data-marking-row={id}>
                    <TableCell className="align-top">
                      <div className="flex flex-col items-start gap-1">
                        <MarkingChip marking={id} catalog={markings} />
                        <span className="font-mono text-meta text-muted-foreground">{id}{decl?.severity ? ` · ${decl.severity}` : ""}</span>
                      </div>
                    </TableCell>
                    <TableCell className="align-top whitespace-normal">{projectionWords(decl)}</TableCell>
                    <TableCell className="align-top whitespace-normal">
                      {fields.length === 0 ? <span className="text-muted-foreground">No fact carries it yet</span> : (
                        <ul className="space-y-0.5">
                          {fields.map(({ type, typeTitle, field, label }) => (
                            <li key={`${type}.${field}`}><Link to="/objects/$type" params={{ type }} className="underline-offset-4 hover:underline">{typeTitle}</Link><span className="text-muted-foreground"> · {label}</span></li>
                          ))}
                        </ul>
                      )}
                    </TableCell>
                    <TableCell className="align-top whitespace-normal">
                      {roles.length === 0 ? <span className="text-muted-foreground">No role: nobody may reveal it</span>
                        : <span className="inline-flex flex-wrap gap-1">{roles.map((role) => <Chip key={role} tone="blue">{role}</Chip>)}</span>}
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table></TableCard>
        )}
      </LoadState>
      <Panel icon={ReceiptText} label="Reveal receipts">
        <p className="px-1 text-body text-muted-foreground">
          A reveal asks the cell, which writes a ledger receipt (who, which fact, when and why) before it answers; the receipt number shows beside the revealed value.
          A list of every reveal in this workspace is not served by the cell yet.
        </p>
      </Panel>
    </section>
  )
}
