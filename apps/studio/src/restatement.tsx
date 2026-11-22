import { useQuery } from "@tanstack/react-query"
import { Badge } from "@/components/ui/badge"
import { Chip } from "@/components/chip"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, contribution, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { day, headline } from "./lib/format.ts"
import { restated, sameAnswer } from "./lib/restatement.ts"
import { transport, type Answer } from "./lib/transport.ts"
import { CorrectionsStatus } from "./restatement-status.tsx"
import { cn } from "@/lib/utils"

const changes = (count: number) => `${count} committed ${count === 1 ? "change" : "changes"}`

function Reading({ title, when, bucket, note, tone }: { readonly title: string; readonly when: string; readonly bucket: Bucket | undefined; readonly note?: string; readonly tone: "blue" | "amber" | "gray" }) {
  return (
    <div className={cn("space-y-1.5 rounded-xl border bg-card p-4", tone === "amber" && "border-tone-amber/30 bg-tone-amber/5", tone === "blue" && "border-data-accent/35 bg-data-accent/5")}>
      <Chip tone={tone} dot={tone !== "gray"} className="rounded-full">{title}</Chip>
      <div className="text-xs text-muted-foreground">{when}</div>
      {bucket
        ? <>
            <div className={cn("pt-2 text-figure leading-none font-medium tracking-[-0.02em] tabular-nums", tone === "amber" && "text-tone-amber", tone === "blue" && "text-tone-blue")}>{shown(bucket)}</div>
            <div className="text-meta text-muted-foreground tabular-nums">{fraction(bucket.totals)}{bucket.totals.length > 1 && ` = ${bucket.value} exactly`}</div>
          </>
        : <div className="pt-2 text-muted-foreground">Not in this read.</div>}
      {note && <div className="pt-2 text-xs text-muted-foreground">{note}</div>}
    </div>
  )
}

const read = (answer: Answer) => `Read ${day(answer.nowMs)}, after ${changes(answer.knownAsOf)}`

export function RestatementCard() {
  const restatement = useQuery({ queryKey: ["restatement"], queryFn: transport.restatement })
  if (restatement.isPending) return <CorrectionsStatus />
  if (restatement.isError) return <CorrectionsStatus error={restatement.error} />
  const { answer, corrected, asOf } = restatement.data
  const declared = declarations(restatement.data)
  const published = buckets(declared, { answer: answer.document, numbers: answer.numbers, at: "" })
  const after = buckets(declared, { answer: corrected.document, numbers: corrected.numbers, at: "" })
  const then = buckets(declared, { answer: asOf.document.result, numbers: asOf.numbers, at: "/result" })
  const recorded = [answer, corrected, asOf].some(({ origin }) => origin === "recorded")
  return (
    <Card id="corrections" className="scroll-mt-4">
      <CardHeader>
        <div className="flex flex-wrap items-center gap-2">
          <CardTitle role="heading" aria-level={2} className="text-xl">Corrections to {headline(declared.question).toLowerCase()}</CardTitle>
          {recorded && <Badge variant="outline" title="Documents the engine produced, replayed exactly. Not a live connection.">Recorded</Badge>}
        </div>
        <CardDescription>
          When a source record is corrected, the answer is recalculated. The answer published before the correction stays readable exactly as it was.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-8">
        {restated(published, after, then).map((change) => (
          <div key={change.key} className="space-y-4">
            <h3 className="font-medium">{change.label}</h3>
            <div className="grid gap-4 md:grid-cols-3">
              <Reading title="Published" when={read(answer.document)} bucket={change.published} tone="blue" />
              <Reading title="Corrected" when={read(corrected.document)} bucket={change.corrected} tone="amber" />
              <Reading
                title={`As known on ${day(asOf.document.knowledgeTimeMs)}`}
                when={`Read again after the correction, at the earlier cutoff (${changes(asOf.document.result.knownAsOf)})`}
                bucket={change.asOf}
                tone="gray"
                note={change.asOf && change.published && (sameAnswer(change.asOf, change.published) ? "Exactly the published answer." : "Differs from the published answer.")}
              />
            </div>
            <Table>
              <TableHeader><TableRow><TableHead>Corrected record</TableHead><TableHead className="text-right">Published</TableHead><TableHead className="text-right">Corrected</TableHead></TableRow></TableHeader>
              <TableBody>
                {change.records.map(({ id, published: was, corrected: is }) => (
                  <TableRow key={id}>
                    <TableCell className="font-mono text-xs">{id}</TableCell>
                    <TableCell className="text-right tabular-nums">{was ? `${contribution(was.amounts)} (version ${was.version})` : "not counted"}</TableCell>
                    <TableCell className="text-right tabular-nums">{is ? `${contribution(is.amounts)} (version ${is.version})` : "not counted"}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}
