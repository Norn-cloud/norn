import { readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { expect, test, type Page, type Route } from "@playwright/test"
import { type Machine, type Msg, walks, wasmEvaluate } from "../../../packages/norn-demo-ui/src/machine/index.ts"
import { sheetPayloads } from "../../../packages/norn-demo-ui/test/support/sheet-payloads.ts"
import { decisionCell } from "./decision-cell.ts"
import { session } from "./object-cell.ts"

// Model-based rows (UI-MACHINE-1): one browser test per transition of the decision sheet's
// admitted machine, generated from the machine itself. Each walk is the shortest message path that
// fires its transition; a person's events become clicks and edits on the controls the page draws,
// and the server's outcomes become the stand-in's answers. After every step the page must be in
// the state the machine says. A transition added to the document gets its row with no new code.

const document = JSON.parse(readFileSync(join(import.meta.dirname, "../src/decisions/decision-sheet.widget.json"), "utf8")) as {
  payload: { ux: { pages: { machine: Machine }[] } }
}
const machine = document.payload.ux.pages[0]!.machine
const engine = join(import.meta.dirname, "../src/checker")
const checker = (await import(pathToFileURL(join(engine, "norn_checker_wasm.js")).href)) as {
  initSync(options: { module: Uint8Array }): unknown
  evaluateExpression(expression: string, inputJson: string, profile?: string): string
}
checker.initSync({ module: readFileSync(join(engine, "norn_checker_wasm_bg.wasm")) })
const all = walks(machine, wasmEvaluate(checker), sheetPayloads)

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the decision route stand-in`) })

test("every transition of the sheet's machine has a walk", () => {
  expect(all.map((walk) => walk.transitionId)).toEqual(machine.transitions.map((transition) => transition.transitionId))
})

const DECISION = "dec_QXBwcm92ZS1yZWZ1bmQ"
/** Room for the expression engine to compile in a fresh page under the suite's load. */
const ENGINE_MS = 30_000
const STATUS = { visibility: 404, permission: 403, precondition: 422, conflict: 409, invalid: 422, limit: 429, internal: 500 } as const

/** The stand-in's answer to the next decide call, for one outcome message. `null` hangs (a timeout). */
function answer(msg: Extract<Msg, { kind: "outcome" }>): { status: number; body: unknown } | null {
  switch (msg.outcome) {
    case "accepted":
      return { status: 200, body: { outcome: "decided", decision: { id: DECISION, choice: "reject", decidedBy: "you", decidedAtMs: Date.UTC(2026, 10, 6, 9) }, receipt: { receiptHash: "a".repeat(64), commitSeq: 7, recordedAtMs: Date.UTC(2026, 10, 6, 9) }, run: { id: "run_refund_2", state: "running" } } }
    case "pending_approval":
      return { status: 202, body: { outcome: "pending", decision: { id: "apv_1", expiresAtMs: Date.UTC(2026, 10, 7) } } }
    case "network_failure":
      return { status: 500, body: "" }
    case "timeout":
    case "cancelled":
      return null
    case "answer_moved":
      return { status: 409, body: { code: "core.projection.answer_moved", message: "The answer moved." } }
    case "refused": {
      const category = msg.category ?? "precondition"
      const code = String(msg.payload?.code ?? "decision.law_not_satisfied")
      const resolution = code === "decision.resolved" ? { resolution: { resolution: "decided", by: "Mona Ali", atMs: Date.UTC(2026, 10, 6, 9, 5) } } : {}
      return { status: code === "decision.expired" ? 410 : STATUS[category], body: { format: "norn/refusal@2", code, category, retryable: false, message: `Refused: ${code}.`, ...resolution } }
    }
  }
}

async function openRefund(page: Page) {
  await session(page)
  await decisionCell(page)
  // Each queued answer is held until the walk reaches its outcome step, so the page is seen in the
  // pending state first and the answer can never race the assertion. Each call takes its answer when
  // it arrives, so a call that is never answered blocks no later one.
  type Pending = { answer: ReturnType<typeof answer>; gate: Promise<void>; open: () => void }
  const pending: Pending[] = []
  let inFlight: Pending | undefined
  // Registered after the stand-in, so it answers decide calls first.
  await page.route("**/api/host/v1/decisions/*:decide**", async (route: Route) => {
    const next = pending.shift()
    if (next === undefined) return route.fallback()
    inFlight = next
    await next.gate
    if (next.answer === null) return
    await route.fulfill({ status: next.answer.status, contentType: "application/json", body: typeof next.answer.body === "string" ? next.answer.body : JSON.stringify(next.answer.body) })
  })
  await page.clock.install()
  await page.goto("/decisions")
  await page.getByRole("list", { name: "Decisions waiting on you" }).getByRole("button").filter({ hasText: "Refund 1,240.00 USD" }).click()
  const sheet = page.locator("norn-decision-sheet .norn-sheet")
  // The sheet draws its first state at once; it takes input once the expression engine (WASM) is
  // here, which the walk waits for before its first step (the decisions rows time both).
  await expect(sheet).toHaveAttribute("data-state", machine.initialStateId)
  await expect(sheet).toHaveAttribute("data-engine", "ready", { timeout: ENGINE_MS })
  return {
    sheet,
    queue: (next: ReturnType<typeof answer>) => {
      let open = () => {}
      const gate = new Promise<void>((resolve) => { open = resolve })
      pending.push({ answer: next, gate, open })
    },
    /** Answer the call in flight. Waits until the page has sent it, so the answer cannot be early. */
    release: async () => {
      await expect.poll(() => inFlight !== undefined).toBe(true)
      inFlight?.open()
      inFlight = undefined
    },
  }
}

for (const walk of all) {
  test(`walk: ${walk.transitionId}`, async ({ page }) => {
    const { sheet, queue, release } = await openRefund(page)
    for (const [index, step] of walk.steps.entries()) {
      const { msg, transition } = step
      const next = walk.steps[index + 1]?.msg
      // The answer a started call gets is the next step's outcome, queued before the person acts.
      // A call the walk never answers is held, never left to the stand-in: an instant answer would
      // move the sheet past the state this step asserts.
      if (transition.effects?.some((effect) => effect.kind === "start")) queue(next?.kind === "outcome" ? answer(next) : null)
      if (msg.kind === "user") {
        const event = machine.events.find((candidate) => candidate.eventId === msg.eventId)!
        const node = event.source.kind === "user" ? event.source.nodeId : ""
        if (node === "option-choice") await sheet.locator(`ui-radio[value="${String(msg.payload?.value)}"]`).click()
        else if (node === "note-input") await sheet.locator("textarea").fill(String(msg.payload?.value ?? ""))
        else await sheet.locator(`button[data-node="${node}"]`).click()
      } else if (msg.kind === "outcome") await release()
      if (msg.kind === "outcome" && msg.outcome === "timeout") {
        await page.clock.runFor(machine.states.find((state) => state.stateId === transition.from)?.deadlineMs ?? 0)
      }
      const where = `${walk.transitionId}: step ${index + 1} (${transition.transitionId})`
      // A closed machine hands the sheet back to its host, which closes it.
      if (transition.target === "closed") await expect(sheet, where).toHaveCount(0)
      else await expect(sheet, where).toHaveAttribute("data-state", transition.target)
    }
  })
}
