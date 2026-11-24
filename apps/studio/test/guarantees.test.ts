import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { cases, field, heldCountText, heldGroupKey, heldKpiSub, heldRuleTechnicalId, heldRuleTitle, heldTabSummary, levelWords, openAfterHash, scalar, targetsGuarantee, verdictWords } from "../src/lib/guarantees.ts"
import { draftValueFromJson, suggestedFixInputs, type ActionSummary } from "../src/lib/actions.ts"
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

test("heldRuleTitle keeps authored titles and falls back to plain words for raw engine IDs", () => {
  expect(heldRuleTitle({
    ruleRef: { kind: "law", artifactId: "canon.ItemizedDocument.total_matches_lines.law", lawId: "canon.ItemizedDocument.total_matches_lines", version: "1" },
    title: "An itemized document's total equals the sum of its lines",
    rationale: null,
  })).toBe("An itemized document's total equals the sum of its lines")
  // Law whose title repeats its id falls back to rationale, or humanizes its id when rationale is absent.
  expect(heldRuleTitle({
    ruleRef: { kind: "law", artifactId: "docs.ScannedDocument.total_matches_ledger.law", lawId: "docs.ScannedDocument.total_matches_ledger", version: "1" },
    title: "docs.ScannedDocument.total_matches_ledger",
    rationale: "Total must match the journal entry.",
  })).toBe("Total must match the journal entry.")
  expect(heldRuleTitle({
    ruleRef: { kind: "law", artifactId: "docs.ScannedDocument.unmatched_check.law", lawId: "docs.ScannedDocument.unmatched_check", version: "1" },
    title: "docs.ScannedDocument.unmatched_check",
    rationale: null,
  })).toBe("Unmatched check")
  // Built-ins and invariants never leak raw engine IDs in Business view.
  expect(heldRuleTitle({
    ruleRef: { kind: "builtin", id: "core.boundary.link_held", version: "1" },
    title: "core.boundary.link_held",
    rationale: null,
  })).toBe("Held link")
  expect(heldRuleTitle({
    ruleRef: { kind: "builtin", id: "norn.values.property_validity", version: "1" },
    title: "norn.values.property_validity",
    rationale: null,
  })).toBe("Property validity")
  expect(heldRuleTitle({
    ruleRef: { kind: "invariant", invariantId: "non_negative_quantity", owner: { kind: "object", name: "docs.ScannedDocument" }, version: "1" },
    title: "non_negative_quantity",
    rationale: null,
  })).toBe("Non negative quantity")
  expect(heldRuleTitle({
    ruleRef: { kind: "invariant", invariantId: "INV1", owner: { kind: "object", name: "docs.ScannedDocument" }, version: "1" },
    title: "INV1",
    rationale: null,
  })).toBe("Type invariant")
  expect(heldRuleTechnicalId({ kind: "invariant", invariantId: "non_negative_quantity", owner: { kind: "object", name: "docs.ScannedDocument" }, version: "1" })).toBe("non_negative_quantity")
  expect(heldGroupKey({ objectType: "docs.ScannedDocument", ruleRef: { kind: "invariant", invariantId: "non_negative_quantity", owner: { kind: "object", name: "docs.ScannedDocument" }, version: "1" } }))
    .toBe("docs.ScannedDocument:invariant:non_negative_quantity")
})

test("held section summaries distinguish incomplete empty scans, untruncated groups, and server-truncated groups", () => {
  expect(heldKpiSub({ count: 0, shown: 0, approximate: false }, [], 10000)).toBe("no record held back")
  expect(heldKpiSub({ count: 0, shown: 0, approximate: true }, [], 10000)).toBe("scan bound (10,000 rows) reached · scan incomplete")
  expect(heldKpiSub({ count: 6, shown: 2, approximate: false }, [{ count: 5 }, { count: 1 }], 10000)).toBe("showing 2 of 2 type-rule groups")
  expect(heldKpiSub({ count: 6, shown: 2, approximate: true }, [{ count: 5 }, { count: 1 }], 10000))
    .toBe("at least · showing 2 of 2+ type-rule groups · scan bound (10,000 rows) reached")
  expect(heldKpiSub({ count: 600, shown: 2, approximate: false }, [{ count: 500 }, { count: 5 }], 10000)).toBe("showing 505 of 600 in top 2 type-rule groups")
  expect(heldKpiSub({ count: 600, shown: 2, approximate: true }, [{ count: 500 }, { count: 5 }], 10000))
    .toBe("at least · showing 505 of 600+ in top 2 type-rule groups · scan bound (10,000 rows) reached")
  expect(heldTabSummary({ count: 600, shown: 2, approximate: false }, [{ count: 500 }, { count: 5 }], 10000))
    .toBe("Showing top 2 type-rule groups (505 of 600 per-rule holds; a record held by two rules counts once per rule).")
  expect(heldTabSummary({ count: 1, shown: 1, approximate: false }, [{ count: 1 }], 10000))
    .toBe("Showing 1 of 1 type-rule group (1 per-rule hold; a record held by two rules counts once per rule).")
  expect(heldTabSummary({ count: 509, shown: 6, approximate: true }, [{ count: 500 }, { count: 5 }, { count: 1 }, { count: 1 }, { count: 1 }, { count: 1 }], 10000))
    .toBe("Showing 6 of 6+ type-rule groups (509+ per-rule holds; a record held by two rules counts once per rule; scan bound (10,000 rows) reached).")
})

test("suggestedFixInputs locks record identity and seeds editable server arguments for the real action flow", () => {
  const action: ActionSummary = {
    actionId: "docs.scanned_document.correct",
    title: "Correct an extracted total",
    description: null,
    objectKind: "docs.ScannedDocument",
    effect: "upsert_input_object",
    objectIdField: "id",
    uiEffects: { irreversible: false, destructive: false, money: ["total"] },
    inputSchema: {
      type: "object",
      required: ["id", "currency", "total", "correction_evidence", "correction_reason"],
      properties: {
        id: { type: "string" },
        currency: { type: "string", enum: ["USD"] },
        total: { type: "number", nornType: { kind: "decimal", scale: 2 } },
        correction_evidence: { type: "string" },
        correction_reason: { type: "string" },
        verified: { type: "boolean" },
      },
    },
  }
  const inputs = suggestedFixInputs(action, "SCAN-INV-0042", {
    actionId: "docs.scanned_document.correct",
    title: "Correct an extracted total",
    label: "Use 72.00",
    property: "total",
    value: "72.00",
    arguments: { id: "SCAN-INV-0042", total: "72.00", currency: "USD", correction_evidence: "SCAN-INV-0042~total", verified: true, unknown_field: "ignored" },
    needs: ["correction_reason"],
  })
  expect(inputs.prefill).toEqual({ id: "SCAN-INV-0042" })
  expect(inputs.defaults).toEqual({ total: "72.00", correction_evidence: "SCAN-INV-0042~total", verified: "true" })
  // Even when a fixture leaves arguments empty, the expected property/value and record id still seed the form.
  expect(suggestedFixInputs(action, "SCAN-INV-0042", {
    actionId: "docs.scanned_document.correct",
    title: "Correct an extracted total",
    label: "Use 72.00",
    property: "total",
    value: "72.00",
    arguments: {},
    needs: ["correction_reason"],
  })).toEqual({ prefill: { id: "SCAN-INV-0042" }, defaults: { total: "72.00" } })
  expect(draftValueFromJson(42, { kind: "integer" })).toBe("42")
  expect(draftValueFromJson(false, { kind: "boolean" })).toBe("false")
  expect(draftValueFromJson(null, { kind: "text" })).toBeNull()
})
