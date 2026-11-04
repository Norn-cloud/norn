import { test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"

test("DIAG-DISCLOSE-1 demo authority is compiled from the typed access declaration", () => {
  const source = join(TOOL_ROOT, "fixtures/host/refusal-access.source.ts")
  const result = compilePack(source, { scope: { orgId: "org_demo", environmentId: "dev" }, root: TOOL_ROOT, modules: [source] })
  if (!result.ok) throw new Error(JSON.stringify(result.failures))
  expect(result.artifacts).toEqual(JSON.parse(readFileSync(join(TOOL_ROOT, "../../crates/norn-api/tests/fixtures/refusal-access.json"), "utf8")))
  expect(result.artifacts).toHaveLength(1)
}, 30_000)
