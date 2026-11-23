import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"

/**
 * Studio is domain-agnostic: it renders whatever business model the cell admits, so no business
 * domain is named in its source. Every view, form and timeline has to work for any pack; a
 * domain's words belong in its pack (registry/packs) and its test fixtures, never in
 * apps/studio/src. This gate fails on a domain identifier or user-visible domain word there.
 *
 * Identifiers are split on snake_case, kebab-case, camelCase and PascalCase boundaries, so
 * `payrollRun`, `employee_roster`, `ShiftSchedule` and `HRManager` are all caught while
 * `Array.prototype.shift`, `onMouseLeave`, "leave" in prose, the Shift key and `<hr>` are not.
 * There is no allowlist; a hit is fixed in the source.
 */
const SINGLE_TOKEN_DOMAIN: readonly { readonly name: string; readonly match: (token: string) => boolean }[] = [
  { name: "payroll", match: (t) => /^payrolls?$/i.test(t) },
  { name: "roster", match: (t) => /^rosters?$|^rostering$/i.test(t) },
  { name: "jewelry", match: (t) => /^jewel/i.test(t) },
  { name: "scrap", match: (t) => /^scraps?$/i.test(t) },
  { name: "melt", match: (t) => /^melt(s|ed|ing)?$/i.test(t) },
  { name: "HR", match: (t) => t === "HR" },
]

const SHIFT_LEAVE_SECOND = /^(requests?|commitments?|requirements?|balances?|entitlements?|ledgers?|polic(y|ies)|types?|swaps?|schedules?|schedulers?|scheduling|rosters?|rostering|assignments?|templates?|patterns?|plans?|accruals?|approvals?|handovers?|coverage|differentials?)$/i

const LINE_PATTERNS: readonly { readonly name: string; readonly pattern: RegExp }[] = [
  { name: "the HR namespace", pattern: /\bhr\.[A-Za-z]|\bpacks\/hr\b/ },
]

/** Split one line into ordered word tokens across punctuation, snake_case, kebab-case, and camelCase/PascalCase boundaries. */
export function tokenizeLine(line: string): readonly string[] {
  const words = line.match(/[A-Za-z0-9]+/g) ?? []
  return words.flatMap((word) =>
    word
      .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
      .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
      .split(" ")
      .filter(Boolean),
  )
}

/** Every domain word on each line of `text`, by line number. */
export function domainWords(text: string): readonly { readonly line: number; readonly word: string; readonly text: string }[] {
  return text.split("\n").flatMap((line, index) => {
    const hits: { readonly line: number; readonly word: string; readonly text: string }[] = []
    const tokens = tokenizeLine(line)
    for (let i = 0; i < tokens.length; i += 1) {
      const token = tokens[i]!
      for (const rule of SINGLE_TOKEN_DOMAIN) {
        if (rule.match(token)) hits.push({ line: index + 1, word: `${token} (${rule.name})`, text: line.trim() })
      }
      const next = tokens[i + 1]
      if (next !== undefined && /^(shift|leave)$/i.test(token) && SHIFT_LEAVE_SECOND.test(next)) {
        hits.push({ line: index + 1, word: `${token} ${next} (shift/leave domain term)`, text: line.trim() })
      }
    }
    for (const { name, pattern } of LINE_PATTERNS) {
      const hit = pattern.exec(line)
      if (hit !== null) hits.push({ line: index + 1, word: `${hit[0]} (${name})`, text: line.trim() })
    }
    return hits
  })
}

const SRC = join(import.meta.dir, "..", "src")
/** Build outputs written into src by `bun run checker` (gitignored): not Studio's source. */
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SRC, dir)))
const TEXT = /\.(tsx?|css|json|html|md)$/

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (GENERATED.has(path)) return []
    if (statSync(path).isDirectory()) return files(path)
    return TEXT.test(name) ? [path] : []
  })
}

test("the matcher finds domain identifiers (including camelCase and snake_case) and passes lookalikes", () => {
  for (const hit of [
    "hr.Employee", "registry/packs/hr", "ShiftCommitment", "LeaveRequest", "LeaveBalance",
    "leave_request", "Shift requirement", "payroll", "Roster", "jewelry", "Jewellery",
    "scrap-yield", "gold scrap", "melting", "the HR team",
    "payrollRun", "employee_roster", "ShiftSchedule", "shift_schedule", "HRManager", "getHRData", "rosterGrid", "scrapYield",
  ]) {
    expect(domainWords(hit), `expected hit on ${hit}`).not.toHaveLength(0)
  }
  for (const miss of [
    "pending.shift()", "queue.shift()", "onMouseLeave", "onPointerLeave", "leave the draft as it is",
    "Shift adds a key", "event.shiftKey", "<hr />", "threshold", "shrink", "scrape", "scrapbook",
    "smelt", "href", "chr.code", "thr.x", "leaves",
  ]) {
    expect(domainWords(miss), `expected no hit on ${miss}`).toEqual([])
  }
})

test("apps/studio/src names no business domain", () => {
  const scanned = files(SRC)
  expect(scanned.length).toBeGreaterThan(100)
  const hits = scanned.flatMap((path) => domainWords(readFileSync(path, "utf8")).map((hit) => `${relative(join(SRC, ".."), path)}:${hit.line}: ${hit.word}: ${hit.text}`))
  expect(hits).toEqual([])
})
