import { expect, test } from "bun:test"
import { join } from "node:path"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const file = join(TOOL_ROOT, "fixtures/definitions/interface-inheritance.source.ts")
const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "interface-inheritance.source.ts" }
function compiled() {
  const result = compileDefinitions(file, options)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result
}

/** The lowered interface payload of the `<name>.interface` artifact. */
function definition(artifacts: ReadonlyArray<unknown>, name: string): Record<string, unknown> | undefined {
  const artifact = artifacts.find((candidate) => (candidate as { artifactId?: string }).artifactId === `${name}.interface`)
  return (artifact as { payload?: { interface?: Record<string, unknown> } } | undefined)?.payload?.interface
}

test("interface parents lower to extends, and a parentless interface omits it", () => {
  const artifacts = compiled().artifacts
  expect(definition(artifacts, "inheritance.Located")?.extends).toEqual(["inheritance.Named"])
  const named = definition(artifacts, "inheritance.Named")
  expect(named).toBeDefined()
  expect(named).not.toHaveProperty("extends")
})

test.skipIf(!checker)("Rust admits the inherited interface contract", () => {
  const checked = checker!.checkArtifactSet(JSON.parse(JSON.stringify(compiled().artifacts)))
  expect(checked.admitted).toBe(true)
})
