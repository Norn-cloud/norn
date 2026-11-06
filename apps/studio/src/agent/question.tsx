import { CircleCheck, CircleX } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { conceptTitle } from "@/lib/catalog"
import { day } from "@/lib/format"

/** The Worker's answer to one proposed question (worker/questions.ts): a checked plan or the checker's findings. Never a result. */
export interface QuestionVerdict {
  readonly kind: "question"
  readonly question: unknown
  readonly admitted: boolean
  readonly plan: { readonly grain: string; readonly question: QuestionShape; readonly sums: readonly { readonly metric: string; readonly property: string }[]; readonly metrics: readonly { readonly name: string; readonly label: string }[] } | null
  readonly findings: readonly { readonly code: string; readonly message: string }[]
}

interface QuestionShape {
  readonly name?: string
  readonly aggregate?: readonly { readonly metric: string }[]
  readonly groupBy?: readonly { readonly path?: { readonly property?: string }; readonly grain?: string }[]
  readonly filter?: readonly { readonly path?: { readonly property?: string }; readonly operator?: string; readonly value?: unknown }[]
  readonly window?: { readonly start?: string; readonly end?: string }
}

export function questionVerdictOf(output: unknown): QuestionVerdict | undefined {
  const value = output as Partial<QuestionVerdict> | null
  return value?.kind === "question" && typeof value.admitted === "boolean" && Array.isArray(value.findings) ? value as QuestionVerdict : undefined
}

const words = (property: string | undefined) => (property ?? "").replaceAll("_", " ")
const OPERATORS: Record<string, string> = { eq: "is", ne: "is not", lt: "is below", lte: "is at most", gt: "is above", gte: "is at least" }

/** The checked plan in Business words: what it measures, of what, by what, for which records and over which window. */
export function planWords(plan: NonNullable<QuestionVerdict["plan"]>): readonly [string, string][] {
  const question = plan.question
  const selected = question.aggregate?.[0]?.metric
  const measure = plan.metrics.find(({ name }) => name === selected)?.label ?? selected ?? "—"
  const [numerator, denominator] = plan.sums
  return [
    ["Measures", numerator && denominator ? `${measure}: ${words(numerator.property)} ÷ ${words(denominator.property)}` : measure],
    ["Of", `${conceptTitle(plan.grain)} records`],
    ["By", (question.groupBy ?? []).map(({ path, grain }) => grain ? `${grain} of ${words(path?.property)}` : words(path?.property)).join(", ") || "—"],
    ["Only where", (question.filter ?? []).map(({ path, operator, value }) => `${words(path?.property)} ${OPERATORS[operator ?? ""] ?? operator} ${String(value)}`).join(" and ") || "every record"],
    ["Over", question.window?.start && question.window.end ? `${day(question.window.start)} until ${day(question.window.end)}` : "—"],
  ]
}

/** What the checker said about a proposed question. It is checked, not run: there is no number to show. */
export function QuestionCard({ verdict }: { readonly verdict: QuestionVerdict }) {
  if (verdict.admitted && verdict.plan) {
    return (
      <div className="space-y-2 rounded-xl border bg-card p-3 text-sm">
        <div className="flex flex-wrap items-center gap-2 font-medium"><CircleCheck className="size-4 text-success" />Question admitted by the checker<Badge variant="outline">Admitted, not yet run</Badge></div>
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
          {planWords(verdict.plan).map(([label, text]) => <div key={label} className="contents"><dt className="text-muted-foreground">{label}</dt><dd>{text}</dd></div>)}
        </dl>
        <details className="text-meta text-muted-foreground"><summary className="cursor-pointer">Details</summary><pre className="mt-1 whitespace-pre-wrap break-all font-mono">{JSON.stringify(verdict.plan.question, null, 2)}</pre></details>
      </div>
    )
  }
  return (
    <div className="space-y-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
      <div className="flex items-center gap-2 font-medium"><CircleX className="size-4 text-destructive" />Question refused by the checker</div>
      {verdict.findings.map(({ code, message }, index) => <div key={`${code}:${index}`} className="rounded-lg bg-muted px-3 py-2">{message}</div>)}
      <details className="text-meta text-muted-foreground"><summary className="cursor-pointer">Details</summary><div className="mt-1 space-y-1 font-mono break-all">{verdict.findings.map(({ code }, index) => <div key={`${code}:${index}`}>{code}</div>)}</div></details>
    </div>
  )
}
