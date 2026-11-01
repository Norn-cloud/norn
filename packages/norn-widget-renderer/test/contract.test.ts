import { expect, test } from "bun:test"
import { parseExact, stringifyExact, exactText } from "../src/json.ts"
import { directPath, installedApps, objectTypeOf, orderedChildren, text, type UxNode } from "../src/model.ts"

// Presentation contract: admitted documents supply meaning; this layer never authorizes.
test("W1: exact numeric tokens survive the presentation boundary", () => {
  const source = '{"amount":9007199254740993,"rate":0.10000000000000000001}'
  expect(stringifyExact(parseExact(source))).toBe(source)
  expect(exactText(parseExact("9007199254740993"))).toBe("9007199254740993")
})

test("W2: only declared applications appear; an empty document stays empty", () => {
  expect(installedApps({ artifacts: [] })).toEqual([])
  expect(installedApps({ artifacts: [
    { artifactId: "alpha.legacy", kind: "widget", version: "0.1.0", payload: {} },
    { artifactId: "alpha.order", kind: "entity", version: "0.1.0", payload: { ux: { app: { appId: "ignored" } } } },
    { artifactId: "alpha.ui", kind: "widget", version: "0.1.0", payload: { ux: { app: { appId: "alpha" } } } },
  ] }).map(app => app.appId)).toEqual(["alpha"])
})

test("W3: expression paths are data references, never locally evaluated policy", () => {
  expect(directPath("input.recordKey")).toEqual({ namespace: "recordKey" })
  expect(directPath("input.draft.amount")).toEqual({ namespace: "draft", name: "amount" })
  for (const expression of ["input.record.amount > 10", "input.actor.isAdmin", "input.draft.x + 1"]) {
    expect(directPath(expression)).toBeNull()
  }
})

test("W4: section order changes only section slots and leaves the document intact", () => {
  const section = (nodeId: string, order: number): UxNode => ({
    nodeId, children: [], component: { kind: "section", section: { kind: "section", sectionId: nodeId, fields: [], order, label: { en: nodeId } } },
  })
  const late = section("late", 2)
  const early = section("early", 1)
  const field: UxNode = { nodeId: "field", children: [], component: { kind: "field", fieldId: "amount" } }
  const root: UxNode = { ...section("root", 0), children: [late, field, early] }
  expect(orderedChildren(root).map(node => node.nodeId)).toEqual(["early", "field", "late"])
  expect(root.children.map(node => node.nodeId)).toEqual(["late", "field", "early"])
})

test("W5: unresolved entities stay unresolved and locale fallback uses declared text", () => {
  expect(objectTypeOf({ artifacts: [] }, { id: "alpha.missing", version: "0.1.0" })).toBeNull()
  expect(text({ en: "Orders", fr: "Commandes" }, "fr-CA")).toBe("Commandes")
  expect(text(undefined, "en")).toBe("")
})
