import { expect, test } from "bun:test"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { renderToStaticMarkup } from "react-dom/server"
import { buildQuestion, draftOf } from "../src/builder.tsx"
import { DashboardEditor, issuesOfFindings, layoutTiles } from "../src/dashboards.tsx"
import { duplicateDashboard, type StoredDashboard } from "../src/lib/definitions.ts"
import { catalog, cell, cellDouble, tile, weekly } from "./support/definitions.ts"

test("a saved tile's question reopens as the draft it was built from, title and all", () => {
  expect(draftOf(buildQuestion(weekly, catalog), weekly.title)).toEqual(weekly)
  expect(draftOf(cell)).toMatchObject({ measures: ["demo.yield"], by: "occurred_at", grain: "week", filters: [{ property: "metal", operator: "eq", value: "gold" }], start: "2026-07-06", end: "2026-10-26" })
})

test("tiles lay out in reading order on two columns; duplicating a dashboard renames it and gives its tiles new ids", () => {
  expect(layoutTiles([tile("a", 1), tile("b", 1), tile("c", 2), tile("d", 1), tile("e", 2)]).map(({ id, layout }) => `${id}:${layout.x},${layout.y},${layout.w}`)).toEqual(["a:0,0,1", "b:1,0,1", "c:0,1,2", "d:0,2,1", "e:0,3,2"])
  let n = 0
  const copy = duplicateDashboard({ title: "Refinery", tiles: [tile("a"), tile("b")] }, () => `new-${++n}`)
  expect(copy.title).toBe("Refinery (copy)")
  expect(copy.tiles.map(({ id }) => id)).toEqual(["new-1", "new-2"])
})

test("the editor shows its empty state, and a reopened dashboard shows every tile with its verbs, view and an honest not-run note", () => {
  const empty = render()
  for (const text of ["No questions on this dashboard yet", "Add question", 'aria-label="Undo"', "All changes saved"]) expect(empty).toContain(text)
  const html = render({ id: "dash-1", version: 3, dashboard: { title: "Refinery weekly", tiles: [tile("a"), { ...tile("b", 1), title: "Loss table", view: { kind: "table" } }] } })
  for (const text of ['value="Refinery weekly"', "Version 3", "Gold Yield, weekly", "Loss table", 'aria-label="Move tile 2 up"', 'aria-label="Duplicate tile 1"', 'aria-label="Remove tile 2"', "Edit question", "Not run yet: the evaluate route isn&#x27;t connected."]) expect(html).toContain(text)
  expect(html).toMatch(/aria-label="Tile 2 view"[^]*?<option value="table" selected/)
})
