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
  /** Each finding as the checker wrote it, with its JSON pointer into the checked document when the checker names one. */
  readonly findings: readonly { readonly code: string; readonly message: string; readonly pointer: string | null }[]
}

type Admit = (artifactsJson: string, metricsJson: string, questionJson: string) => string

/** Admit a proposed question against the analytics world with the checker's `admitQuestion`. */
export function questionVerdict(question: unknown, admit: Admit): QuestionVerdict {
  try {
    const out = JSON.parse(admit(JSON.stringify(ANALYTICS.artifacts), JSON.stringify(ANALYTICS.metrics), JSON.stringify(question))) as {
      admitted?: unknown
      plan?: QuestionVerdict["plan"]
      diagnostics?: { diagnostics?: readonly { code: string; message: string; span?: { pointer?: string } | null }[] }
    }
    return { kind: "question", question, admitted: out.admitted === true, plan: out.plan ?? null, findings: (out.diagnostics?.diagnostics ?? []).map(({ code, message, span }) => ({ code, message, pointer: span?.pointer ?? null })) }
  } catch (error) {
    // Not a QuestionIR at all: the checker's input error, as it wrote it.
    return { kind: "question", question, admitted: false, plan: null, findings: [{ code: "question.shape", message: error instanceof Error ? error.message : String(error), pointer: null }] }
  }
}

/** The question vocabulary of the IR (`norn_ir::metric::TimeGrain`, `FilterOperator`). The checker decides which it admits. */
export const GRAINS = ["day", "week", "month", "quarter", "year"] as const
export const OPERATORS = ["eq", "ne", "lt", "le", "gt", "ge"] as const

interface PropertyType { readonly kind: string; readonly unit?: { readonly code: string } }

/**
 * What a question can be built from: the world's object set, its properties
 * and declared metrics, and the IR's grains and operators. A builder only
 * offers these; whether a combination is a question is the checker's call.
 */
export function questionCatalog() {
  const [entity] = ANALYTICS.artifacts
  const properties = (entity?.payload.objectType.properties ?? []).map(({ id, type }: { id: string; type: PropertyType }) => ({ name: id, kind: type.kind, unit: type.unit?.code ?? null }))
  return {
    objectSet: entity?.name ?? null,
    properties,
    metrics: ANALYTICS.metrics.map(({ name, label, type }) => ({ name, label, type })),
    grains: GRAINS,
    operators: OPERATORS,
    example: ANALYTICS.example,
    // The declarations an answer to a built question is read with: units, ratios and labels.
    declarations: { metrics: ANALYTICS.metrics, objectType: entity?.payload.objectType ?? null },
  }
}

const MAX_QUESTION = 16 * 1024
const refuse = (status: number, reason: string, headers: HeadersInit = {}) =>
  Response.json({ error: reason }, { status, headers: { "cache-control": "no-store", ...headers } })

/**
 * GET /api/questions/catalog and POST /api/questions/check: a question built
 * in Studio, admitted or refused by the server's checker. Nothing is evaluated.
 */
/** Every question route the Worker serves; the Worker's gate covers exactly these. */
export const QUESTION_ROUTES = ["/api/questions/catalog", "/api/questions/recommendations", "/api/questions/check"] as const
export const isQuestionRoute = (pathname: string) => (QUESTION_ROUTES as readonly string[]).includes(pathname)

export async function questionsRoute(request: Request, pathname: string, checker: () => Promise<Admit>): Promise<Response> {
  if (pathname === "/api/questions/catalog") {
    if (request.method !== "GET") return refuse(405, "Use GET.", { allow: "GET" })
    return Response.json(questionCatalog(), { headers: { "cache-control": "no-store" } })
  }
  if (pathname === "/api/questions/recommendations") {
    if (request.method !== "GET") return refuse(405, "Use GET.", { allow: "GET" })
    const { questionRecommendations } = await import("./question-tree.ts")
    return Response.json(questionRecommendations(await checker()), { headers: { "cache-control": "no-store" } })
  }
  if (request.method !== "POST") return refuse(405, "Use POST.", { allow: "POST" })
  const text = await request.text()
  if (text.length > MAX_QUESTION) return refuse(413, "The question is too long.")
  let body: unknown
  try {
    body = JSON.parse(text)
  } catch {
    return refuse(400, "Expected {question}.")
  }
  const question = body !== null && typeof body === "object" ? (body as { question?: unknown }).question : undefined
  if (question === null || typeof question !== "object") return refuse(400, "Expected {question}.")
  // The verdict with the question in plain words (generated from the IR by code) and its hash, for sign-off.
  const { restatement } = await import("./question-tree.ts")
  return Response.json({ ...questionVerdict(question, await checker()), restatement: await restatement(question as never) }, { headers: { "cache-control": "no-store" } })
}
