import { expect, test } from "bun:test"
import { renderToStaticMarkup } from "react-dom/server"
import { ProvenanceChain } from "../src/components/provenance"
import type { SourceLineage } from "../src/lib/transport"

const lineage = (digests: Partial<Pick<SourceLineage, "bundleDigest" | "documentDigest">>): SourceLineage => ({
  mappingPin: { artifactId: "demo.mapping", version: "0.1.0", sha256: "m".repeat(64) },
  sourceRecordRef: { system: "synthetic-erpnext", record: "Rows/1" },
  documentRef: { system: "synthetic-erpnext", record: "DOC-1" },
  fieldProvenance: {
    recovered: { source: { system: "synthetic-erpnext", record: "Rows/1" }, mappingVersion: "0.1.0", transform: "rule.recovered", recordedAt: "2026-01-01T00:00:00Z", sourceFields: ["recovered"] },
  },
  ...digests,
}) as SourceLineage

const html = (value: SourceLineage) => renderToStaticMarkup(<ProvenanceChain lineage={value} fact="recovered" />)

test("digests the server withheld read withheld, absent or null, and never leak an old value", () => {
  for (const withheld of [{}, { bundleDigest: null, documentDigest: null }]) {
    const out = html(lineage(withheld))
    expect(out.match(/withheld/g)?.length).toBe(2)
    expect(out).not.toContain("b".repeat(12))
    expect(out).not.toContain("d".repeat(12))
  }
})

test("disclosed digests render, and one withheld digest does not hide the other", () => {
  const both = html(lineage({ bundleDigest: "b".repeat(64), documentDigest: "d".repeat(64) }))
  expect(both).toContain("b".repeat(12))
  expect(both).toContain("d".repeat(12))
  expect(both).not.toContain("withheld")
  const one = html(lineage({ bundleDigest: "b".repeat(64) }))
  expect(one).toContain("b".repeat(12))
  expect(one.match(/withheld/g)?.length).toBe(1)
})

test("rendering again with a different lineage shows only that lineage's digests", () => {
  const first = html(lineage({ bundleDigest: "b".repeat(64), documentDigest: "d".repeat(64) }))
  const second = html(lineage({}))
  expect(first).toContain("b".repeat(12))
  expect(second).not.toContain("b".repeat(12))
})
