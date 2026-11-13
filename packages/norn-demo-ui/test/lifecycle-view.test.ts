// The lifecycle view's model (UI-LIFECYCLE-2): layout from the declaration, counts only from the
// cell's facets answer, and a counterexample stepped by its witness alone.
import { expect, test } from "bun:test"
import { type LifecycleDoc, type LifecycleFinding, countsFrom, layout, stepState } from "../src/elements/lifecycle-view.ts"

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
