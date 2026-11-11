import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compileModule } from "../src/compile-stdin.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

// LABEL-LOCALE-1: labels ride on the definition and lower to a separate
// `<artifact id>.labels` artifact; the definition's artifact never changes.
const file = join(TOOL_ROOT, "fixtures/definitions/labels.source.ts")
const scope = { orgId: "org_demo", environmentId: "dev" }
const LABEL_MEMBER = /^\s*(label|localizedDescription|needsReview|propertyLabels|valueLabels): .*\n/gm

function compile(source: string): any[] {
  const result = compileModule(source, scope)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result.artifacts as any[]
}

test("labels lower to their own artifact and leave every definition artifact byte-identical", () => {
  const source = readFileSync(file, "utf8")
  const labelled = compile(source)
  const plain = compile(source.replace(LABEL_MEMBER, ""))
  expect(plain.some(a => a.kind === "labels")).toBe(false)
  expect(labelled.filter(a => a.kind !== "labels")).toEqual(plain)
  const invoice = labelled.find(a => a.artifactId === "sales.Invoice.entity.labels")
  expect(invoice.kind).toBe("labels")
  expect(invoice.payload.labelsVersion).toBe("0.1")
  expect(invoice.payload.entries).toContainEqual({
    target: { kind: "objectType", objectType: "sales.Invoice" },
    label: { ar: "فاتورة", "ar-EG": "فاتورة بيع", en: "Invoice" },
    description: { ar: "فاتورة بيع صادرة لعميل.", en: "A sales invoice issued to a customer." },
    review: { "ar-EG": "needs_review" },
  })
  expect(invoice.payload.entries).toContainEqual({
    target: { kind: "enumValue", objectType: "sales.Invoice", property: "status", value: "posted" },
    label: { ar: "مُرحّلة", en: "Posted" },
  })
  expect(labelled.find(a => a.artifactId === "sales.Party.interface.labels").payload.entries).toContainEqual({
    target: { kind: "property", objectType: "sales.Party", property: "id" },
    label: { ar: "الكود", en: "Code" },
  })
}, 30_000)

test("a label for an undeclared property is refused at compile time", () => {
  const result = compileModule(readFileSync(file, "utf8").replace('propertyLabels: { id: { en: "Invoice number"', 'propertyLabels: { total: { en: "Total" }, id: { en: "Invoice number"'), scope)
  expect(result.ok).toBe(false)
}, 30_000)

test.skipIf(!checker)("Rust admits the labelled set and refuses a label on an unknown value", () => {
  const artifacts = compile(readFileSync(file, "utf8"))
  expect(checker!.checkArtifactSet(artifacts).diagnostics.diagnostics).toEqual([])
  expect(checker!.checkArtifactSet(artifacts).admitted).toBe(true)
  const unknown = compile(readFileSync(file, "utf8").replace("posted: { en: \"Posted\"", "void: { en: \"Void\""))
  const codes = (checker!.checkArtifactSet(unknown).diagnostics.diagnostics as { code: string }[]).map(d => d.code)
  expect(codes).toContain("labels.admission.unknown_target")
}, 30_000)
