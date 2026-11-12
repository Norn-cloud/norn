import { expect, test } from "bun:test"
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { loadInvoice, notify } from "../../../tools/norn-ts/fixtures/flows/support/billing.builder.ts"
import { lower } from "../src/lib/canvas/automation.ts"
import { formatDuration } from "../src/decision-tree.tsx"
import { DECISION_FN as RUNS_DECISION } from "../../../tools/norn-ts/src/builder/index.ts"
import { DECISION_FN, decisionView, nextOptionId, optionPort, setOptions, type DecisionOption } from "../src/lib/canvas/decision.ts"
import type { CanvasEdge, CanvasNode, Graph } from "../src/lib/canvas/graph.ts"

const tools = [loadInvoice, notify]
const meta = { id: "billing.overdue_review", version: "1.0.0" }
const scope = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "apps/studio/canvas" }
const node = (id: string, kind: string, config: Record<string, string> = {}): CanvasNode => ({ id, kind, position: { x: 0, y: 0 }, config })
const edge = (source: string, sourcePort: string, target: string): CanvasEdge => ({ id: `${source}.${sourcePort}`, source, sourcePort, target, targetPort: "in", config: {} })
const options = (...all: DecisionOption[]) => JSON.stringify(all)

/** Load an invoice, ask whether to remind; reminding asks a follow-up. */
const graph: Graph = {
  nodes: [
    node("start", "start"),
    node("load", "perform", { tool: "billing.load_invoice", bind: "invoice", input: JSON.stringify({ invoiceId: "inv-1" }) }),
    node("chase", "decide", { question: "Invoice is overdue: what should I do?", risk: "high", deciderRoles: "admin", deciderLaw: "billing.reminder_needs_a_person", expiresInMs: "14400000", agentPick: "o1", options: options({ id: "o1", label: "Send a reminder", scope: "Email · customer", effect: { binding: "billing.notify" } }, { id: "o2", label: "Leave it", scope: "Nothing sent", effect: { action: "billing.leave_open" } }) }),
    node("remind", "perform", { tool: "billing.notify", input: JSON.stringify({ invoiceId: "invoice.id", outcome: "overdue" }) }),
    node("escalate", "decide", { question: "Tell the account owner too?", risk: "low", deciderRoles: "admin", options: options({ id: "o1", label: "Yes", scope: "#accounts", effect: { binding: "billing.notify" } }, { id: "o2", label: "No", scope: "Nothing more", effect: { binding: "billing.notify" } }) }),
    node("reminded", "succeed", { outcome: "reminded" }),
    node("escalated", "succeed", { outcome: "escalated" }),
    node("left", "succeed", { outcome: "left" }),
  ],
  edges: [
    edge("start", "next", "load"), edge("load", "next", "chase"),
    edge("chase", "opt:o1", "remind"), edge("remind", "next", "escalate"),
    edge("escalate", "opt:o1", "escalated"), edge("escalate", "opt:o2", "reminded"),
    edge("chase", "opt:o2", "left"),
  ],
}

test("an ask step lowers to a wait for norn.decision and a route on the choice", () => {
  const { lowered, problems } = lower(graph, tools, meta, scope)
  expect(problems).toEqual([])
  expect(lowered?.states.get("chase")).toContain("awaiting_perform_ask_chase")
  expect(lowered?.states.get("escalate")).toContain("awaiting_perform_ask_escalate")
  // The rule without the canvas layout extension (which carries the drawing's own config).
  const rule = JSON.stringify(lowered?.artifacts.find(({ kind }) => kind === "rule"), (key, value: unknown) => key === "extensions" ? undefined : value)
  expect(rule).toContain("norn.decision")
  // The relayed contract: decider {roles, law?}, options with effect {action} | {binding}, optional expiresInMs.
  for (const text of ["decider", "billing.reminder_needs_a_person", "expiresInMs", "14400000", "billing.leave_open"]) expect(rule).toContain(text)
  expect(rule).not.toContain("deciderRoles")
})

test("the canvas lowers decisions with RUNS' built-in handle, not a copy of it", () => {
  expect(DECISION_FN).toBe(RUNS_DECISION)
  expect(DECISION_FN).toMatchObject({ id: "norn.decision", version: "1" })
})

test("the canvas names the bounds the cell enforces: question length, option count, expiry", () => {
  const long: Graph = { ...graph, nodes: graph.nodes.map((one) => one.id === "chase" ? { ...one, config: { ...one.config, question: "q".repeat(501), expiresInMs: String(8 * 24 * 3_600_000) } } : one) }
  expect(lower(long, tools, meta, scope).problems.map(({ code }) => code).sort()).toEqual(["canvas.decision.expiry_too_long", "canvas.decision.question_too_long"])
  // The cell refuses expiresInMs 0 (decision.expires_in_ms), so the canvas does too.
  const instant: Graph = { ...graph, nodes: graph.nodes.map((one) => one.id === "chase" ? { ...one, config: { ...one.config, expiresInMs: "0" } } : one) }
  expect(lower(instant, tools, meta, scope).problems.map(({ code }) => code)).toEqual(["canvas.decision.expiry_too_short"])
})

test("the decider's tree: question, options with the agent's pick and their tools, and the follow-up", () => {
  const view = decisionView(graph, "chase")
  expect(view?.risk).toBe("high")
  expect(view?.options.map(({ option, agentPick, tools: called }) => `${option.label}${agentPick ? "*" : ""}:${called.join("+")}`)).toEqual(["Send a reminder*:billing.notify", "Leave it:"])
  expect(view?.options[0]?.followUp?.question).toBe("Tell the account owner too?")
  expect(view?.options[1]?.followUp).toBeNull()
})

test("removing an option removes its exit and clears the agent's pick when it was that option", () => {
  const chase = graph.nodes.find(({ id }) => id === "chase")!
  const kept = setOptions(graph, "chase", [{ id: "o2", label: "Leave it", scope: "Nothing sent" }])
  expect(kept.edges.some(({ source, sourcePort }) => source === "chase" && sourcePort === optionPort({ id: "o1" }))).toBe(false)
  expect(kept.nodes.find(({ id }) => id === "chase")?.config.agentPick).toBe("")
  expect(nextOptionId(JSON.parse(chase.config.options ?? "[]"))).toBe("o3")
})

test("the canvas names a decision without a question, options or deciders, and an option that leads nowhere", () => {
  const bare: Graph = { nodes: [node("start", "start"), node("ask", "decide", { options: options({ id: "o1", label: "Go", scope: "" }) })], edges: [edge("start", "next", "ask")] }
  expect(lower(bare, tools, meta, scope).problems.map(({ code }) => code).sort()).toEqual(["canvas.decision.no_decider", "canvas.decision.no_question", "canvas.decision.option_leads_nowhere", "canvas.decision.option_without_effect"])
})

// The real checker over what an automation with decisions lowers to (built into worker/checker or src/checker).
// CI's browser phase builds the package and sets NORN_CHECKER_WASM_REQUIRED=1, so there a missing
// package fails this case instead of skipping it; the cheap phase builds no WASM and skips it.
const pkg = [join(import.meta.dir, "../worker/checker"), join(import.meta.dir, "../src/checker")].find((dir) => existsSync(join(dir, "norn_checker_wasm_bg.wasm")))
const required = process.env.NORN_CHECKER_WASM_REQUIRED === "1"
test.skipIf(!pkg && !required)("the checker admits what a drawn decision lowers to", async () => {
  expect(pkg, "checker WASM package (crates/norn-checker-wasm/scripts/build-pkg.sh)").toBeDefined()
  const wasm = await import(join(pkg!, "norn_checker_wasm.js")) as { initSync: (input: { module: BufferSource }) => unknown; checkArtifactSet: (a: string, c: string) => string }
  wasm.initSync({ module: readFileSync(join(pkg!, "norn_checker_wasm_bg.wasm")) })
  const artifacts = lower(graph, tools, meta, scope).lowered?.artifacts ?? []
  const result = JSON.parse(wasm.checkArtifactSet(JSON.stringify(artifacts), "null")) as { diagnostics: { diagnostics: { code: string; message: string }[] } }
  expect(result.diagnostics.diagnostics).toEqual([])
})

test("the decider reads the real time limit, never rounded to whole hours", () => {
  expect(formatDuration(30 * 60_000)).toBe("30 min")
  expect(formatDuration(10 * 60_000)).toBe("10 min")
  expect(formatDuration(90 * 60_000)).toBe("1 h 30 min")
  expect(formatDuration(4 * 3_600_000)).toBe("4 h")
  expect(formatDuration(52 * 3_600_000)).toBe("2 d 4 h")
})
