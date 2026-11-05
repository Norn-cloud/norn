import { useQuery } from "@tanstack/react-query"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { buckets, declarations, fraction, shown, type Bucket } from "./lib/answer.ts"
import { day, headline } from "./lib/format.ts"
import { restated, sameAnswer } from "./lib/restatement.ts"
import { recordedTransport, type Answer } from "./lib/transport.ts"
import { CorrectionsStatus } from "./restatement-status.tsx"

const changes = (count: number) => `${count} committed ${count === 1 ? "change" : "changes"}`

function Reading({ title, when, bucket, note }: { readonly title: string; readonly when: string; readonly bucket: Bucket | undefined; readonly note?: string }) {
  return (
    <div className="space-y-1 rounded-lg border p-4">
      <div className="text-sm font-medium">{title}</div>
      <div className="text-xs text-muted-foreground">{when}</div>
      {bucket
        ? <>
            <div className="pt-2 text-figure leading-none font-normal tracking-[-0.02em] tabular-nums">{shown(bucket)}</div>
            <div className="text-meta text-muted-foreground tabular-nums">{fraction(bucket.totals)}{bucket.totals.length > 1 && ` = ${bucket.value} exactly`}</div>
          </>
        : <div className="pt-2 text-muted-foreground">Not in this read.</div>}
      {note && <div className="pt-2 text-xs text-muted-foreground">{note}</div>}
    </div>
  )
}

const read = (answer: Answer) => `Read ${day(answer.nowMs)}, after ${changes(answer.knownAsOf)}`

export function RestatementCard() {
  const restatement = useQuery({ queryKey: ["restatement"], queryFn: recordedTransport.restatement })
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
          <CardTitle className="text-xl">Corrections to {headline(declared.question).toLowerCase()}</CardTitle>
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
              <Reading title="Published" when={read(answer.document)} bucket={change.published} />
              <Reading title="Corrected" when={read(corrected.document)} bucket={change.corrected} />
              <Reading
                title={`As known on ${day(asOf.document.knowledgeTimeMs)}`}
                when={`Read again after the correction, at the earlier cutoff (${changes(asOf.document.result.knownAsOf)})`}
                bucket={change.asOf}
                note={change.asOf && change.published && (sameAnswer(change.asOf, change.published) ? "Exactly the published answer." : "Differs from the published answer.")}
              />
            </div>
            <Table>
              <TableHeader><TableRow><TableHead>Corrected record</TableHead><TableHead className="text-right">Published</TableHead><TableHead className="text-right">Corrected</TableHead></TableRow></TableHeader>
              <TableBody>
                {change.records.map(({ id, published: was, corrected: is }) => (
                  <TableRow key={id}>
                    <TableCell className="font-mono text-xs">{id}</TableCell>
                    <TableCell className="text-right tabular-nums">{was ? `${fraction(was.amounts)} (version ${was.version})` : "not counted"}</TableCell>
                    <TableCell className="text-right tabular-nums">{is ? `${fraction(is.amounts)} (version ${is.version})` : "not counted"}</TableCell>
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
