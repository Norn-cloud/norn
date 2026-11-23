import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"

/**
 * Studio is domain-agnostic: it renders whatever business model the cell admits, so no business
 * domain is named in its source. Every view, form and timeline has to work for any pack; a
 * domain's words belong in its pack (registry/packs) and its test fixtures, never in
 * apps/studio/src. This gate fails on a domain identifier or user-visible domain word there.
 *
 * The patterns match identifiers and words, not substrings: `Array.prototype.shift`,
 * `onMouseLeave`, "leave" in prose, the Shift key and `<hr>` are not domain words. There is no
 * allowlist; a hit is fixed in the source.
 */
const DOMAIN_WORDS: readonly { readonly name: string; readonly pattern: RegExp }[] = [
  { name: "payroll", pattern: /\bpayrolls?\b/i },
  { name: "roster", pattern: /\brosters?\b/i },
  { name: "jewelry", pattern: /\bjewel/i },
  { name: "scrap", pattern: /\bscraps?\b/i },
  { name: "melt", pattern: /\bmelt(s|ed|ing)?\b/i },
  { name: "shift/leave record types", pattern: /\b(Shift|Leave)(Request|Commitment|Requirement|Balance|Entitlement|Ledger|Polic(y|ies)|Types?|Swaps?)\b/ },
  { name: "shift/leave record words", pattern: /\b(shift|leave)[_ -](requests?|commitments?|requirements?|balances?|entitlements?|ledgers?|polic(y|ies)|types?|swaps?)\b/i },
  { name: "the HR namespace", pattern: /\bhr\.[A-Za-z]|\bpacks\/hr\b/ },
  { name: "HR", pattern: /\bHR\b/ },
]

/** Every domain word on each line of `text`, by line number. */
export function domainWords(text: string): readonly { readonly line: number; readonly word: string; readonly text: string }[] {
  return text.split("\n").flatMap((line, index) => DOMAIN_WORDS.flatMap(({ name, pattern }) => {
    const hit = pattern.exec(line)
    return hit === null ? [] : [{ line: index + 1, word: `${hit[0]} (${name})`, text: line.trim() }]
  }))
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

test("the matcher finds domain identifiers and words, and passes words that only look like them", () => {
  for (const hit of ["hr.Employee", "registry/packs/hr", "ShiftCommitment", "LeaveRequest", "LeaveBalance", "leave_request", "Shift requirement", "payroll", "Roster", "jewelry", "Jewellery", "scrap-yield", "gold scrap", "melting", "the HR team"]) {
    expect(domainWords(hit)).not.toHaveLength(0)
  }
  for (const miss of ["pending.shift()", "queue.shift()", "onMouseLeave", "onPointerLeave", "leave the draft as it is", "Shift adds a key", "<hr />", "threshold", "shrink", "scrape", "scrapbook", "smelt", "href", "chr.code", "thr.x", "leaves"]) {
    expect(domainWords(miss)).toEqual([])
  }
})

test("apps/studio/src names no business domain", () => {
  const scanned = files(SRC)
  expect(scanned.length).toBeGreaterThan(100)
  const hits = scanned.flatMap((path) => domainWords(readFileSync(path, "utf8")).map((hit) => `${relative(join(SRC, ".."), path)}:${hit.line}: ${hit.word}: ${hit.text}`))
  expect(hits).toEqual([])
})
