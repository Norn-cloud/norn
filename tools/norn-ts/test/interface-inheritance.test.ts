import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { ProfileChecker } from "../src/check.ts"
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

/** TypeScript errors for an implementor of `Located` keyed on `key`. */
function typeErrors(key: string): ReadonlyArray<string> {
  const dir = mkdtempSync(join(tmpdir(), "norn-inheritance-"))
  const file = join(dir, "shelf.ts")
  writeFileSync(file, `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
const Named = Norn.interfaceType({
  name: "inheritance.Named", title: "Named", version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String }),
})
const Located = Norn.interfaceType({
  name: "inheritance.Located", title: "Located", version: "0.1.0",
  extends: [Named], properties: Schema.Struct({ site: Schema.String }),
})
export const Shelf = Norn.type({
  name: "inheritance.Shelf", title: "Shelf", version: "0.1.0",
  implements: [Located], properties: Schema.Struct({ shelf_no: Schema.String }),
  identity: [{ name: "by_key", properties: ["${key}"] }],
})
`)
  try {
    const [report] = new ProfileChecker().check([file])
    return (report?.diagnostics ?? []).filter((d) => d.code === "NORN-FLOW-TYPE").map((d) => d.message)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test("an implementor's key may name a grandparent interface's field, and not an unknown one", () => {
  expect(typeErrors("code")).toEqual([])
  expect(typeErrors("site")).toEqual([])
  expect(typeErrors("colour").length).toBeGreaterThan(0)
}, 60_000)
