import { expect, test } from "bun:test"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { interfaceType, lowerEntity } from "../src/builder/entity.ts"
import { compileDefinitions } from "../src/compiler/definitions.ts"
import { checker } from "./checker.ts"

const options = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "links.ts" }

const source = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Holder = Norn.interfaceType({
  name: "links.Holder", title: "Holder", version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String }),
})
export const Held = Norn.interfaceType({
  name: "links.Held", title: "Held", version: "0.1.0",
  properties: Schema.Struct({ code: Schema.String }),
  links: [
    { id: "owner", description: "Who holds it.", target: Holder, cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", target: "links.Holder", cardinality: { to: "one", from: "many" } },
  ],
})
export const Person = Norn.type({
  name: "links.Person", title: "Person", version: "0.1.0", implements: [Holder],
  properties: Schema.Struct({ code: Schema.String }),
})
export const Item = Norn.type({
  name: "links.Item", title: "Item", version: "0.1.0", implements: [Held],
  properties: Schema.Struct({ code: Schema.String }),
  links: [
    { id: "owner", target: Person, cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", target: Person, cardinality: { to: "one", from: "many" } },
  ],
})
`

function compile(text: string) {
  const dir = mkdtempSync(join(tmpdir(), "norn-interface-links-"))
  const file = join(dir, "links.ts")
  writeFileSync(file, text)
  try {
    return compileDefinitions(file, options)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

function artifacts() {
  const result = compile(source)
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  return result.artifacts
}

test("interface links lower to InterfaceLink requirements, by reference or by name", () => {
  const held = artifacts().find((a) => (a as { artifactId?: string }).artifactId === "links.Held.interface") as
    | { payload: { interface: { links?: unknown } } }
    | undefined
  expect(held?.payload.interface.links).toEqual([
    { id: "owner", description: "Who holds it.", target: "links.Holder", cardinality: { to: "one", from: "many", required: true } },
    { id: "custodian", target: "links.Holder", cardinality: { to: "one", from: "many" } },
  ])
  const holder = artifacts().find((a) => (a as { artifactId?: string }).artifactId === "links.Holder.interface") as
    | { payload: { interface: Record<string, unknown> } }
    | undefined
  expect(holder?.payload.interface).not.toHaveProperty("links")
})

test("an interface link takes no inverse or acyclic member", () => {
  const result = compile(source.replace('cardinality: { to: "one", from: "many" } },\n  ],\n})\nexport const Person', 'cardinality: { to: "one", from: "many" }, acyclic: true },\n  ],\n})\nexport const Person'))
  expect(result.ok).toBe(false)
})

test.skipIf(!checker)("Rust admits an implementation that declares every interface link", () => {
  expect(checker!.checkArtifactSet(JSON.parse(JSON.stringify(artifacts()))).admitted).toBe(true)
})

test.skipIf(!checker)("Rust refuses an implementation that omits a required interface link", () => {
  const result = compile(source.replace('    { id: "owner", target: Person, cardinality: { to: "one", from: "many", required: true } },\n', ""))
  if (!result.ok) throw new Error(JSON.stringify(result.diagnostics))
  expect(checker!.checkArtifactSet(JSON.parse(JSON.stringify(result.artifacts))).admitted).toBe(false)
})

test("the builder emits only InterfaceLink members, even from an object-type link", () => {
  const link = { id: "owner", target: "links.Holder", cardinality: { to: "one" as const, from: "many" as const }, inverse: "held", acyclic: true }
  const lowered = lowerEntity(interfaceType({ name: "links.Held", title: "Held", version: "0.1.0", properties: [], links: [link] }), { scope: {}, sourcePath: "links.ts" })
  if (!lowered.ok) throw new Error(JSON.stringify(lowered.diagnostics))
  expect((lowered.artifact as { payload: { interface: { links: unknown } } }).payload.interface.links).toEqual([
    { id: "owner", target: "links.Holder", cardinality: { to: "one", from: "many" } },
  ])
})
