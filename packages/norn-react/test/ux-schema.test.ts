// The renderer's closed kinds are the IR's (#1762 milestone C): every component kind the widget
// schema admits is drawn (`RENDERED_COMPONENTS`) or refused by name, so a kind the IR adds fails
// here until the renderer draws it or refuses it.
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { RENDERED_COMPONENTS } from "../src/capabilities.ts"

const REFUSED = ["child_grid", "composition", "form"]

test("every WidgetComponent kind in the schema is drawn or refused", () => {
  const schema = readFileSync(join(import.meta.dir, "../../../crates/norn-ir/schema/widget.schema.json"), "utf8")
  const component = JSON.parse(schema) as { $defs?: Record<string, { oneOf?: { properties?: { kind?: { const?: string; enum?: string[] } } }[] }>; definitions?: Record<string, { oneOf?: { properties?: { kind?: { const?: string; enum?: string[] } } }[] }> }
  const defs = component.$defs ?? component.definitions ?? {}
  const kinds = (defs.WidgetComponent?.oneOf ?? []).map((variant) => variant.properties?.kind?.const ?? variant.properties?.kind?.enum?.[0]).filter((kind): kind is string => kind !== undefined).sort()
  expect(kinds.length).toBeGreaterThan(5)
  expect(kinds).toEqual([...RENDERED_COMPONENTS, ...REFUSED].sort())
})
