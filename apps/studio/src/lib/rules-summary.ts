import type { Law } from "./catalog.ts"

/** The parts of an inbox held-back group the summary reads (`GET /v1/inbox`, `kind: "held_back"`). */
export interface HeldGroup {
  readonly ruleRef: { readonly kind: string; readonly artifactId?: string; readonly lawId?: string }
  readonly count: number
  readonly approximate: boolean
}

/** The inbox's held-back section: its record count and whether a scan bound was reached. */
export interface HeldSection {
  readonly count: number
  readonly approximate: boolean
}

export interface RuleState {
  readonly law: Law
  /** Records the inbox lists as held back by this rule, summed over the types it governs. */
  readonly held: number
  readonly approximate: boolean
}

/**
 * The top bar's rules summary, from two server reads only: the laws the catalog lists and the
 * held-back groups the inbox returns. The cell serves no per-rule "satisfied" verdict, so nothing
 * here claims one: a rule the inbox lists is holding records back; any other rule carries no state.
 *
 * `holding` is how many rules the inbox lists as holding records back. It is a lower bound
 * (`partial`) when the cell reached its scan bound or listed fewer groups than its section counted.
 * `clear` is true only when the inbox's held-back section counted no records at all, exactly.
 */
export interface RulesSummary {
  readonly rules: readonly RuleState[]
  readonly holding: number
  readonly partial: boolean
  readonly clear: boolean
}

/** Whether an inbox group's rule is this law: by its artifact when both name one, else by the law's id. */
const governs = (law: Law, group: HeldGroup): boolean =>
  group.ruleRef.kind === "law"
  && ((law.artifactId !== undefined && group.ruleRef.artifactId === law.artifactId) || group.ruleRef.lawId === law.id)

export function rulesSummary(laws: readonly Law[], groups: readonly HeldGroup[] | undefined, section: HeldSection | undefined): RulesSummary {
  const states = laws.map((law): RuleState => {
    const mine = (groups ?? []).filter((group) => governs(law, group))
    return { law, held: mine.reduce((sum, group) => sum + group.count, 0), approximate: mine.some((group) => group.approximate) }
  })
  const listed = (groups ?? []).reduce((sum, group) => sum + group.count, 0)
  const partial = section !== undefined && (section.approximate || listed < section.count)
  // Holding first, the most held first; the rest keep the catalog's order.
  const rules = [...states].sort((a, b) => (b.held > 0 ? 1 : 0) - (a.held > 0 ? 1 : 0) || b.held - a.held)
  return {
    rules,
    holding: states.filter(({ held }) => held > 0).length,
    partial,
    clear: section !== undefined && !section.approximate && section.count === 0,
  }
}
