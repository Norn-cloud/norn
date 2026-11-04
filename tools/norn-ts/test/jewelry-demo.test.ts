import { expect, test } from "bun:test"
import { readFileSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { canonicalJson } from "../src/canonical.ts"

const root = join(TOOL_ROOT, "../..")
const sourcePath = "scripts/demo/jewelry.source.ts"
const fixture = join(root, "crates/norn-checker/tests/fixtures/jewelry_demo.json")

test("authored jewelry golden matches the law and catalog native inputs", () => {
  const result = compileDefinitions(join(root, sourcePath), {
    scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath,
  })
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(result.warnings).toEqual([])
  if (process.env.NORN_UPDATE_JEWELRY_GOLDEN === "1") {
    writeFileSync(fixture, JSON.stringify(result.artifacts) + "\n")
  }
  const retained: unknown = JSON.parse(readFileSync(fixture, "utf8"))
  expect(canonicalJson(result.artifacts)).toBe(canonicalJson(retained))
}, 60_000)
