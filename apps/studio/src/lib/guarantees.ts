import type { Schemas } from "./transport.ts"

type Scalar = Schemas["NornGuaranteesGuaranteeScalar"]
type Verdict = Schemas["NornGuaranteesGuaranteeVerdict"]
type Level = Schemas["NornGuaranteesGuaranteeLevel"]

/** An exact scalar as text with its unit: coefficient `125`, scale 2, `g` → `1.25 g`. */
export function scalar({ coefficient, scale, unit }: Scalar): string {
  const negative = coefficient.startsWith("-")
  const digits = (negative ? coefficient.slice(1) : coefficient).padStart(scale + 1, "0")
  const whole = scale > 0 ? `${digits.slice(0, digits.length - scale)}.${digits.slice(digits.length - scale)}` : digits
  return `${negative ? "-" : ""}${whole}${unit ? ` ${unit.code}` : ""}`
}

/** The server's verdict in words; the reason for an unknown is shown as the server wrote it. */
export const verdictWords = (verdict: Verdict): string =>
  verdict.kind === "bounded-pass" ? "Holds in every checked case" : verdict.kind === "refuted" ? "Refuted" : "Unknown"

/** The server's evidence level in words. */
export const levelWords = (level: Level): string => ({
  "exhaustive-finite/bounded": "Proved on the model (every case within the bounds)",
  "runtime-enforced": "Enforced at runtime",
  "parity-tested": "Tested for parity",
  unknown: "No evidence",
})[level.kind]

/** A property name in words: `input_fine_g` → `input fine g`. */
export const field = (name: string): string => name.replaceAll("_", " ")

/** How many cases the checker's declared bounds enabled, as the server recorded them; `undefined` when the bounds name none. */
export function cases(boundsJson: string): number | undefined {
  const bounds = JSON.parse(boundsJson) as { declaredBoundsProfile?: { enabledCases?: unknown } }
  const count = bounds.declaredBoundsProfile?.enabledCases
  return typeof count === "number" ? count : undefined
}

/** A hash that names one guarantee document (`#guarantee-original`). */
export function targetsGuarantee(hash: string): boolean {
  return hash.startsWith("guarantee-")
}

/**
 * The technical details' open state once the hash moves: a guarantee link
 * opens them; any other hash (Back to the page, an unrelated anchor) keeps
 * what the reader chose.
 */
export function openAfterHash(open: boolean, hash: string): boolean {
  return open || targetsGuarantee(hash)
}

type RuleRef = Schemas["NornIrDataQualityRuleRef"]
type HeldGroupSummary = Pick<Schemas["NornHeldBackGroup"], "objectType" | "ruleRef" | "title" | "rationale" | "count" | "approximate">
type InboxSectionSummary = Pick<Schemas["NornInboxSection"], "count" | "shown" | "approximate">

/** Plain-words titles for the engine's own built-in data-quality rules (Business view). */
const BUILTIN_RULE_TITLES: Readonly<Record<string, string>> = {
  "core.boundary.link_held": "Held link",
  "core.boundary.import_finding": "Import check",
  "core.observation.source_disagreement": "Source disagreement",
  "norn.values.property_validity": "Property validity",
  "norn.temporal.property_validity": "Temporal validity",
  "norn.lifecycle.observed_out_of_band": "Observed lifecycle state",
  "norn.invariant.check": "Invariant check",
}

const RULE_KIND_FALLBACK: Readonly<Record<RuleRef["kind"], string>> = {
  builtin: "Built-in rule",
  invariant: "Type invariant",
  law: "Unnamed rule",
}

/** A held-back count as text, preserving the server's lower-bound `+` when its scan bound was reached. */
export const heldCountText = (count: number, approximate: boolean): string => `${count}${approximate ? "+" : ""}`

/** The stable engine identifier for a held-back rule reference (shown in Technical view only). */
export function heldRuleTechnicalId(ruleRef: RuleRef): string {
  if (ruleRef.kind === "law") return ruleRef.lawId
  if (ruleRef.kind === "invariant") return ruleRef.invariantId
  return ruleRef.id
}

/** Stable key for a held-back group's type and rule reference. */
export function heldGroupKey(group: Pick<HeldGroupSummary, "objectType" | "ruleRef">): string {
  return `${group.objectType}:${group.ruleRef.kind}:${heldRuleTechnicalId(group.ruleRef)}`
}

/** Turn a structured engine id (`docs.ScannedDocument.unmatched_check`, `non_negative_quantity`) into plain words. */
function humanizeRuleId(id: string): string | undefined {
  const structured = /[._-]/.test(id) || /[a-z0-9][A-Z]/.test(id)
  if (!structured) return undefined
  const leaf = id.slice(id.lastIndexOf(".") + 1)
  const words = leaf
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .replace(/[_-]+/g, " ")
    .trim()
  if (words === "" || words === id || !/[A-Za-z]/.test(words)) return undefined
  return words.charAt(0).toUpperCase() + words.slice(1).toLowerCase()
}

/**
 * A held-back group's title for Business view: keeps authored rule titles, falls back to rationale
 * or a plain-words label when the server returned a raw engine ID (built-ins, invariants, or
 * unmatched laws), keeping the raw ID under Technical view.
 */
export function heldRuleTitle(group: Pick<HeldGroupSummary, "ruleRef" | "title" | "rationale">): string {
  const technicalId = heldRuleTechnicalId(group.ruleRef)
  const title = group.title.trim()
  const isRawId = title === ""
    || title === technicalId
    || (group.ruleRef.kind === "law" && title === group.ruleRef.artifactId)
  if (!isRawId) return title
  const rationale = group.rationale?.trim()
  if (rationale && rationale !== technicalId) return rationale
  if (group.ruleRef.kind === "builtin") {
    const known = BUILTIN_RULE_TITLES[group.ruleRef.id]
    if (known !== undefined) return known
  }
  return humanizeRuleId(technicalId) ?? RULE_KIND_FALLBACK[group.ruleRef.kind]
}

const groupWord = (count: number) => count === 1 ? "type-rule group" : "type-rule groups"

/** Subtitle for the Rules page's per-rule held-back KPI tile. */
export function heldKpiSub(
  section: InboxSectionSummary,
  groups: readonly Pick<HeldGroupSummary, "count">[],
  scanLimit?: number,
): string {
  const bound = scanLimit ? ` (${scanLimit.toLocaleString("en-US")} rows)` : ""
  if (section.count === 0) {
    return section.approximate
      ? `scan bound${bound} reached · scan incomplete`
      : "no record held back"
  }
  const shownGroups = groups.length
  const shownHolds = groups.reduce((sum, group) => sum + group.count, 0)
  const truncated = shownHolds < section.count
  const countScope = shownGroups === 0
    ? `showing 0 of ${heldCountText(section.count, section.approximate)}`
    : truncated
      ? `showing ${shownHolds} of ${heldCountText(section.count, section.approximate)} in top ${shownGroups} ${groupWord(shownGroups)}`
      : `showing ${shownGroups} of ${heldCountText(shownGroups, section.approximate)} ${groupWord(shownGroups)}`
  return section.approximate
    ? `at least · ${countScope} · scan bound${bound} reached`
    : countScope
}

/** Summary line above the Held back tab's groups: per-rule counting and shown vs total scope. */
export function heldTabSummary(
  section: InboxSectionSummary,
  groups: readonly Pick<HeldGroupSummary, "count">[],
  scanLimit?: number,
): string {
  const shownGroups = groups.length
  const shownHolds = groups.reduce((sum, group) => sum + group.count, 0)
  const truncated = shownHolds < section.count
  const totalText = heldCountText(section.count, section.approximate)
  const bound = section.approximate
    ? `; scan bound${scanLimit ? ` (${scanLimit.toLocaleString("en-US")} rows)` : ""} reached`
    : ""
  if (truncated) {
    return `Showing top ${shownGroups} ${groupWord(shownGroups)} (${shownHolds} of ${totalText} per-rule holds; a record held by two rules counts once per rule${bound}).`
  }
  const holdNoun = section.count === 1 && !section.approximate ? "per-rule hold" : "per-rule holds"
  return `Showing ${shownGroups} of ${heldCountText(shownGroups, section.approximate)} ${groupWord(shownGroups)} (${totalText} ${holdNoun}; a record held by two rules counts once per rule${bound}).`
}
