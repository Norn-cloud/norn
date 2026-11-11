import { useMutation } from "@tanstack/react-query"
import { Play } from "lucide-react"
import { ValidationPanel } from "@/components/editor"
import { Button } from "@/components/ui/button"
import { evaluateQuestion, traceQuestion, type Evaluation } from "@/lib/evaluate"
import { readDocument } from "@/lib/exact"
import { NO_DATA_CELL } from "@/lib/definitions"
import type { QuestionAnswer } from "@/lib/transport"
import { fieldOf, type QuestionCatalog } from "./builder.tsx"
import { AnswerView, RefusedNote } from "./answer-view.tsx"

/** Each bound the cell enforces, in words, for a 413 that carries no message of its own. */
const BOUNDS: Record<string, string> = {
  "core.compute.bound_exceeded": "it would read more records than the cell allows",
  "question.bucket_bound_exceeded": "it would answer with more buckets than the cell allows",
  "request.too_large": "the question is larger than the cell accepts",
}

/** What a run that produced no answer says: never an empty chart, never a guess. */
export function notRun(result: Exclude<Evaluation, { kind: "answer" }>): string {
  if (result.kind === "not-a-question") return "The cell refused this question."
  if (result.kind === "refused") return result.refusal.message
  if (result.kind === "too-large") return `Too large to run: ${result.message || BOUNDS[result.code] || "over the cell's bounds"}.`
  if (result.kind === "bad-request") return `Not run: the cell refused the request${result.message ? ` (${result.message})` : ""}.`
  if (result.reason === "no-cell") return NO_DATA_CELL
  return result.status === 404 || result.status === 0 ? "Not run: the evaluate route isn't connected yet." : result.status === 401 ? "Not run: sign in again." : `Not run: the cell did not answer (${result.status}).`
}

/** The answer document and the declarations it is read with, as the answer view takes them. */
export const answerRead = (question: unknown, catalog: QuestionCatalog, result: Extract<Evaluation, { kind: "answer" }>): QuestionAnswer => ({
  question: readDocument(JSON.stringify(question), "live"),
  answer: result.read,
  metrics: readDocument(JSON.stringify(catalog.declarations.metrics), "live"),
  objectType: readDocument(JSON.stringify(catalog.declarations.objectType), "live"),
} as QuestionAnswer)

/**
 * Run one admitted question in the cell: its answer, as a published one reads
 * (withheld and suppressed values included), or why there is none.
 */
export function RunQuestion({ question, catalog, label = "Run", compact = false }: { readonly question: unknown; readonly catalog: QuestionCatalog; readonly label?: string; readonly compact?: boolean }) {
  const run = useMutation({ mutationFn: () => evaluateQuestion(question) })
  const result = run.data
  return (
    <div className="space-y-3">
      <Button type="button" size="sm" variant={compact ? "outline" : "default"} disabled={run.isPending} onClick={() => run.mutate()}><Play />{run.isPending ? "Running…" : result ? "Run again" : label}</Button>
      {run.isError && <p role="alert" className="text-sm text-destructive">Not run: {run.error instanceof Error ? run.error.message : String(run.error)}</p>}
      {result?.kind === "answer" && <AnswerView read={answerRead(question, catalog, result)} heading="h3" fetchTrace={(lineageRef, bucket) => traceQuestion(question, lineageRef, bucket)} />}
      {result?.kind === "not-a-question" && <ValidationPanel title="The cell refused this question" issues={result.findings.map(({ code, message, pointer }) => ({ code, message, field: fieldOf(pointer) }))} />}
      {result?.kind === "refused" && <RefusedNote refusal={result.refusal} />}
      {result && result.kind !== "answer" && result.kind !== "refused" && result.kind !== "not-a-question" && <p role="status" className="text-sm text-muted-foreground">{notRun(result)}</p>}
    </div>
  )
}
