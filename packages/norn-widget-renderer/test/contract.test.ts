import { expect, test } from "bun:test"
import { parseExact, stringifyExact, exactText } from "../src/json.ts"
import { actionIdOf, pageBindings, directPath, installedApps, objectTypeOf, orderedChildren, text, type Ux, type UxDataBinding, type UxPage, type WidgetDefinitionSet, type UxNode } from "../src/model.ts"

// Presentation contract: admitted documents supply meaning; this layer never authorizes.
test("W1: exact numeric tokens survive the presentation boundary", () => {
  const source = '{"amount":9007199254740993,"rate":0.10000000000000000001}'
  expect(stringifyExact(parseExact(source))).toBe(source)
  expect(exactText(parseExact("9007199254740993"))).toBe("9007199254740993")
  for (const token of ["1.00", "-0", "1e2"]) {
    expect(stringifyExact(parseExact(token))).toBe(token)
    expect(exactText(parseExact(token))).toBe(token)
  }
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


test("W5: entity and action references require the exact installed version", () => {
  const set: WidgetDefinitionSet = { artifacts: [
    { artifactId: "demo.Entity", version: "1.0.0", kind: "entity", payload: { objectType: { name: "demo.First" } } },
    { artifactId: "demo.Entity", version: "2.0.0", kind: "entity", payload: { objectType: { name: "demo.Second" } } },
    { artifactId: "demo.Action", version: "1.0.0", kind: "action", payload: { actionId: "demo.First" } },
  ] }
  expect(objectTypeOf(set, { id: "demo.Entity", version: "2.0.0" })).toBe("demo.Second")
  expect(objectTypeOf(set, { id: "demo.Entity", version: "3.0.0" })).toBeNull()
  expect(actionIdOf(set, { id: "demo.Action", version: "1.0.0" })).toBe("demo.First")
  expect(actionIdOf(set, { id: "demo.Action", version: "2.0.0" })).toBeNull()
})

test("W6: navigation condition bindings are collected once in first-use order", () => {
  const binding = (bindingId: string): UxDataBinding => ({ bindingId, object: { id: "demo.Entity", version: "1.0.0" }, query: { kind: "object_list", filters: [], sort: [], page: { size: 10, maxSize: 10 } } })
  const node: UxNode = { nodeId: "nav", children: [], component: { kind: "navigation", itemId: "orders" } }
  const page: UxPage = { pageId: "home", name: "Home", contextParams: [], root: { ...node, children: [node] } }
  const ux: Ux = { uxVersion: "0.1", pages: [page], fields: [], forms: [], actionBindings: [], actionPlacements: [], overrides: [],
    navigationItems: [{ itemId: "orders", label: { en: "Orders" }, requiredCapability: "read", destination: { kind: "page", pageId: "home", parameters: {} }, visibleWhen: { expression: "true", inputs: { recordBindingId: "record", selectionBindingId: "selection" } } }],
    dataBindings: [binding("selection"), binding("record"), binding("unused")],
  }
  expect(pageBindings(page, ux).map(item => item.bindingId)).toEqual(["record", "selection"])
})
