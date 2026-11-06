import world from "../../../fixtures/demo/analytics.world.json"
import example from "../../../fixtures/demo/answers/scrap-yield.question.json"

/**
 * The analytics world the demo cell answers questions from (demo.Conversion
 * and its yield metrics), and the cell's own admitted question as the example
 * the assistant shapes its proposals on.
 */
export const ANALYTICS = { artifacts: world.artifacts, metrics: world.metrics, example } as const

/** The checker's answer to one proposed question: its checked plan, or what it refused. Never a result. */
export interface QuestionVerdict {
  readonly kind: "question"
  readonly question: unknown
  readonly admitted: boolean
  /** Grain, sums and declarations of the admitted question; `null` when refused. */
  readonly plan: { readonly grain: string; readonly question: unknown; readonly sums: readonly { readonly metric: string; readonly property: string }[]; readonly metrics: readonly { readonly name: string; readonly label: string }[] } | null
  readonly findings: readonly { readonly code: string; readonly message: string }[]
}

type Admit = (artifactsJson: string, metricsJson: string, questionJson: string) => string

/** Admit a proposed question against the analytics world with the checker's `admitQuestion`. */
export function questionVerdict(question: unknown, admit: Admit): QuestionVerdict {
  try {
    const out = JSON.parse(admit(JSON.stringify(ANALYTICS.artifacts), JSON.stringify(ANALYTICS.metrics), JSON.stringify(question))) as {
      admitted?: unknown
      plan?: QuestionVerdict["plan"]
      diagnostics?: { diagnostics?: readonly { code: string; message: string }[] }
    }
    return { kind: "question", question, admitted: out.admitted === true, plan: out.plan ?? null, findings: (out.diagnostics?.diagnostics ?? []).map(({ code, message }) => ({ code, message })) }
  } catch (error) {
    // Not a QuestionIR at all: the checker's input error, as it wrote it.
    return { kind: "question", question, admitted: false, plan: null, findings: [{ code: "question.shape", message: error instanceof Error ? error.message : String(error) }] }
  }
}
