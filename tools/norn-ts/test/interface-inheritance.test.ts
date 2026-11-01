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

const parents = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Named = Norn.interfaceType({
  name: "inheritance.Named", title: "Named", version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String }),
})
export const Located = Norn.interfaceType({
  name: "inheritance.Located", title: "Located", version: "0.1.0",
  extends: [Named], properties: Schema.Struct({ site: Schema.String }),
})
`

/** Write `source` to a scratch module and run `inspect` on its path. */
function withModule<T>(source: string, inspect: (file: string) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "norn-inheritance-"))
  const file = join(dir, "shelf.ts")
  writeFileSync(file, source)
  try {
    return inspect(file)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

/** TypeScript errors for `value` typed as `Located`'s effective properties. */
function typeErrors(value: string): ReadonlyArray<string> {
  const source = `${parents}type Fields = NonNullable<(typeof Located)["__types"]>["type"]
export const fields: Fields = ${value}
`
  return withModule(source, (file) => {
    const [report] = new ProfileChecker().check([file])
    return (report?.diagnostics ?? []).filter((d) => d.code === "NORN-FLOW-TYPE").map((d) => d.message)
  })
}

test("an interface's type carries its grandparent's properties", () => {
  expect(typeErrors(`{ code: "c", site: "s" }`)).toEqual([])
  expect(typeErrors(`{ site: "s" }`).length).toBeGreaterThan(0)
}, 60_000)

/** Whether Rust admits an implementor of `Located` declaring `properties`, keyed on `code`. */
function implementorAdmitted(properties: string): boolean {
  const source = `${parents}export const Shelf = Norn.type({
  name: "inheritance.Shelf", title: "Shelf", version: "0.1.0",
  implements: [Located], properties: Schema.Struct({ ${properties} }),
  identity: [{ name: "by_code", properties: ["code"] }],
})
`
  return withModule(source, (file) => {
    const result = compileDefinitions(file, { ...options, sourcePath: "shelf.ts" })
    if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
    return checker!.checkArtifactSet(JSON.parse(JSON.stringify(result.artifacts))).admitted
  })
}

test.skipIf(!checker)("Rust admits an implementor only when it declares the inherited properties", () => {
  expect(implementorAdmitted("code: Schema.String, site: Schema.String")).toBe(true)
  // TypeScript also accepts the key when `code` is only inherited; admission is the authority.
  expect(implementorAdmitted("site: Schema.String")).toBe(false)
}, 60_000)
