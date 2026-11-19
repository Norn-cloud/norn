// The decision sheet's markup, and which controls its admitted machine offers in each state, read
// with the checker's engine. Browser rows (Studio's e2e) replay every transition; these rows pin
// the document-level contract without a DOM.

import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { parseFragment } from "parse5"

import { type SheetDecision, type SheetDocument, decideRequest, needsConfirm, sheetMarkup } from "../src/elements/decision-sheet.ts"
import { type Evaluate, type Snapshot, enabled, init, update, walks, wasmEvaluate } from "../src/machine/index.ts"
import { sheetPayloads } from "./support/sheet-payloads.ts"

const document = JSON.parse(readFileSync(join(import.meta.dir, "../../../apps/studio/src/decisions/decision-sheet.widget.json"), "utf8")) as SheetDocument
const machine = document.payload.ux.pages[0]?.machine
if (!machine) throw new Error("the sheet document has a machine")

const decision: SheetDecision = {
  id: "dec_1",
  question: "Approve the refund?",
  risk: "medium",
  run: { id: "run_1" },
  expiresAtMs: 1_000_000,
  flow: { title: "Refunds", ruleId: "refunds.flow.rule" },
  step: { id: "approve", label: "Approve" },
  options: [
    { id: "approve", label: "Approve", scope: "Refund 40 EUR", effect: { action: "refund.issue" } },
    { id: "reject", label: "Reject", scope: "No refund", effect: { binding: "outcome" } },
  ],
  agentPick: "reject",
}

type Node = { nodeName: string; attrs?: { name: string; value: string }[]; childNodes?: Node[] }
function all(node: Node, out: Node[] = []): Node[] {
  out.push(node)
  for (const child of node.childNodes ?? []) all(child, out)
  return out
}
const attr = (node: Node, name: string) => node.attrs?.find((a) => a.name === name)?.value

describe("sheetMarkup", () => {
  const tree = all(parseFragment(sheetMarkup(document, decision, "en")) as unknown as Node)
  test("every option is a UI SDK radio, and every control of the page is drawn", () => {
    expect(tree.filter((n) => n.nodeName === "ui-radio").map((n) => attr(n, "value"))).toEqual(["approve", "reject"])
    const drawn = new Set(tree.map((n) => attr(n, "data-node")).filter(Boolean))
    const controls = machine.events.flatMap((event) => (event.source.kind === "user" ? [event.source.nodeId] : []))
    for (const control of controls) expect(drawn.has(control)).toBe(true)
  })
  test("Arabic draws right to left with the page's own labels", () => {
    const ar = sheetMarkup(document, decision, "ar")
    expect(ar).toContain('dir="rtl"')
    expect(ar).toContain("قرّر")
  })
  test("decide sends the choice, a non-empty note, and the key", () => {
    const request = decideRequest("dec_1", { choice: "approve", note: "  " }, "k1")
    expect(request.url).toBe("/v1/decisions/dec_1:decide")
    expect(JSON.parse(String(request.init.body))).toEqual({ choice: "approve", idempotencyKey: "k1" })
    expect(needsConfirm(decision, "approve")).toBe(true)
    expect(needsConfirm(decision, "reject")).toBe(false)
  })
})

const pkg = process.env.NORN_CHECKER_WASM_PKG
const withEngine = pkg ? describe : describe.skip
const checker = pkg
  ? ((await import(pathToFileURL(join(pkg, "norn_checker_wasm.js")).href)) as {
      initSync(options: { module: Uint8Array }): unknown
      evaluateExpression(expression: string, inputJson: string, profile?: string): string
    })
  : null
checker?.initSync({ module: readFileSync(join(pkg ?? "", "norn_checker_wasm_bg.wasm")) })
const evaluate: Evaluate = checker ? wasmEvaluate(checker) : () => {
  throw new Error("no checker engine")
}

withEngine("the machine decides which controls exist", () => {
  const offered = (snapshot: Snapshot) =>
    ["decide-button", "confirm-button", "back-button", "retry-button", "close-button", "stay-button", "leave-button", "option-choice"].filter((node) => enabled(machine, evaluate, snapshot, node))
  test("decide waits for a choice; a choice that starts an action asks for confirmation", () => {
    const opened = init(machine).snapshot
    expect(offered(opened)).toEqual(["close-button", "option-choice"])
    const chosen = update(machine, evaluate, opened, { kind: "user", eventId: "choose", payload: { value: "approve", confirm: true } }).snapshot
    expect(offered(chosen)).toEqual(["decide-button", "close-button", "option-choice"])
    const confirming = update(machine, evaluate, chosen, { kind: "user", eventId: "decide" })
    expect(confirming.snapshot.stateId).toBe("confirming")
    expect(confirming.cmds).toEqual([])
    const sending = update(machine, evaluate, confirming.snapshot, { kind: "user", eventId: "confirm" })
    expect(sending.cmds[0]).toMatchObject({ kind: "start", call: { kind: "operation", operationId: "decideDecision" }, input: { choice: "approve", note: "" }, deadlineMs: 20000 })
    expect(offered(sending.snapshot)).toEqual([])
  })
  test("a refusal routes by the server's category and code", () => {
    let snapshot = init(machine).snapshot
    snapshot = update(machine, evaluate, snapshot, { kind: "user", eventId: "choose", payload: { value: "reject", confirm: false } }).snapshot
    snapshot = update(machine, evaluate, snapshot, { kind: "user", eventId: "decide" }).snapshot
    expect(snapshot.stateId).toBe("sending")
    const outcome = (category: "conflict" | "invalid" | "permission" | "visibility", code: string) =>
      update(machine, evaluate, snapshot, { kind: "outcome", entry: snapshot.entry, outcome: "refused", category, payload: { code, retryable: false } }).snapshot.stateId
    expect(outcome("conflict", "decision.resolved")).toBe("resolved")
    expect(outcome("conflict", "decision.idempotency_conflict")).toBe("refused")
    expect(outcome("invalid", "decision.expired")).toBe("expired")
    expect(outcome("invalid", "decision.unknown_choice")).toBe("refused")
    expect(outcome("visibility", "decision.not_found")).toBe("gone")
    expect(outcome("permission", "decision.not_a_decider")).toBe("refused")
  })
  test("every transition of the sheet has a replayable walk", () => {
    const all = walks(machine, evaluate, sheetPayloads)
    expect(all.map((walk) => walk.transitionId)).toEqual(machine.transitions.map((transition) => transition.transitionId))
    for (const walk of all) {
      let snapshot = init(machine).snapshot
      for (const step of walk.steps) {
        const next = update(machine, evaluate, snapshot, step.msg)
        expect(next.fired?.transitionId).toBe(step.transition.transitionId)
        snapshot = next.snapshot
      }
    }
  })
})
