import { expect, test, type Page } from "@playwright/test"
import { node, open, shown } from "./canvas.ts"

/**
 * One test per row of the automation verb matrix for saving and running (D66; see the PR body). Locally a
 * stand-in answers the definitions routes for kind "flow" (DEFS #1078) and the run routes (RUNS #1047), checking
 * scope and the request shapes; against a deployment these rows need the cell's own data and are skipped.
 */
interface Kept { version: number; payload: { flow: { id: string; title?: string }; artifacts: unknown[]; layout?: { nodes: { id: string; position: { x: number; y: number } }[] } }; retired: boolean }

/** The stand-in cell's words for a flow (its restater): the title and each labelled step. */
const wordsFor = (flow: { title?: string; id: string; steps?: { label: string }[] }, suffix = "") => `${flow.title ?? flow.id}: ${(flow.steps ?? []).map(({ label }) => label).join(", ")}${suffix}`
const sha = async (text: string) => Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text))), (b) => b.toString(16).padStart(2, "0")).join("")

async function cell(page: Page, options: { refuseOnce?: string; runsLive?: boolean; mismatchOnce?: boolean } = {}) {
  test.skip(Boolean(process.env.E2E_BASE), "these rows use a deterministic stand-in cell; the deployed suite runs against next")
  const state = { flows: new Map<string, Kept & { signoff?: string }>(), installs: [] as Record<string, unknown>[], runs: new Map<string, { id: string; flow: string }>(), refuseOnce: options.refuseOnce, runsLive: options.runsLive ?? false, mismatchOnce: options.mismatchOnce ?? false, suffix: "" }
  await page.route("**/api/host/v1/**", async (route) => {
    const url = new URL(route.request().url())
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") return route.fulfill({ status: 403, json: { code: "authz.denied" } })
    const body = route.request().postDataJSON() as { kind?: string; id?: string; payload?: Kept["payload"]; expectedVersion?: number; flow?: string; runId?: string; signoff?: unknown } | null
    const path = url.pathname.replace(/^.*\/v1\//, "")
    if (path === "definitions:admit") {
      expect(body?.kind).toBe("flow")
      if (state.refuseOnce) {
        const step = state.refuseOnce
        state.refuseOnce = undefined
        return route.fulfill({ json: { admitted: false, diagnostics: [{ code: "definition.flow_function_unknown", severity: "error", message: `step ${step} calls billing.notify@1, which is not an installed binding`, span: { pointer: "/artifacts/1/payload" } }] } })
      }
      const text = wordsFor(body!.payload!.flow as never, state.suffix)
      return route.fulfill({ json: { admitted: true, diagnostics: [], restatement: text, restatementSha256: await sha(text) } })
    }
    if (path === "definitions:install" && body?.id && body.payload) {
      state.installs.push(body as Record<string, unknown>)
      // Signed installs only, of exactly the cell's words; the first time (if asked) the cell's declarations moved on.
      if (state.mismatchOnce) { state.mismatchOnce = false; state.suffix = " (as the cell reads it now)" }
      const words = wordsFor(body.payload.flow as never, state.suffix)
      if ((body.signoff as { restatementSha256?: string } | undefined)?.restatementSha256 !== await sha(words)) return route.fulfill({ status: 422, json: { code: "definition.signoff_mismatch", restatement: words, restatementSha256: await sha(words) } })
      const kept = state.flows.get(body.id)
      if ((kept && !kept.retired ? kept.version : undefined) !== body.expectedVersion) return route.fulfill({ status: 409, json: { code: "definition.version_conflict" } })
      const version = (kept?.version ?? 0) + 1
      state.flows.set(body.id, { version, payload: body.payload, retired: false, signoff: await sha(words) })
      return route.fulfill({ json: { installed: true, id: body.id, version, receipt: { receiptHash: `receipt${version}000000`, commitSeq: version, recordedAtMs: 0 }, flow: { flowId: body.id, version: "0.1.0" } } })
    }
    const retire = /^definitions\/(.+):retire$/.exec(path)
    if (retire?.[1]) {
      if (state.runsLive) { state.runsLive = false; return route.fulfill({ status: 409, json: { code: "definition.flow_runs_live" } }) }
      const kept = state.flows.get(decodeURIComponent(retire[1]))!
      kept.version += 1; kept.retired = true
      return route.fulfill({ json: { installed: true, id: retire[1], version: kept.version, receipt: { receiptHash: "retired000000", commitSeq: kept.version, recordedAtMs: 0 } } })
    }
    if (path === "definitions:list") {
      expect(url.searchParams.get("kind")).toBe("flow")
      return route.fulfill({ json: { items: [...state.flows].filter(([, kept]) => !kept.retired).map(([id, kept]) => ({ id, kind: "flow", title: (kept.payload.flow as { title?: string }).title ?? "", version: kept.version, lifecycle: "active" })), cursor: null } })
    }
    const get = /^definitions\/([^:]+)$/.exec(path)
    if (get?.[1]) {
      const kept = state.flows.get(decodeURIComponent(get[1]))
      if (!kept || kept.retired) return route.fulfill({ status: 404, json: { code: "definition.not_found" } })
      const now = wordsFor(kept.payload.flow as never, state.suffix)
      return route.fulfill({ json: { id: get[1], kind: "flow", version: kept.version, lifecycle: "active", payload: kept.payload, signoff: kept.signoff ? { signer: "u_e2e", signedAtMs: 1_700_000_000_000, restatementSha256: kept.signoff } : null, restatement: now, restatementSha256: await sha(now) } })
    }
    if (path === "runs" && body?.flow && body.runId) {
      if (!state.flows.get(body.flow) || state.flows.get(body.flow)!.retired) return route.fulfill({ status: 404, json: { code: "run.flow_not_found" } })
      state.runs.set(body.runId, { id: body.runId, flow: body.flow })
      return route.fulfill({ json: { id: body.runId, generation: 1, flow: { flowId: body.flow }, objectType: "billing.RouteRun", state: "awaiting_perform_load", version: 1, createdAtMs: 0, createdCommitSeq: 1, updatedAtMs: 0, updatedCommitSeq: 1 } })
    }
    const read = /^runs\/(.+)$/.exec(path)
    if (read?.[1]) {
      const run = state.runs.get(decodeURIComponent(read[1]))
      return run ? route.fulfill({ json: { id: run.id, flow: { flowId: run.flow }, state: "awaiting_perform_load" } }) : route.fulfill({ status: 404, json: { code: "run.not_found" } })
    }
    return route.fulfill({ status: 404, json: {} })
  })
  return state
}

const saveButton = (page: Page) => page.getByRole("button", { name: /Save to server/ })
/** Save as a person does now: Save shows the cell's words, sign them off, Save installs signed. */
async function saveSigned(page: Page) {
  await saveButton(page).click()
  await expect(page.getByTestId("automation-restatement")).toBeVisible()
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await saveButton(page).click()
}
const actions = (page: Page) => page.getByTestId("automation-actions")

test("automation: create, save to the cell, reopen as drawn, edit and save the next version", async ({ page }) => {
  const state = await cell(page)
  await open(page, "Automation")
  await expect(actions(page)).toContainText("Not saved in the cell")
  await expect(page.getByRole("button", { name: /Run in the cell/ })).toBeDisabled()
  await saveSigned(page)
  await expect(actions(page)).toContainText("Saved in the cell · version 1")
  await expect(page.getByText(/Saved as version 1; the cell admitted and installed it/)).toBeVisible()
  // The install carried the flow (id, version, title, labelled steps), its two artifacts and the drawing.
  const first = state.installs[0] as { id: string; payload: { flow: { id: string; title?: string; steps: { id: string; label: string }[] }; artifacts: unknown[]; layout: { nodes: unknown[] } } }
  expect(first.id).toBe("studio.automation.invoice_follow_up")
  expect(first.payload.flow).toMatchObject({ id: "studio.automation.invoice_follow_up", title: "Invoice follow-up" })
  expect(first.payload.flow.steps.map(({ id }) => id)).toContain("load")
  expect(first.payload.artifacts.length).toBe(2)
  expect(first.payload.layout.nodes.length).toBeGreaterThan(0)
  // edit: move a step and save again; the next version names the one it replaces.
  const step = await shown(page, "open")
  const box = (await step.boundingBox())!
  await step.hover()
  await page.mouse.down(); await page.mouse.move(box.x + box.width / 2 + 60, box.y + box.height / 2 + 40, { steps: 8 }); await page.mouse.up()
  await saveSigned(page)
  await expect(actions(page)).toContainText("version 2")
  expect(state.installs.at(-1)).toMatchObject({ expectedVersion: 1 })
  // reopen: the cell's drawing comes back where it was left.
  const moved = (state.installs.at(-1) as { payload: { layout: { nodes: { id: string; position: { x: number } }[] } } }).payload.layout.nodes.find(({ id }) => id === "open")!.position.x
  await page.reload()
  await page.getByRole("button", { name: "Automation", exact: true }).click()
  await expect(actions(page)).toContainText("Saved in the cell · version 2")
  await expect(node(page, "open")).toHaveAttribute("style", new RegExp(`translate\\(${Math.round(moved)}(\\.\\d+)?px`))
})

test("automation: the cell's refusal lands on the step it names, and Save is offered again after an edit", async ({ page }) => {
  await cell(page, { refuseOnce: "remind" })
  await open(page, "Automation")
  await saveButton(page).click()
  await expect(page.getByText(/The cell refused the automation: definition.flow_function_unknown/)).toBeVisible()
  await expect(node(page, "remind")).toContainText("1 problem")
  await expect(page.getByTestId("problem").filter({ hasText: "definition.flow_function_unknown" })).toContainText("On remind")
  // Save waits for the finding to clear: an edit makes the refusal stale.
  await expect(saveButton(page)).toHaveAccessibleName(/Fix 1 problem first/)
  const step = await shown(page, "remind")
  const box = (await step.boundingBox())!
  await step.hover()
  await page.mouse.down(); await page.mouse.move(box.x + box.width / 2 + 30, box.y + box.height / 2, { steps: 6 }); await page.mouse.up()
  await saveSigned(page)
  await expect(actions(page)).toContainText("Saved in the cell · version 1")
})

test("automation: Run starts the saved version in the cell and opens its run", async ({ page }) => {
  const state = await cell(page)
  await open(page, "Automation")
  await saveSigned(page)
  await expect(actions(page)).toContainText("version 1")
  await page.getByRole("button", { name: /Run in the cell/ }).click()
  await expect(page).toHaveURL(/\/runs\/run_[0-9a-z]+_[0-9a-z]{6}$/)
  await expect(page.getByRole("heading", { name: /^Run run_/ })).toBeVisible()
  await expect(page.getByTestId("run-state")).toHaveText("awaiting_perform_load")
  expect([...state.runs.values()][0]?.flow).toBe("studio.automation.invoice_follow_up")
})

test("automation: retire is refused while runs are live, then retires; Run is no longer offered", async ({ page }) => {
  await cell(page, { runsLive: true })
  await open(page, "Automation")
  await saveSigned(page)
  await expect(actions(page)).toContainText("version 1")
  await actions(page).getByRole("button", { name: "Retire" }).click()
  await expect(actions(page)).toContainText("this automation has runs still going")
  await actions(page).getByRole("button", { name: "Retire" }).click()
  await expect(actions(page)).toContainText("Retired.")
  await expect(actions(page)).toContainText("Not saved in the cell")
  await expect(page.getByRole("button", { name: /Run in the cell/ })).toBeDisabled()
})

test("automations: two saved side by side under their own ids, and each reopens as itself", async ({ page }) => {
  const state = await cell(page)
  await open(page, "Automation")
  await saveSigned(page)
  await expect(actions(page)).toContainText("Saved in the cell · version 1")
  // A second automation, by name: a new id minted from it, starting from its start and one outcome.
  await page.getByLabel("New automation name").fill("Payment reminders")
  await page.getByRole("button", { name: "New automation" }).click()
  await expect(node(page, "done")).toBeVisible()
  await expect(actions(page)).toContainText("Not saved in the cell")
  await saveSigned(page)
  await expect(actions(page)).toContainText("Saved in the cell · version 1")
  expect([...state.flows.keys()].sort()).toEqual(["studio.automation.invoice_follow_up", "studio.automation.payment_reminders"])
  // Both are listed; each reopens as itself (the first with its example steps, the second with its own).
  const list = page.getByTestId("automations")
  await expect(list.getByRole("button", { name: /^Invoice follow-up/ })).toBeVisible()
  await expect(list.getByRole("button", { name: /^Payment reminders/ })).toBeVisible()
  await list.getByRole("button", { name: /^Invoice follow-up/ }).click()
  await expect(node(page, "load")).toBeVisible()
  await expect(actions(page)).toContainText("version 1")
  await list.getByRole("button", { name: /^Payment reminders/ }).click()
  await expect(node(page, "done")).toBeVisible()
  await expect(node(page, "load")).toHaveCount(0)
  await page.reload()
  await page.getByRole("button", { name: "Automation", exact: true }).click()
  await page.getByTestId("automations").getByRole("button", { name: /^Payment reminders/ }).click()
  await expect(node(page, "done")).toBeVisible()
  await expect(actions(page)).toContainText("Saved in the cell · version 1")
})

test("sign-off: Save shows the cell's words; when the cell reads them differently it shows its own; re-sign and save; reopen shows who signed", async ({ page }) => {
  const state = await cell(page, { mismatchOnce: true })
  await open(page, "Automation")
  await saveButton(page).click()
  await expect(page.getByTestId("automation-restatement")).toHaveText(/^Invoice follow-up: Load/)
  await expect(actions(page)).toContainText("Not saved in the cell")
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await saveButton(page).click()
  // The cell's declarations moved on: its own words come back, nothing was kept, and the sign-off is cleared.
  await expect(page.getByText("The cell reads this automation differently. Review its words and sign off again.")).toBeVisible()
  await expect(page.getByText("The cell reads it as:")).toBeVisible()
  await expect(page.getByTestId("automation-restatement")).toContainText("(as the cell reads it now)")
  expect(state.flows.size).toBe(0)
  await page.getByRole("button", { name: "This is what I asked for" }).click()
  await saveButton(page).click()
  await expect(actions(page)).toContainText("Saved in the cell · version 1")
  // Reopen: who signed, from the cell.
  await page.reload()
  await page.getByRole("button", { name: "Automation", exact: true }).click()
  await expect(page.getByTestId("automation-signoff")).toContainText("Version 1 signed off by u_e2e")
})
