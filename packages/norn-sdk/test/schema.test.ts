import { expect, test } from "bun:test"
import document from "../../../sdks/openapi/norn-api.openapi.json"

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
