import type { Artifact } from "./transport.ts"

/**
 * PRIVACY-1 declarations as the server admitted them, for display only: the marking catalog (each
 * marking's label, how a marked value is projected, its severity) and the reveal policy's rules
 * (which roles may reveal which markings). Whether this reader may reveal a value is never decided
 * here: each record's `masked` map carries the server's answer (`revealable`), and the reveal itself
 * is the server's call, recorded as a receipt.
 */
export interface MarkingDecl {
  readonly id: string
  readonly label: string
  readonly category: string | undefined
  readonly severity: string | undefined
  /** How a marked value reaches a reader without the right to see it: "pseudonym", "refuse", … */
  readonly mask: string | undefined
  /** What a pseudonym names ("Customer" → "Customer #…"). */
  readonly subject: string | undefined
}

/** One policy rule that grants `norn.reveal`: the roles it names and the markings it covers. */
export interface RevealRule {
  readonly policyId: string
  readonly ruleId: string
  readonly roles: readonly string[]
  readonly markings: readonly string[]
}

export interface MarkingCatalog {
  readonly declared: ReadonlyMap<string, MarkingDecl>
  readonly reveal: readonly RevealRule[]
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)
const text = (value: unknown): string | undefined => typeof value === "string" ? value : undefined
const strings = (value: unknown): readonly string[] => Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : []

/** Every `anyOf` list under `key` in a rule's conditions (`allOf`/`anyOf` nest them). */
function anyOf(condition: unknown, key: "role" | "marking"): string[] {
  if (Array.isArray(condition)) return condition.flatMap((item) => anyOf(item, key))
  if (!isRecord(condition)) return []
  const own = isRecord(condition[key]) ? [...strings((condition[key] as Record<string, unknown>).anyOf), ...text((condition[key] as Record<string, unknown>).equals) ? [text((condition[key] as Record<string, unknown>).equals)!] : []] : []
  return [...own, ...anyOf(condition.allOf, key), ...anyOf(condition.anyOf, key)]
}

/** The marking catalog and reveal rules the admitted artifacts declare; empty when none do. */
export function markingCatalog(artifacts: readonly Artifact[]): MarkingCatalog {
  const declared = new Map<string, MarkingDecl>()
  const reveal: RevealRule[] = []
  for (const artifact of artifacts) {
    const payload = artifact.payload as unknown
    if (!isRecord(payload)) continue
    if (artifact.kind === "entity" && Array.isArray(payload.markings)) {
      for (const marking of payload.markings) {
        if (!isRecord(marking) || typeof marking.id !== "string") continue
        const mask = isRecord(marking.mask) ? marking.mask : {}
        declared.set(marking.id, { id: marking.id, label: text(marking.label) ?? marking.id, category: text(marking.category), severity: text(marking.severity), mask: text(mask.kind), subject: text(mask.subject) })
      }
    }
    if (artifact.kind === "policy" && Array.isArray(payload.rules)) {
      for (const rule of payload.rules) {
        if (!isRecord(rule) || !strings(rule.actions).includes("norn.reveal")) continue
        reveal.push({ policyId: text(payload.policyId) ?? artifact.artifactId, ruleId: text(rule.ruleId) ?? "", roles: [...new Set(anyOf(rule, "role"))], markings: [...new Set(anyOf(rule, "marking"))] })
      }
    }
  }
  return { declared, reveal }
}

/** A marking as the reader sees it: its declared label, or its id when the catalog does not declare it. */
export const markingLabel = (catalog: MarkingCatalog | undefined, id: string): string => catalog?.declared.get(id)?.label ?? id

/** How a marked value is shown to a reader who may not see it, in words. */
export function projectionWords(decl: MarkingDecl | undefined): string {
  switch (decl?.mask) {
    case "pseudonym": return decl?.subject ? `Shown as a stable pseudonym ("${decl.subject} #…")` : "Shown as a stable pseudonym"
    case "refuse": return "Never shown; the server withholds it"
    case "redact": case "redacted": return "Shown as •••"
    case "last-n": case "last_n": return "Only the last characters are shown"
    case undefined: return "Not declared in this workspace's marking catalog"
    default: return `Projected as ${decl?.mask}`
  }
}

/** The roles the reveal policy names for `marking`, in declared order. */
export const revealRoles = (catalog: MarkingCatalog | undefined, marking: string): readonly string[] =>
  [...new Set((catalog?.reveal ?? []).filter(({ markings }) => markings.length === 0 || markings.includes(marking)).flatMap(({ roles }) => roles))]
