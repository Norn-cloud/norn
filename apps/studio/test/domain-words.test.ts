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

/** Split one compound identifier (`payrollRun`, `hr_manager`, `ShiftSchedule`) into word sub-tokens. */
export function splitIdentifier(ident: string): readonly string[] {
  return ident
    .split(/[_-]+/)
    .flatMap((part) =>
      part
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
    // 1. Check each compound identifier independently (catches camelCase/snake_case, including `hrManager`/`hr_manager`).
    const idents = line.match(/[A-Za-z0-9_-]+/g) ?? []
    for (const ident of idents) {
      const parts = splitIdentifier(ident)
      for (let i = 0; i < parts.length; i += 1) {
        const token = parts[i]!
        for (const rule of SINGLE_TOKEN_DOMAIN) {
          if (rule.match(token)) hits.push({ line: index + 1, word: `${token} (${rule.name})`, text: line.trim() })
        }
        if (parts.length >= 2 && /^hr$/i.test(token) && token !== "HR") {
          hits.push({ line: index + 1, word: `${ident} (HR identifier)`, text: line.trim() })
        }
        const next = parts[i + 1]
        if (next !== undefined && /^(shift|leave)$/i.test(token) && SHIFT_LEAVE_SECOND.test(next)) {
          hits.push({ line: index + 1, word: `${token} ${next} (shift/leave domain term)`, text: line.trim() })
        }
      }
    }
    // 2. Check space-separated prose phrases without crossing statement punctuation (`;`, `(`, `)`, `.`, `,`, `{`, `}`).
    for (const phrase of line.split(/[^A-Za-z0-9_\-\s]+/)) {
      const words = phrase.trim().split(/\s+/).filter(Boolean)
      for (let i = 0; i < words.length - 1; i += 1) {
        const a = words[i]!
        const b = words[i + 1]!
        if (/^(shift|leave)$/i.test(a) && SHIFT_LEAVE_SECOND.test(b)) {
          hits.push({ line: index + 1, word: `${a} ${b} (shift/leave domain term)`, text: line.trim() })
        }
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
    "payrollRun", "employee_roster", "ShiftSchedule", "shift_schedule", "HRManager",
    "hrManager", "hr_manager", "getHRData", "rosterGrid", "scrapYield",
  ]) {
    expect(domainWords(hit), `expected hit on ${hit}`).not.toHaveLength(0)
  }
  for (const miss of [
    "pending.shift()", "queue.shift()", "list.shift(); schedule()", "a.shift(), schedule()",
    "onMouseLeave", "onPointerLeave", "leave the draft as it is",
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
