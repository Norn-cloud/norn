import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { expect, test, type Page } from "@playwright/test"
import { type Machine, type Msg, type Walk, wasmEvaluate } from "../../../../packages/norn-react/src/machine/index.ts"
import type { WidgetUx } from "../../../../packages/norn-react/src/index.ts"
import { demoDefinitions, MATRIX_WIDGET_ID, matrixArtifact } from "../../../../packages/norn-react/test/fixtures/matrix-page.ts"
import { answerFor, matrixCell, RECORDS_ELSEWHERE, type NotServed } from "./cell.ts"
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
/** The expression engine's WASM, as the build serves it (`src/engine`, hashed by Vite). */
const ENGINE_WASM = /\/norn_engine_wasm_bg[^/]*\.wasm(\?|$)/

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

/**
 * Open the matrix page; the page's own `page_opened` starts its first read, so that answer is queued
 * first. `engine: "any"` opens it without waiting for the expression engine (a row that holds or fails it).
 */
async function open(page: Page, walk: Walk, variant: Variant, notServed: NotServed, engine: "ready" | "any" = "ready", environment?: { current: string }) {
  await page.setViewportSize({ width: variant.width, height: 900 })
  const cell = await matrixCell(page, definitions, environment)
  if (walk.steps[0]?.msg.kind === "page_opened") queueFor(cell, walk.steps[0], walk.steps[1]?.msg, notServed)
  await page.clock.install({ time: new Date("2026-11-25T09:00:00Z") })
  await page.goto(`/matrix.html?widget=${MATRIX_WIDGET_ID}&theme=${variant.theme}&dir=${variant.dir}&locale=${localeOf(variant)}`)
  const root = page.locator("[data-norn-page]")
  if (engine === "ready") await expect(root).toHaveAttribute("data-engine", "ready", { timeout: ENGINE_MS })
  return { root, cell }
}

/** Open the page with the engine not here (held or failing) and answer its first read: the page browses. */
async function browseWithoutEngine(page: Page) {
  const { root, cell } = await open(page, matrix.states.get("browsing")!, DEFAULT, "404", "any")
  await expect.poll(() => cell.reads.inFlight).toBe(true)
  cell.reads.release()
  await expect(root).toHaveAttribute("data-state", "browsing")
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

test("engine held: the note and the save wait for the engine, in order, and run once it is here", async ({ page }) => {
  let release = () => {}
  const held = new Promise<void>((resolve) => { release = resolve })
  await page.route(ENGINE_WASM, async (route) => {
    await held
    await route.continue()
  })
  const { root, cell } = await browseWithoutEngine(page)
  // The note's assignment needs the engine: it waits. Save needs none, yet waits behind it: nothing is reordered.
  expect((await agentAct(page, "Note", "setValue", ["Checked twice"])).ok).toBe(true)
  await expect(root).toHaveAttribute("data-waiting", "1")
  expect((await agentAct(page, "Save", "press")).ok).toBe(true)
  await expect(root).toHaveAttribute("data-waiting", "2")
  await expect(root).toHaveAttribute("data-state", "browsing")
  release()
  await expect(root).toHaveAttribute("data-engine", "ready", { timeout: ENGINE_MS })
  await expect(root).toHaveAttribute("data-state", "confirming")
  await expect(root).toHaveAttribute("data-waiting", "0")
  expect(cell.sent).toEqual([])
})

test("engine failing: the page says so with a person's Retry; what waited runs once Retry loads it", async ({ page }) => {
  let failing = true
  await page.route(ENGINE_WASM, (route) => (failing ? route.abort("failed") : route.continue()))
  const { root } = await browseWithoutEngine(page)
  const unavailable = page.locator('[data-unavailable="engine"]')
  await expect(root).toHaveAttribute("data-engine", "failed", { timeout: ENGINE_MS })
  await expect(unavailable).toBeVisible()
  await expect(unavailable).toContainText("Page engine unavailable")
  expect((await agentAct(page, "Note", "setValue", ["Checked twice"])).ok).toBe(true)
  expect((await agentAct(page, "Save", "press")).ok).toBe(true)
  await expect(root).toHaveAttribute("data-waiting", "2")
  await expect(unavailable).toContainText("2 steps wait for it")
  await expect(root).toHaveAttribute("data-state", "browsing")
  if (process.env.SHOTS) await page.screenshot({ path: join(process.env.SHOTS, "engine-unavailable-light-1280-ltr.png"), fullPage: true })
  // The chunk is back; a person presses Retry.
  failing = false
  await unavailable.getByRole("button", { name: "Retry" }).click()
  await expect(root).toHaveAttribute("data-engine", "ready", { timeout: ENGINE_MS })
  await expect(root).toHaveAttribute("data-state", "confirming")
  await expect(unavailable).toHaveCount(0)
})

test("workspace A → B: B's page reads and calls in B only, and shows nothing A read or called", async ({ page }) => {
  const environment = { current: "analytics" }
  const browsing = matrix.states.get("browsing")!
  const { root, cell } = await open(page, browsing, DEFAULT, "404", "ready", environment)
  await expect.poll(() => cell.reads.inFlight).toBe(true)
  cell.reads.release()
  await expect(root.getByText("Moving average")).toBeVisible()
  // A's save commits: the status island draws it.
  expect((await agentAct(page, "Save", "press")).ok).toBe(true)
  cell.calls.push(answerFor({ kind: "outcome", entry: 0, outcome: "accepted" }, "action"))
  await page.getByRole("button", { name: "Confirm", exact: true }).click()
  await expect.poll(() => cell.calls.inFlight).toBe(true)
  cell.calls.release()
  await expect(root).toHaveAttribute("data-state", "saved")
  const status = page.locator('[data-island="fixture-status"] p')
  await expect(status).toHaveAttribute("data-island-call", "committed")
  // The session moves to B (the server's answer; the tab notices on focus). B's cell holds other records.
  cell.reads.push({ status: 200, body: RECORDS_ELSEWHERE })
  environment.current = "ops"
  await page.evaluate(() => {
    document.dispatchEvent(new Event("visibilitychange", { bubbles: true }))
    window.dispatchEvent(new Event("focus"))
  })
  await expect.poll(() => cell.reads.urls.length).toBe(2)
  expect(new URL(cell.reads.urls[1]!).searchParams.get("environmentId")).toBe("ops")
  // B's page opened on its own: nothing A read or called is drawn while B's read is pending.
  await expect(root).toHaveAttribute("data-state", "loading")
  await expect(root.getByText("Moving average")).toHaveCount(0)
  await expect(status).toHaveAttribute("data-island-call", "none")
  cell.reads.release()
  await expect(root).toHaveAttribute("data-state", "browsing")
  await expect(root.getByText("Ops policy")).toBeVisible()
  await expect(root.getByText("Moving average")).toHaveCount(0)
  // B's save goes to B.
  expect((await agentAct(page, "Save", "press")).ok).toBe(true)
  cell.calls.push(answerFor({ kind: "outcome", entry: 0, outcome: "accepted" }, "action"))
  await page.getByRole("button", { name: "Confirm", exact: true }).click()
  await expect.poll(() => cell.sent.length).toBe(2)
  expect(cell.sent.map((call) => new URL(call.url).searchParams.get("environmentId"))).toEqual(["analytics", "ops"])
  expect(new URL(cell.reads.urls[0]!).searchParams.get("environmentId")).toBe("analytics")
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
      // SHOTS=<dir>: keep each cell's screenshot for review (the lane's browser proof), never in the repo.
      if (process.env.SHOTS) await page.screenshot({ path: join(process.env.SHOTS, `${state}-${variant.theme}-${variant.width}-${variant.dir}.png`), fullPage: true })
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
