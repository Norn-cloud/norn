import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { cases, field, levelWords, openAfterHash, scalar, targetsGuarantee, verdictWords } from "../src/lib/guarantees.ts"
import type { Guarantees } from "../src/lib/transport.ts"

// Recorded model-checker documents (fixtures/demo/guarantees).
const recorded = (name: string) =>
  JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo/guarantees", `constitution.${name}.guarantees.json`), "utf8")) as Guarantees

test("the declared law holds on every eligible goal: 3 of 3 discharged, 7 of 10 unknown", () => {
  const original = recorded("original")
  expect(verdictWords(original.verdict)).toBe("Holds in every checked case")
  expect([original.discharged, original.eligible, original.total]).toEqual([3, 3, 10])
  expect(original.goals.filter(({ verdict }) => verdict.kind === "unknown")).toHaveLength(7)
  expect(levelWords(original.level)).toBe("Proved on the model (every case within the bounds)")
})

test("the broken copy is refuted by the exact counterexample 2 g in, 1 g out, 0 g loss", () => {
  const mutant = recorded("mutant")
  expect(verdictWords(mutant.verdict)).toBe("Refuted")
  const scenario = mutant.goals.find(({ scenario }) => scenario?.kind === "counterexample")?.scenario
  const after = scenario?.states.at(-1)?.values ?? {}
  expect(Object.entries(after).map(([name, value]) => `${field(name)} ${scalar(value)}`).sort())
    .toEqual(["input fine g 2 g", "loss fine g 0 g", "output fine g 1 g"])
})

test("a scaled coefficient reads as exact decimal text", () => {
  expect(scalar({ coefficient: "125", scale: 2, unit: { code: "g", system: "ucum" } })).toBe("1.25 g")
  expect(scalar({ coefficient: "-5", scale: 3 })).toBe("-0.005")
})

test("the declared bounds name how many cases were checked", () => {
  expect(cases(recorded("original").boundsJson)).toBe(77)
  expect(cases(JSON.stringify({ profile: {} }))).toBeUndefined()
})

test("a guarantee link opens the details; any other hash keeps the reader's choice", () => {
  expect(targetsGuarantee("guarantee-original")).toBe(true)
  expect(targetsGuarantee("")).toBe(false)
  expect(openAfterHash(false, "guarantee-mutant")).toBe(true)
  // Back from a guarantee link to the bare page leaves opened details open.
  expect(openAfterHash(true, "")).toBe(true)
  // A closed disclosure stays closed on an unrelated anchor.
  expect(openAfterHash(false, "summary")).toBe(false)
})

test("heldBy matches on both rule and object type, and lower-bound counts keep their + suffix", async () => {
  const { heldBy, heldCountText } = await import("../src/laws.tsx")
  const { recordName } = await import("../src/lib/objects.ts")
  const law = { id: "canon.ItemizedDocument.total_matches_lines", artifactId: "canon.ItemizedDocument.total_matches_lines.law", subject: "canon.ItemizedDocument", rationale: "r", severity: "error" as const, modality: "always", response: "predicate", owner: "canon" }
  const groups = [
    { kind: "held_back" as const, ruleRef: { kind: "law" as const, artifactId: law.artifactId, lawId: law.id, version: "1" }, title: "t", objectType: "docs.LedgerEntry", count: 5, approximate: false, target: "/objects/docs.LedgerEntry?population=held_back", records: [] },
    { kind: "held_back" as const, ruleRef: { kind: "law" as const, artifactId: law.artifactId, lawId: law.id, version: "1" }, title: "t", objectType: "docs.ScannedDocument", count: 500, approximate: true, target: "/objects/docs.ScannedDocument?population=held_back", records: [] },
  ]
  expect(heldBy(groups, law, "docs.ScannedDocument")?.count).toBe(500)
  expect(heldBy(groups, law, "docs.LedgerEntry")?.count).toBe(5)
  expect(heldBy(groups, law, "canon.Item")).toBeUndefined()
  expect(heldCountText(1, false)).toBe("1")
  expect(heldCountText(500, true)).toBe("500+")
  // Title resolution matches main's record page: label, display_name, else id (never guessing doc_number).
  expect(recordName({ id: "SCAN-1", values: { doc_number: "INV-1" } })).toBe("SCAN-1")
  expect(recordName({ id: "SCAN-1", values: { label: "October invoice", doc_number: "INV-1" } })).toBe("October invoice")
  expect(recordName({ id: "SCAN-1", values: { label: "", display_name: "Fallback name" } })).toBe("Fallback name")
  expect(recordName({ id: "SCAN-1", values: null })).toBe("SCAN-1")
  expect(recordName({ id: "SCAN-1" })).toBe("SCAN-1")
})
