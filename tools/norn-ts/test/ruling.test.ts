import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { RU } from "../src/builder/index.ts"
import { canonicalJson } from "../src/canonical.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker, CHECKER_PKG_ENV } from "./checker.ts"
if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const REPO = join(TOOL_ROOT, "..", "..")
const ROOT = join(REPO, "fixtures/ruling-ir")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "fixtures/ruling-ir/rulings.source.ts" }

test("typed claim and ruling handles compile to the ClaimIR and RulingIR goldens", () => {
  const compiled = compileDefinitions(join(ROOT, "rulings.source.ts"), options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const golden = JSON.parse(readFileSync(join(ROOT, "rulings.json"), "utf8")) as unknown[]
  expect(canonicalJson(compiled.artifacts)).toBe(canonicalJson(golden))

  // The builder is the compiler's lowering target: the same handles, the same bytes.
  const order = RU.pin("commerce.SalesOrder.entity", "0.1.0")
  const built = RU.lowerClaim(RU.claim({
    id: "commerce.sales_order_invoiced_is_derived.claim",
    title: "A sales order's invoiced state is derived",
    version: "0.1.0",
    statement: RU.statement.derivedState(RU.meta.state(order, "commerce.SalesOrder", "status", "invoiced"), [RU.meta.link(order, "commerce.SalesOrder", "lines")]),
    evidence: [RU.evidence.upstream("erpnext", "v16.0.0", "erpnext/selling/doctype/sales_order/sales_order.py#set_status", "structural", "status follows per_billed, computed from Sales Invoice items")],
  }), options)
  expect(canonicalJson(built.artifact)).toBe(canonicalJson(golden[0]))

  // Registry mode emits `active` envelopes; a ruling must never be ratified by authoring.
  const registry = compileDefinitions(join(ROOT, "rulings.source.ts"), { ...options, registryPackagePath: "registry/core" })
  if (!registry.ok) throw new Error(JSON.stringify(registry.diagnostics))
  for (const artifact of registry.artifacts as Array<{ lifecycle: string }>) expect(artifact.lifecycle).toBe("draft")
}, 30_000)

test("raw IR in a claim is refused even when it forges the handle type", () => {
  const dir = mkdtempSync(join(ROOT, "raw-"))
  try {
    const source = join(dir, "raw.ts")
    writeFileSync(source, `import { Norn } from "@norn/authoring"
export const raw = Norn.claim({
  id: "x.raw.claim", title: "raw", version: "0.1.0",
  statement: { _tag: "NornStatement" },
  evidence: [],
})
`)
    const compiled = compileDefinitions(source, options)
    expect(compiled.ok).toBe(false)
    if (!compiled.ok) expect(compiled.diagnostics.map((d) => d.code)).toContain("NORN-FLOW-LITERAL")
  } finally { rmSync(dir, { recursive: true, force: true }) }
}, 30_000)

test("the checker flags norn#59 from the authored claim", () => {
  if (!checker) return
  const compiled = compileDefinitions(join(ROOT, "rulings.source.ts"), options)
  if (!compiled.ok) throw new Error(JSON.stringify(compiled.diagnostics))
  const order = JSON.parse(readFileSync(join(REPO, "registry/core/artifacts/commerce.SalesOrder.entity.json"), "utf8")) as unknown
  const checked = checker.checkArtifactSet([order, compiled.artifacts[0]])
  const codes = checked.diagnostics.diagnostics.map((d: { code: string }) => d.code)
  expect(codes).toContain("catalog.derived_state.declared_transition")
  expect(checked.admitted).toBe(false)
}, 30_000)
