import { test, expect } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"

test("the hosted Studio role policy is compiled from its typed access declaration", () => {
  const source = join(TOOL_ROOT, "fixtures/host/studio-access.source.ts")
  const result = compilePack(source, { scope: { orgId: "org_demo" }, root: TOOL_ROOT, modules: [source] })
  if (!result.ok) throw new Error(JSON.stringify(result.failures))
  expect(result.artifacts).toEqual(JSON.parse(readFileSync(join(TOOL_ROOT, "../../crates/norn-api/tests/fixtures/studio-access.json"), "utf8")))
}, 30_000)
