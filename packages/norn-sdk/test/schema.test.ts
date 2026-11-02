import { expect, test } from "bun:test"
import document from "../../../sdks/openapi/norn-api.openapi.json"
import type { Schemas } from "../src/index.ts"

test("MAP-ROLES: generated interpretations require exact endpoints and a distinct role", () => {
  const endpoint = { artifactId: "toy.capacity", version: "1.0.0", sha256: "a".repeat(64) }
  const interpretation: Schemas["NornIrLawInterpretationIr"] = {
    format: "norn.interpretation/1", role: "law_interpretation",
    sourceLayer: endpoint, targetLayer: { ...endpoint, artifactId: "toy.credit" },
    members: [],
  }
  // @ts-expect-error Source records cannot stand in for a law interpretation.
  const wrongRole: typeof interpretation.role = "source_record"
  // @ts-expect-error An endpoint cannot omit its content identity.
  const missingDigest: typeof interpretation.sourceLayer = { artifactId: "toy.capacity", version: "1.0.0" }
  expect(wrongRole).not.toBe(interpretation.role)
  expect(missingDigest).not.toHaveProperty("sha256")
})

test("every generated local schema reference resolves in the retained contract", () => {
  const value: unknown = document
  function walk(node: unknown): void {
    if (typeof node !== "object" || node === null) return
    if (Array.isArray(node)) { node.forEach(walk); return }
    for (const [key, child] of Object.entries(node)) {
      if (key === "$ref" && typeof child === "string" && child.startsWith("#/")) {
        let target: unknown = value
        for (const token of child.slice(2).split("/")) {
          if (typeof target !== "object" || target === null || !(token in target)) throw new Error(`unresolved schema reference ${child}`)
          target = Reflect.get(target, token)
        }
        expect(target).toBeDefined()
      }
      walk(child)
    }
  }
  walk(value)
})

test("LAW-INSTANCE-CONTRACT-1: effective instances retain origins and bindings", () => {
  const at: Schemas["NornIrEffectiveLawInstance"]["subject"] = {
    artifactId: "toy.credit", version: "1.0.0", member: { kind: "type", typeName: "toy.Credit" },
  }
  const instance: Schemas["NornIrEffectiveLawInstance"] = {
    law: { ...at, artifactId: "toy.law", member: { kind: "law", law: "toy.preserve" } },
    lawDigest: "a".repeat(64), subject: at, subjectDigest: "b".repeat(64),
    attachment: { kind: "pack", id: "toy.layer" }, origin: at,
    paths: [[at]], instancePaths: [], bindings: { self: at },
  }
  const cell: Schemas["NornIrLawRootOneOf2"] = { kind: "cell", id: "cell_demo" }
  const entity: Schemas["NornIrLawRootOneOf3"] = { kind: "entity", id: "toy.Credit" }
  expect(cell.kind).toBe("cell")
  expect(entity.kind).toBe("entity")
  expect(instance.law.member.kind).toBe("law")
  expect(instance.bindings.self).toEqual(instance.subject)
})
