import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { expect, test, type Page } from "@playwright/test"
import { type Machine, type Msg, type Walk, wasmEvaluate } from "../../../../packages/norn-react/src/machine/index.ts"
import type { WidgetUx } from "../../../../packages/norn-react/src/index.ts"
import { demoDefinitions, MATRIX_WIDGET_ID, matrixArtifact } from "../../../../packages/norn-react/test/fixtures/matrix-page.ts"
import { answerFor, matrixCell, type NotServed } from "./cell.ts"
import { matrixOf, nodesById } from "./manifest.ts"

// The generated state matrix (#1762 milestone C). From the fixture page's admitted Widget UX
// document and `walk.ts`, every transition gets a row that drives the page there through UI SDK's
// page agent (`createAgent`: `snapshot` / `act`), as a browser agent would, against a stand-in cell
// that answers with the real wire envelopes. A thin raw-Playwright slice checks what an agent cannot:
// pointer hit-testing, Escape and the focus trap, 390px overflow, RTL, axe and UI SDK's ARIA pattern
// check. One `verify` row sweeps every reachable state across light/dark × desktop/390 × LTR/RTL and
// writes the report. The clock is the test's (`page.clock`); nothing sleeps.

const artifact = matrixArtifact()
const ux = artifact.payload.ux as WidgetUx
const machine = ux.pages[0]!.machine as Machine
const definitions = [...demoDefinitions(), artifact]
const checkerDir = join(import.meta.dirname, "../../src/checker")
const checker = (await import(pathToFileURL(join(checkerDir, "norn_checker_wasm.js")).href)) as {
  initSync(options: { module: Uint8Array }): unknown
  evaluateExpression(expression: string, inputJson: string, profile?: string): string
}
checker.initSync({ module: readFileSync(join(checkerDir, "norn_checker_wasm_bg.wasm")) })
const matrix = matrixOf(machine, wasmEvaluate(checker), (transition) => (transition.on === "edit-note" ? [{ value: "Checked twice" }] : undefined))
const nodes = nodesById(ux)
const AXE = createRequire(import.meta.url).resolve("axe-core/axe.min.js")
/** Room for the expression engine (WASM) to compile in a fresh page under the suite's load. */
const ENGINE_MS = 30_000

test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the matrix stand-in cell`) })

interface Variant { readonly theme: "light" | "dark"; readonly width: 1280 | 390; readonly dir: "ltr" | "rtl" }
const DEFAULT: Variant = { theme: "light", width: 1280, dir: "ltr" }
const localeOf = (variant: Variant) => (variant.dir === "rtl" ? "ar" : "en")

type Cell = Awaited<ReturnType<typeof matrixCell>>

/** Queue the stand-in's answer to each call `step` starts: the next step's outcome, or none (held). */
function queueFor(cell: Cell, step: Walk["steps"][number] | undefined, next: Msg | undefined, notServed: NotServed) {
  for (const effect of step?.transition.effects ?? []) {
    if (effect.kind !== "start") continue
    const kind = effect.call.kind === "read" ? "read" : "action"
    // A call the walk never answers is held, never left to the stand-in: an instant answer would move the page past the state asserted.
    ;(kind === "read" ? cell.reads : cell.calls).push(next?.kind === "outcome" ? answerFor(next, kind, notServed) : null)
  }
}

/** Open the matrix page; the page's own `page_opened` starts its first read, so that answer is queued first. */
async function open(page: Page, walk: Walk, variant: Variant, notServed: NotServed) {
  await page.setViewportSize({ width: variant.width, height: 900 })
  const cell = await matrixCell(page, definitions)
  if (walk.steps[0]?.msg.kind === "page_opened") queueFor(cell, walk.steps[0], walk.steps[1]?.msg, notServed)
  await page.clock.install({ time: new Date("2026-11-25T09:00:00Z") })
  await page.goto(`/matrix.html?widget=${MATRIX_WIDGET_ID}&theme=${variant.theme}&dir=${variant.dir}&locale=${localeOf(variant)}`)
  const root = page.locator("[data-norn-page]")
  await expect(root).toHaveAttribute("data-engine", "ready", { timeout: ENGINE_MS })
  return { root, cell }
}

/** The label a control node shows in `variant`'s locale. */
function labelOf(nodeId: string, variant: Variant): string {
  const component = nodes.get(nodeId)?.component
  if (component?.kind === "control") return component.label[localeOf(variant)] ?? component.label.en ?? nodeId
  if (component?.kind === "action") {
    const placement = ux.actionPlacements.find((candidate) => candidate.placementId === component.placementId)
    return placement?.label[localeOf(variant)] ?? placement?.label.en ?? nodeId
  }
  return nodeId === "meter-node" ? "Pick" : nodeId
}

/** One step through UI SDK's page agent: the component named `label`, then `action` on it. */
async function agentAct(page: Page, label: string, action: string, args: readonly unknown[] = []) {
  return page.evaluate(async ({ label, action, args }) => {
    const agent = (window as unknown as { nornAgent: { snapshot(): { ref: string; label?: string }[]; act(input: unknown): Promise<{ ok: boolean; error?: { message: string } }> } }).nornAgent
    const found = agent.snapshot().find((component) => component.label === label)
    if (found === undefined) return { ok: false, error: { message: `no component labelled ${label}` } }
    return agent.act({ steps: [{ ref: found.ref, action, args }] })
  }, { label, action, args })
}

/** Replay `walk`: a person's events through the page agent (a confirm-marked one by a person), outcomes from the stand-in. */
async function replay(page: Page, walk: Walk, variant: Variant = DEFAULT, notServed: NotServed = "404") {
  const { root, cell } = await open(page, walk, variant, notServed)
  for (const [index, { msg, transition }] of walk.steps.entries()) {
    const where = `${walk.transitionId}: step ${index + 1} (${transition.transitionId})`
    // The page opened itself; every later step's calls get the next step's outcome, queued before the step.
    if (msg.kind !== "page_opened") {
      queueFor(cell, walk.steps[index], walk.steps[index + 1]?.msg, notServed)
      await act(page, msg, transition.from, variant, cell)
    }
    if (msg.kind === "outcome" && msg.outcome === "timeout") await page.clock.runFor(machine.states.find((state) => state.stateId === transition.from)?.deadlineMs ?? 0)
    await expect(root, where).toHaveAttribute("data-state", transition.target)
  }
  return { root, cell }
}

async function act(page: Page, msg: Msg, from: string, variant: Variant, cell: Cell) {
  if (msg.kind === "outcome") {
    // The answer to the call the state awaits; released once the page has sent it, never early.
    const held = machine.states.find((state) => state.stateId === from)?.awaits?.kind === "action" ? cell.calls : cell.reads
    await expect.poll(() => held.inFlight).toBe(true)
    held.release()
    return
  }
  if (msg.kind !== "user") return
  const event = machine.events.find((candidate) => candidate.eventId === msg.eventId)!
  const nodeId = event.source.kind === "user" ? event.source.nodeId : ""
  if (nodeId === "confirm-layer") {
    // The layer's own close: Escape, a person's key.
    await page.keyboard.press("Escape")
    return
  }
  const label = labelOf(nodeId, variant)
  if ((await page.locator(`[data-node="${nodeId}"][data-agent-confirm]`).count()) > 0) {
    // A press that starts a ledgered action needs a person (L6, `data-agent-confirm`): the agent is refused, a person presses.
    const refused = await agentAct(page, label, "press")
    expect(refused.ok, `the agent may not press ${label}`).toBe(false)
    await page.locator(`[data-node="${nodeId}"]`).click()
    return
  }
  const result = nodes.get(nodeId)?.component.kind === "control" && (nodes.get(nodeId)!.component as { input: string }).input === "text"
    ? await agentAct(page, label, "setValue", [String(msg.payload?.value ?? "")])
    : await agentAct(page, label, "press")
  expect(result.ok, `${label}: ${result.error?.message ?? ""}`).toBe(true)
}

test("the matrix walks every transition and reaches every declared state", () => {
  expect(matrix.walks.map((walk) => walk.transitionId)).toEqual(machine.transitions.map((transition) => transition.transitionId))
  expect(matrix.unreached).toEqual([])
  expect(matrix.transient).toEqual([machine.initialStateId])
  // Every pending state's outcomes: the read's six, the save's six.
  expect(matrix.outcomes.length).toBe(12)
})

for (const walk of matrix.walks) {
  test(`walk: ${walk.transitionId}`, async ({ page }) => {
    await replay(page, walk)
  })
}

test("not served: the deployment's no-host 503 lands where a 404 does", async ({ page }) => {
  const walk = matrix.walks.find((candidate) => candidate.transitionId === "read-not_served")!
  await replay(page, walk, DEFAULT, "no-host")
  await expect(page.locator('[data-read="not_served"]')).toBeVisible()
})

test("L6: masked values are secret, withheld ones hidden, a ledgered confirm needs a person", async ({ page }) => {
  const { root } = await replay(page, matrix.states.get("confirming")!)
  await expect(root.locator('[data-mask="redacted"]')).toHaveAttribute("data-agent-secret", "")
  await expect(root.locator('[data-mask="redacted"]')).toHaveText("•••")
  await expect(root.locator('[data-value="withheld"]')).toHaveAttribute("data-agent-hidden", "")
  const confirm = page.getByRole("button", { name: "Confirm", exact: true })
  await expect(confirm).toHaveAttribute("data-agent-confirm", /ledger/)
  // The agent is refused the ledgered confirm and the page stays where a person must act.
  const refused = await agentAct(page, "Confirm", "press")
  expect(refused.ok).toBe(false)
  await expect(root).toHaveAttribute("data-state", "confirming")
})

test("Escape: while the layer saves, it refuses and keeps focus inside", async ({ page }) => {
  const { root, cell } = await replay(page, matrix.states.get("saving")!)
  const layer = page.locator('[data-layer="alert-dialog"]')
  await expect(layer).toBeVisible()
  // Focus stays in the layer: Tab cycles within it.
  for (let step = 0; step < 4; step += 1) {
    await page.keyboard.press("Tab")
    expect(await page.evaluate(() => document.activeElement?.closest("[data-layer]") !== null)).toBe(true)
  }
  await page.keyboard.press("Escape")
  await expect(root).toHaveAttribute("data-state", "saving")
  await expect(layer).toBeVisible()
  expect(cell.sent).toHaveLength(1)
})

test("Escape: while the layer asks, it closes it", async ({ page }) => {
  const { root } = await replay(page, matrix.states.get("confirming")!)
  await page.keyboard.press("Escape")
  await expect(root).toHaveAttribute("data-state", "browsing")
  await expect(page.locator('[data-layer="alert-dialog"]')).toHaveCount(0)
})

test("ARIA: the open layer passes UI SDK's alertdialog pattern check", async ({ page }) => {
  await replay(page, matrix.states.get("confirming")!)
  const problems = await page.evaluate(async () => {
    const check = (window as unknown as { nornCheckPattern(root: Element, pattern: string): Promise<void> }).nornCheckPattern
    const layer = document.querySelector('[data-layer="alert-dialog"]')
    if (layer === null) return "no layer"
    try {
      await check(layer, "alertdialog")
      return null
    } catch (error) {
      return String(error)
    }
  })
  expect(problems).toBeNull()
})

interface VerifyCell { readonly state: string; readonly variant: string; readonly reached: boolean; readonly overflow: number; readonly hitMisses: readonly string[]; readonly axe: readonly { readonly id: string; readonly impact: string | null; readonly nodes: number }[] }

/** Buttons a pointer at their centre does not reach (inside the open layer when there is one). */
const hitMisses = (page: Page) => page.evaluate(() => {
  const scope = document.querySelector("[data-layer]") ?? document
  return [...scope.querySelectorAll<HTMLElement>("button:not([disabled]):not([aria-disabled='true'])")].flatMap((button) => {
    button.scrollIntoView({ block: "center", inline: "center" })
    const box = button.getBoundingClientRect()
    if (box.width === 0 || box.height === 0) return []
    const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2)
    return hit !== null && (hit === button || button.contains(hit)) ? [] : [button.textContent ?? "?"]
  })
})

test("verify: every state × light/dark × desktop/390 × LTR/RTL, plus axe", async ({ browser }, info) => {
  test.setTimeout(900_000)
  const variants: Variant[] = []
  for (const theme of ["light", "dark"] as const) for (const width of [1280, 390] as const) for (const dir of ["ltr", "rtl"] as const) variants.push({ theme, width, dir })
  const cells: VerifyCell[] = []
  for (const [state, walk] of matrix.states) {
    for (const variant of variants) {
      const name = `${variant.theme}/${variant.width}/${variant.dir}`
      // A fresh context per cell: its own clock and routes, nothing carried from the last.
      const context = await browser.newContext({ baseURL: info.project.use.baseURL, colorScheme: variant.theme })
      const page = await context.newPage()
      const { root } = await replay(page, walk, variant)
      const reached = (await root.getAttribute("data-state")) === state
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
      const misses = await hitMisses(page)
      await page.addScriptTag({ path: AXE })
      const violations = await page.evaluate(async () => {
        const axe = (window as unknown as { axe: { run(context: unknown, options: unknown): Promise<{ violations: { id: string; impact: string | null; nodes: unknown[] }[] }> } }).axe
        return (await axe.run(document, { resultTypes: ["violations"] })).violations.map((violation) => ({ id: violation.id, impact: violation.impact, nodes: violation.nodes.length }))
      })
      cells.push({ state, variant: name, reached, overflow, hitMisses: misses, axe: violations })
      expect.soft(reached, `${state} ${name}: reached`).toBe(true)
      expect.soft(overflow, `${state} ${name}: horizontal overflow`).toBeLessThanOrEqual(1)
      expect.soft(misses, `${state} ${name}: pointer hit-testing`).toEqual([])
      expect.soft(violations.filter((violation) => violation.impact === "serious" || violation.impact === "critical"), `${state} ${name}: axe`).toEqual([])
      await context.close()
    }
  }
  const failing = cells.filter((cell) => !cell.reached || cell.overflow > 1 || cell.hitMisses.length > 0 || cell.axe.some((violation) => violation.impact === "serious" || violation.impact === "critical"))
  const report = {
    format: "norn-widget-matrix/verify@1",
    widget: MATRIX_WIDGET_ID,
    transitions: machine.transitions.length,
    walks: matrix.walks.length,
    states: [...matrix.states.keys()],
    outcomes: matrix.outcomes.length,
    variants: variants.length,
    cells: cells.length,
    failing: failing.length,
    axeMinor: cells.reduce((sum, cell) => sum + cell.axe.filter((violation) => violation.impact !== "serious" && violation.impact !== "critical").length, 0),
    rows: cells,
  }
  const dir = join(info.project.outputDir, "matrix-verify")
  mkdirSync(dir, { recursive: true })
  writeFileSync(join(dir, "verify.json"), `${JSON.stringify(report, null, 2)}\n`)
  console.log(`matrix verify: ${report.cells} cells (${report.states.length} states × ${report.variants} variants), ${report.failing} failing, ${report.axeMinor} minor axe findings`)
})
