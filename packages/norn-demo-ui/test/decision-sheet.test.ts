// The decision sheet's markup, and which controls its admitted machine offers in each state, read
// with the checker's engine. Browser rows (Studio's e2e) replay every transition; these rows pin
// the document-level contract without a DOM.

import { describe, expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { parseFragment } from "parse5"

import { type SheetDecision, type SheetDocument, decideRequest, directCall, needsConfirm, refusalPanelMarkup, sheetMarkup } from "../src/elements/decision-sheet.ts"
import { SHEET_CSS } from "../src/elements/sheet.css.ts"
import { type Evaluate, type Snapshot, enabled, init, update, walks, wasmEvaluate } from "../../norn-react/src/machine/index.ts"
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
  test("an approval of a direct call offers no run to open; a flow's ask and a flow step's call do", () => {
    const call = { action: "canon.holding.adjust", definition: "ops.adjust_approval", input: {} }
    const direct = { ...decision, run: { id: "norn.approval/abc", state: null }, call }
    const step = { ...decision, run: { id: "run_1", state: "waiting" }, call }
    expect([directCall(direct), directCall(step), directCall(decision)]).toEqual([true, false, false])
    const runLinks = (d: SheetDecision) => all(parseFragment(sheetMarkup(document, d, "en")) as unknown as Node)
      .filter((n) => n.nodeName === "a" && (attr(n, "href") ?? "").startsWith("/runs/"))
    expect(runLinks(direct)).toHaveLength(0)
    expect(runLinks(step)).toHaveLength(1)
    expect(runLinks(decision)).toHaveLength(1)
  })
  test("Arabic draws right to left with the page's own labels", () => {
    const ar = sheetMarkup(document, decision, "ar")
    expect(ar).toContain('dir="rtl"')
    expect(ar).toContain("قرّر")
    expect(ar).toContain("عرض الحلقة")
  })
  test("outcome markup reserves a hidden See episode link for when the cell supplies episodeId", () => {
    const episodeLinks = tree.filter((n) => n.nodeName === "a" && attr(n, "data-region") === "outcome-episode")
    expect(episodeLinks).toHaveLength(1)
    expect(episodeLinks[0]!.attrs?.some((a) => a.name === "hidden")).toBe(true)
    expect(sheetMarkup(document, decision, "en")).toContain("See episode")
  })
  test("decide sends the choice, a non-empty note, and the key", () => {
    const request = decideRequest("dec_1", { choice: "approve", note: "  " }, "k1")
    expect(request.url).toBe("/v1/decisions/dec_1:decide")
    expect(JSON.parse(String(request.init.body))).toEqual({ choice: "approve", idempotencyKey: "k1" })
    expect(needsConfirm(decision, "approve")).toBe(true)
    expect(needsConfirm(decision, "reject")).toBe(false)
  })
  test("refusalPanelMarkup distinguishes unevaluated decider laws from evaluated law violations and redacted refusals", () => {
    const receivedAt = new Date("2027-01-15T09:30:00Z")
    const laws = {
      "canon.refund.limit.law": { id: "canon.refund.limit", name: "Refund authority limit", rationale: "Refunds over 1,000 EUR need finance approval." },
    }
    // 1. Disclosed unevaluated decider law (empty values, cannot-evaluate explanation): renders "Not evaluated", never "Fails" or choice as checked operand.
    const unevaluated = refusalPanelMarkup(
      {
        code: "decision.law_not_satisfied",
        message: "the decider law is not satisfied",
        explanation: "this decision names a law the cell cannot evaluate for the caller",
        decidedAt: "2027-01-15T08:00:00Z",
        reason: {
          law: "canon.refund.limit",
          pin: { artifactId: "canon.refund.limit.law", version: "sha256:1" },
          subject: "decision/dec_1",
          values: {},
        },
      },
      { decision, choiceLabel: "Approve", note: "Customer asked", locale: "en", technical: false, caller: "user:sara", laws, receivedAt },
    )
    expect(unevaluated.variant).toBe("disclosed")
    expect(unevaluated.html).toContain('data-verdict="not-evaluated"')
    expect(unevaluated.html).toContain("Not evaluated")
    expect(unevaluated.html).toContain("This choice could not be checked against a rule")
    expect(unevaluated.html).not.toContain('data-verdict="fails"')
    expect(unevaluated.html).not.toContain("data-checked=")
    expect(unevaluated.html).toContain("Decided at")
    expect(unevaluated.html).toContain('datetime="2027-01-15T08:00:00Z"')
    expect(unevaluated.html).toContain("user:sara")

    // Even if a legacy payload includes `choice` in `reason.values` on `decision.law_not_satisfied`, it is never shown as an evaluated operand.
    const legacyChoiceOnly = refusalPanelMarkup(
      {
        code: "decision.law_not_satisfied",
        message: "the decider law is not satisfied",
        decidedAt: "2027-01-15T08:00:00Z",
        reason: {
          law: "canon.refund.limit",
          subject: "decision/dec_1",
          values: { choice: "approve" },
        },
      },
      { decision, choiceLabel: "Approve", note: "", locale: "en", technical: false, caller: "user:sara", laws, receivedAt },
    )
    expect(legacyChoiceOnly.html).toContain('data-verdict="not-evaluated"')
    expect(legacyChoiceOnly.html).not.toContain('data-checked="choice"')

    // 2. Disclosed evaluated law failure (`core.laws.violated` with real evaluated operands): renders "Fails" and checked operand chips.
    const evaluated = refusalPanelMarkup(
      {
        code: "core.laws.violated",
        message: "a declared precondition does not hold",
        decidedAt: "2027-01-15T08:00:00Z",
        reason: {
          law: "canon.refund.limit",
          pin: { artifactId: "canon.refund.limit.law", version: "sha256:1" },
          subject: "canon.Refund/ref-1",
          values: { amount: "1240.00", role_limit: "1000.00" },
        },
      },
      { decision, choiceLabel: "Approve", note: "Customer asked", locale: "en", technical: false, caller: "user:sara", laws, receivedAt },
    )
    expect(evaluated.variant).toBe("disclosed")
    expect(evaluated.html).toContain("Refunds over 1,000 EUR need finance approval.")
    expect(evaluated.html).toContain('data-verdict="fails"')
    expect(evaluated.html).toContain('data-checked="amount"')
    expect(evaluated.html).toContain('data-checked="role_limit"')
    expect(evaluated.html).toContain("Decided at")

    // 3. Redacted refusal: hides law/values and labels local timestamp as "Received at" when server omits `decidedAt`.
    const redacted = refusalPanelMarkup(
      { code: "decision.law_not_satisfied", message: "the decider law is not satisfied", reason: null },
      { decision, choiceLabel: "Approve", note: "", locale: "en", technical: false, caller: "user:member", laws, receivedAt },
    )
    expect(redacted.variant).toBe("redacted")
    expect(redacted.html).toContain("Received at")
    expect(redacted.html).not.toContain("Decided at")
    expect(redacted.html).not.toContain("Refund authority limit")
    expect(redacted.html).not.toContain("data-checked=")

    // 4. Arabic refusal uses Western (latn) digits and wraps technical codes/pins in <bdi dir="ltr">.
    const arRefusal = refusalPanelMarkup(
      {
        code: "core.laws.violated",
        message: "a declared precondition does not hold",
        decidedAt: "2027-01-15T08:00:00Z",
        reason: {
          law: "canon.refund.limit",
          pin: { artifactId: "canon.refund.limit.law", version: "sha256:1" },
          subject: "canon.Refund/ref-1",
          values: { amount: "1240.00" },
        },
      },
      { decision, choiceLabel: "Approve", note: "", locale: "ar", technical: true, caller: "user:sara", laws, receivedAt },
    )
    expect(arRefusal.html).toContain('<bdi dir="ltr">core.laws.violated · canon.Refund/ref-1</bdi>')
    expect(arRefusal.html).toContain('<bdi dir="ltr">canon.refund.limit.law@sha256:1</bdi>')
    expect(arRefusal.html).not.toMatch(/[\u0660-\u0669]/)
  })

  test("SHEET_CSS follows DESIGN.md weights, dot-first chip, danger tokens, reduced motion, and touch targets", () => {
    expect(SHEET_CSS).not.toContain("font-weight: 600")
    expect(SHEET_CSS).toContain(".norn-chip::before")
    expect(SHEET_CSS).toContain("--destructive-text")
    expect(SHEET_CSS).toContain("--destructive-foreground")
    expect(SHEET_CSS).toContain("prefers-reduced-motion: reduce")
    expect(SHEET_CSS).toContain("min-height: 44px")
  })
})

const pkg = process.env.NORN_CHECKER_WASM_PKG
if (!pkg && process.env.NORN_CHECKER_WASM_REQUIRED === "1") {
  throw new Error("NORN_CHECKER_WASM_PKG is required when NORN_CHECKER_WASM_REQUIRED=1")
}
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
