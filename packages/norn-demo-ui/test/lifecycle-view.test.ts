// The lifecycle view's model (UI-LIFECYCLE-2): layout from the declaration, counts only from the
// cell's facets answer, and a counterexample stepped by its witness alone.
import { expect, test } from "bun:test"
import { type ArrowRoute, type Box, type LifecycleDoc, type LifecycleFinding, type Point, countsFrom, layout, routeArrow, stepState } from "../src/elements/lifecycle-view.ts"

const ORDER: LifecycleDoc = {
  property: "status",
  states: ["draft", "submitted", "approved", "rejected", "received", "cancelled", "archived"],
  initial: ["draft"],
  terminal: ["received", "cancelled"],
  transitions: [
    { id: "submit", from: ["draft"], to: "submitted" }, { id: "approve", from: ["submitted"], to: "approved" },
    { id: "reject", from: ["submitted"], to: "rejected" }, { id: "revise", from: ["rejected"], to: "draft" },
    { id: "receive", from: ["approved"], to: "received" }, { id: "cancel", from: ["submitted", "approved"], to: "cancelled" },
  ],
}

test("states are laid out by distance from the start; a state no run reaches goes last", () => {
  expect(layout(ORDER)).toEqual([["draft"], ["submitted"], ["approved", "rejected", "cancelled"], ["received"], ["archived"]])
})

test("counts are the cell's: a declared state the answer omits is 0, a scan past the bound is a lower bound, unseen rows are said", () => {
  const answer = { status: 200, body: { rows: 40, withheldRows: 3, quarantinedRows: 0, approximate: true, facets: [{ property: "status", values: [{ value: "draft", count: 30 }, { value: "submitted", count: 10 }], otherRows: 2, nulls: 1 }] } }
  const counts = countsFrom(ORDER, answer)
  expect(counts.states.get("draft")).toEqual({ kind: "count", count: 30, lowerBound: true })
  expect(counts.states.get("cancelled")).toEqual({ kind: "count", count: 0, lowerBound: true })
  expect(counts.notes).toEqual(["3 withheld from you", "1 without a state", "2 in states this view does not list"])
  expect(counts.refused).toBeNull()
})

test("before an answer every count is pending; a refusal shows no number at all", () => {
  expect([...countsFrom(ORDER, null).states.values()].every((count) => count.kind === "pending")).toBe(true)
  const refused = countsFrom(ORDER, { status: 403, body: { code: "authz.denied" } }, "ar")
  expect(refused.refused).toBe("لا يمكنك رؤية أعداد هذه السجلات.")
  expect([...refused.states.values()].every((count) => count.kind === "pending")).toBe(true)
})

test("a counterexample steps by its witness: taken lifecycle transitions and where the object is", () => {
  const stuck: LifecycleFinding = {
    code: "widget.lifecycle.stuck",
    message: "page state `done` with the object `draft`: no ending of both is reachable",
    witness: { steps: [
      { edge: 0, moves: [{ side: "ui", label: "submit" }] },
      { edge: 3, moves: [{ side: "ui", label: "on-accepted" }, { side: "backend", label: "submit" }] },
      { edge: 9, moves: [{ side: "backend", label: "reject" }] },
      { edge: 14, moves: [{ side: "backend", label: "revise" }] },
    ] },
  }
  expect(stepState(ORDER, stuck, 0)).toEqual({ transitions: new Set(), state: null })
  expect(stepState(ORDER, stuck, 1)).toEqual({ transitions: new Set(), state: null })
  expect(stepState(ORDER, stuck, 2)).toEqual({ transitions: new Set(["submit"]), state: "submitted" })
  expect(stepState(ORDER, stuck, 4)).toEqual({ transitions: new Set(["submit", "reject", "revise"]), state: "draft" })
})

// A property over generated lifecycles, in both directions: on the board's grid (11rem cards,
// 4rem between columns) no arrow passes through a card, whichever states a transition joins.
test("no arrow crosses a card, for any lifecycle the layout can produce", () => {
  let seed = 0x9e3779b9
  const random = () => { seed = (seed + 0x6d2b79f5) | 0; let t = Math.imul(seed ^ (seed >>> 15), 1 | seed); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296 }
  const pick = <T>(items: readonly T[]) => items[Math.floor(random() * items.length)]!
  const sample = (route: ArrowRoute): Point[] => {
    const points: Point[] = []
    let from = route.start
    for (const segment of route.segments) {
      const control = [from, ...(segment.via ?? []), segment.to]
      for (let step = 1; step < 40; step += 1) {
        let level = control
        while (level.length > 1) level = level.slice(1).map((point, index) => [level[index]![0] + (point[0] - level[index]![0]) * (step / 40), level[index]![1] + (point[1] - level[index]![1]) * (step / 40)] as const)
        points.push(level[0]!)
      }
      from = segment.to
    }
    return points
  }
  let arrows = 0
  for (let round = 0; round < 300; round += 1) {
    const states = Array.from({ length: 2 + Math.floor(random() * 7) }, (_, index) => `s${index}`)
    const lifecycle: LifecycleDoc = {
      property: "status", states, initial: [states[0]!], terminal: [],
      transitions: Array.from({ length: 1 + Math.floor(random() * 12) }, (_, index) => ({ id: `t${index}`, from: [pick(states), pick(states)], to: pick(states) })),
    }
    const columns = layout(lifecycle)
    const column = new Map(columns.flatMap((column, index) => column.map((state) => [state, index] as const)))
    for (const rtl of [false, true]) {
      const boxes = new Map<string, Box>()
      columns.forEach((column, index) => {
        let top = 20
        for (const state of column) {
          const height = 60 + Math.floor(random() * 40)
          const left = rtl ? 2000 - index * 240 - 176 : 20 + index * 240
          boxes.set(state, { left, right: left + 176, top, bottom: top + height })
          top += height + 12
        }
      })
      const floor = Math.max(...[...boxes.values()].map((box) => box.bottom))
      let lane = 0
      for (const transition of lifecycle.transitions) {
        for (const from of transition.from) {
          const hop = column.get(transition.to)! - column.get(from)!
          expect(hop).toBeLessThanOrEqual(1)
          const route = routeArrow(boxes.get(from)!, boxes.get(transition.to)!, hop, rtl, floor, hop < -1 ? lane++ : 0)
          arrows += 1
          for (const [x, y] of sample(route)) {
            const inside = [...boxes.entries()].find(([, box]) => x > box.left + 0.5 && x < box.right - 0.5 && y > box.top + 0.5 && y < box.bottom - 0.5)
            if (inside) throw new Error(`${from} -> ${transition.to} (hop ${hop}, rtl ${rtl}) passes through ${inside[0]} at ${x},${y}`)
          }
        }
      }
    }
  }
  expect(arrows).toBeGreaterThan(1000)
})

// The view's load states are a declared machine, admitted by the checker like any page's: every
// state can exit (no endless "Counting…"), every outcome of the read is handled.
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { describe } from "bun:test"

const pkg = process.env.NORN_CHECKER_WASM_PKG
const withChecker = pkg ? describe : describe.skip
withChecker("the checker", () => {
  test("admits the view's machine: no widget.machine.* finding", async () => {
    const checker = (await import(pathToFileURL(join(pkg!, "norn_checker_wasm.js")).href)) as {
      initSync(options: { module: Uint8Array }): unknown
      checkArtifactSet(artifactsJson: string, contextJson: string): string
    }
    checker.initSync({ module: readFileSync(join(pkg!, "norn_checker_wasm_bg.wasm")) })
    const document = readFileSync(join(import.meta.dir, "../src/elements/lifecycle-view.widget.json"), "utf8")
    const result = JSON.parse(checker.checkArtifactSet(`[${document}]`, "null")) as { diagnostics: { diagnostics: { code: string; message: string }[] } }
    const machine = result.diagnostics.diagnostics.filter((finding) => finding.code.startsWith("widget.machine."))
    expect(machine).toEqual([])
  })
})
