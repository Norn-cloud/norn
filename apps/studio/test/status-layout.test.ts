import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { crossings, drawingOf } from "../src/lib/canvas/drawing.ts"
import { STATE_SPACING, tidyLifecycle } from "../src/lib/canvas/layout.ts"
import { decompile, lifecyclesIn } from "../src/lib/canvas/lifecycle.ts"
import type { Envelope } from "../src/lib/transport.ts"

// The real sales order lifecycle, as the status canvas opens it.
const directory = join(import.meta.dir, "../../../registry/core/artifacts")
const envelopes = readdirSync(directory).filter((name) => name.endsWith(".json")).map((name) => JSON.parse(readFileSync(join(directory, name), "utf8")) as Envelope)
const order = lifecyclesIn(envelopes).find(({ objectType }) => objectType === "commerce.SalesOrder")!
const opened = decompile(envelopes.find(({ artifactId }) => artifactId === order.artifactId)!, order)

test("tidy up draws the sales order with no more crossings than it opens with, and fewer here", () => {
  const before = crossings(opened)
  const after = crossings(tidyLifecycle(opened))
  expect(after).toBeLessThanOrEqual(before)
  expect(after).toBeLessThan(before)
})

test("the main row is what every way to Completed passes through; side states sit in a band off it", () => {
  const tidied = tidyLifecycle(opened)
  const y = new Map(tidied.nodes.map(({ id, position }) => [id, position.y]))
  for (const id of ["draft", "submitted", "approved", "invoiced", "completed"]) expect(y.get(id)).toBe(0)
  for (const id of ["cancelled", "on_hold"]) expect(Math.abs(y.get(id)!)).toBe(STATE_SPACING.row)
  // Same graph, same layout.
  expect(tidyLifecycle(opened)).toEqual(tidied)
})

test("steps back along the row loop on the side away from the side states", () => {
  const tidied = tidyLifecycle(opened)
  const sideStates = tidied.nodes.filter(({ position }) => position.y !== 0).map(({ position }) => Math.sign(position.y))
  const amend = drawingOf(tidied).rails.get("amend:submitted")
  if (sideStates.every((sign) => sign < 0)) expect(amend?.side).toBe(1)
  if (sideStates.every((sign) => sign > 0)) expect(amend?.side).toBe(-1)
})
