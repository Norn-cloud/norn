/** One artifact envelope of the world, as JSON. */
export interface Envelope {
  readonly artifactId: string
  readonly kind: string
  readonly name?: string
  readonly payload: Record<string, unknown>
  readonly [member: string]: unknown
}

/** A change an agent proposes: set the value at a JSON pointer inside one artifact. */
export interface Proposal {
  readonly artifact: string
  readonly pointer: string
  readonly value: unknown
}

/** One checker finding (`norn-checker/diagnostics@1`). */
export interface Finding {
  readonly code: string
  readonly tier: string | null
  readonly severity: "error" | "warning" | "info" | string
  readonly message: string
  readonly span: { readonly pointer: string; readonly source: unknown } | null
  readonly artifactId: string | null
  readonly remediation: string | null
}

/** The checker's answer to one candidate set (`norn-checker-abi/check@1`). */
export interface CheckResult {
  readonly admitted: boolean
  readonly diagnostics: { readonly diagnostics: readonly Finding[] }
  readonly receiptSha256: string | null
}

/** The law a refusal names, in the words its definition gives. */
export interface Law {
  readonly artifactId: string
  readonly name: string | undefined
  readonly rationale: string | undefined
  readonly expression: string | undefined
}

/** Where the checker stopped, read from the finding codes it returned. */
export type Stage = "shape" | "resolution" | "model" | "admitted"

/** What the checker decided about one proposal; Studio adds nothing it did not say. */
export interface Verdict {
  readonly proposal: Proposal
  readonly admitted: boolean
  readonly stage: Stage
  readonly refusals: readonly (Finding & { readonly law: Law | undefined })[]
  readonly receiptSha256: string | null
}

export class ProposalError extends Error {}

const token = (part: string) => part.replaceAll("~1", "/").replaceAll("~0", "~")

/** The world with the proposal applied; the world itself is left untouched. */
export function applyProposal(world: readonly Envelope[], proposal: Proposal): Envelope[] {
  if (!proposal.pointer.startsWith("/")) throw new ProposalError(`A pointer starts with "/": ${proposal.pointer}`)
  const next = structuredClone(world) as Envelope[]
  const target = next.find(({ artifactId }) => artifactId === proposal.artifact)
  if (target === undefined) throw new ProposalError(`No artifact ${proposal.artifact} in this world.`)
  const parts = proposal.pointer.slice(1).split("/").map(token)
  const field = parts.pop() ?? ""
  let parent: unknown = target
  for (const part of parts) {
    parent = parent !== null && typeof parent === "object" ? (parent as Record<string, unknown>)[part] : undefined
    if (parent === undefined) throw new ProposalError(`${proposal.pointer} does not exist in ${proposal.artifact}.`)
  }
  if (parent === null || typeof parent !== "object") throw new ProposalError(`${proposal.pointer} does not exist in ${proposal.artifact}.`)
  if (Array.isArray(parent)) {
    const index = field === "-" ? parent.length : Number(field)
    if (!Number.isInteger(index) || index < 0 || index > parent.length) throw new ProposalError(`${proposal.pointer} is not a position in a list.`)
    parent[index] = proposal.value
  } else {
    (parent as Record<string, unknown>)[field] = proposal.value
  }
  return next
}

const lawOf = (world: readonly Envelope[], artifactId: string | null): Law | undefined => {
  const law = world.find((envelope) => envelope.artifactId === artifactId && envelope.kind === "law")
  if (law === undefined) return undefined
  const response = law.payload.response as { expression?: unknown } | undefined
  return {
    artifactId: law.artifactId,
    name: law.name,
    rationale: typeof law.payload.rationale === "string" ? law.payload.rationale : undefined,
    expression: typeof response?.expression === "string" ? response.expression : undefined,
  }
}

/** Finding codes name the checker phase that produced them. */
export function stageOf(findings: readonly Finding[], admitted: boolean): Stage {
  if (admitted) return "admitted"
  const errors = findings.filter(({ severity }) => severity === "error")
  if (errors.some(({ code }) => code.startsWith("artifact_set.") || code.includes(".resolution."))) return "resolution"
  if (errors.some(({ code }) => code.includes(".model.") || code.startsWith("law."))) return "model"
  return "shape"
}

/** The checker's result, with each refusal tied to the law it names. */
export function verdictOf(world: readonly Envelope[], proposal: Proposal, result: CheckResult): Verdict {
  const findings = result.diagnostics.diagnostics
  return {
    proposal,
    admitted: result.admitted,
    stage: stageOf(findings, result.admitted),
    refusals: findings.filter(({ severity }) => severity === "error").map((finding) => ({ ...finding, law: lawOf(world, finding.artifactId) })),
    receiptSha256: result.receiptSha256,
  }
}

/** A short list of the world's artifacts for the agent to read. */
export const worldIndex = (world: readonly Envelope[]) => world.map(({ artifactId, kind, name, payload }) => ({
  artifactId,
  kind,
  name,
  ...kind === "law" ? { rationale: payload.rationale, expression: (payload.response as { expression?: unknown } | undefined)?.expression } : {},
}))
