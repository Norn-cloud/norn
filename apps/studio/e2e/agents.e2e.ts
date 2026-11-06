import { expect, test, type Page } from "@playwright/test"
import { node, open, press } from "./canvas.ts"

/**
 * Locally there is no cell: an in-memory stand-in answers the definition routes (CELL-DO's contract) and
 * CONNECT's bindings, refusing any agent under the law `core.analytics.restatement_policy` the way the cell names a law.
 * Against a deployment (E2E_BASE) the real routes answer.
 */
/** Whether this deployment's definition routes answer; the stand-in tests skip, with the reason, where they do not. */
async function routesLive(page: Page): Promise<boolean> {
  const response = await page.request.get("/api/host/v1/definitions:list?kind=agent&limit=1")
  return response.status() !== 404
}

async function cell(page: Page) {
  if (process.env.E2E_BASE) {
    test.skip(!(await routesLive(page)), "this deployment's definition routes are not live yet (lane DEFS); the routes-down test covers it")
    return
  }
  const saved = new Map<string, { version: number; payload: unknown; retired: boolean }>()
  await page.route("**/api/connectors/bindings", (route) => route.fulfill({ json: { manage: true, bindings: [
    { id: "bind_tickets", app: "helpdesk", tool: "list_tickets", label: "Read tickets", inputMapping: {}, runRole: "member", updatedBy: "u", updatedAt: 0, manifestDigest: `sha256:${"a".repeat(64)}`, connector: { gateway: "executor", origin: "https://gateway.example", app: "helpdesk", tool: "list_tickets", version: "v1" } },
    { id: "bind_chat", app: "chat", tool: "post_message", label: "Post to #support", inputMapping: {}, runRole: "member", updatedBy: "u", updatedAt: 0, manifestDigest: `sha256:${"b".repeat(64)}`, connector: { gateway: "executor", origin: "https://gateway.example", app: "chat", tool: "post_message", version: "v1" } },
    { id: "bind_legacy", app: "chat", tool: "old_post", label: "Saved before pinning", inputMapping: {}, runRole: "member", updatedBy: "u", updatedAt: 0 },
  ] } }))
  await page.route("**/api/host/v1/definitions**", async (route) => {
    const url = new URL(route.request().url())
    const body = route.request().postDataJSON() as { id?: string; payload?: { laws?: string[] }; expectedVersion?: number } | null
    if (url.pathname.endsWith("definitions:list")) return route.fulfill({ json: { items: [...saved].filter(([, one]) => !one.retired).map(([id, one]) => ({ id, title: (one.payload as { title: string }).title, version: one.version, lifecycle: "active" })), cursor: null } })
    if (url.pathname.endsWith("definitions:install") && body?.id) {
      if (body.payload?.laws?.includes("core.analytics.restatement_policy")) return route.fulfill({ status: 422, json: { code: "definition.refused", diagnostics: [{ code: "law.refused", message: "agents may not run under this law", law: "core.analytics.restatement_policy", span: { pointer: "/laws/0" } }] } })
      const version = (saved.get(body.id)?.version ?? 0) + 1
      saved.set(body.id, { version, payload: body.payload, retired: false })
      return route.fulfill({ json: { installed: true, id: body.id, version, receipt: { receiptHash: "e2e0000000000000", commitSeq: version, recordedAtMs: 0 } } })
    }
    const retire = /definitions\/(.+):retire$/.exec(url.pathname)
    if (retire?.[1]) { const id = decodeURIComponent(retire[1]); const one = saved.get(id); if (one) one.retired = true; return route.fulfill({ json: { installed: true, id, version: (one?.version ?? 0) + 1, receipt: { receiptHash: "e2e1", commitSeq: 0, recordedAtMs: 0 } } }) }
    const get = /definitions\/([^:]+)$/.exec(url.pathname)
    if (get?.[1]) { const id = decodeURIComponent(get[1]); const one = saved.get(id); return one ? route.fulfill({ json: { id, kind: "agent", version: one.version, payload: one.payload } }) : route.fulfill({ status: 404, json: { code: "definition.not_found" } }) }
    return route.fulfill({ status: 404, json: {} })
  })
}

test("agent: create, view, edit, wire tools and laws, undo/redo, validate (law named), save, reopen, duplicate, retire", async ({ page }) => {
  await cell(page)
  await open(page)
  await page.getByRole("button", { name: "Agents", exact: true }).click()
  await expect(page.getByText("No agents yet")).toBeVisible()
  // create
  await page.getByLabel("New agent name").fill("Support triage")
  await page.getByRole("button", { name: "New agent" }).click()
  await expect(node(page, "agent")).toBeVisible()
  // edit
  await node(page, "agent").click()
  await page.getByLabel("Title").fill("Support triage")
  await page.getByLabel("Title").blur()
  await page.getByLabel("Prompt").fill("Triage new tickets.")
  await page.getByLabel("Prompt").blur()
  await page.getByLabel("A person decides").selectOption("laws")
  await expect(node(page, "agent")).toContainText("asks when a law applies")
  // "When a marked law applies" with no law marked is the checker's decision-policy finding, and Save waits for it.
  await expect(page.getByText("agent.validation.invalid_decision_policy").first()).toBeVisible()
  await expect(page.getByRole("button", { name: /Save to server/ })).toHaveAccessibleName(/Fix \d+ problems? first/)
  await page.getByLabel("A person decides").selectOption("always")
  // wire a tool (create edge), then undo/redo it
  await page.getByRole("button", { name: "Tool", exact: true }).click()
  await expect(node(page, "tool:bind_tickets")).toContainText("Read tickets")
  await press(page, "z")
  await expect(node(page, "tool:bind_tickets")).toHaveCount(0)
  await press(page, "Shift+z")
  await expect(node(page, "tool:bind_tickets")).toContainText("helpdesk/list_tickets")
  // save: the cell installs it
  await page.getByRole("button", { name: "Save to server" }).click()
  await expect(page.getByText(/Saved as version 1/).first()).toBeVisible()
  await expect(page.getByRole("button", { name: "Run the agent" })).toBeDisabled()
  // reopen
  await page.reload()
  await page.getByRole("button", { name: "Agents", exact: true }).click()
  await page.getByRole("button", { name: "Support triage" }).click()
  await node(page, "agent").click()
  await expect(page.getByLabel("Prompt")).toHaveValue("Triage new tickets.")
  await expect(node(page, "tool:bind_tickets")).toBeVisible()
  // duplicate, then retire the copy
  await page.getByRole("button", { name: "Duplicate studio.agent.support_triage" }).click()
  await expect(page.getByText("Agent: studio.agent.support_triage_copy")).toBeVisible()
  await page.getByRole("button", { name: "Save to server" }).click()
  await expect(page.getByText(/Saved as version 1/).first()).toBeVisible()
  await page.getByRole("button", { name: "Retire studio.agent.support_triage_copy" }).click()
  await expect(page.getByText("Retired studio.agent.support_triage_copy.")).toBeVisible()
})

test("agent refusal: the cell's refusal names the law on the law node; Save waits for an edit, then is offered again", async ({ page }) => {
  await cell(page)
  await open(page)
  await page.getByRole("button", { name: "Agents", exact: true }).click()
  await page.getByLabel("New agent name").fill("Refused")
  await page.getByRole("button", { name: "New agent" }).click()
  await node(page, "agent").click()
  await page.getByLabel("Title").fill("Refused")
  await page.getByLabel("Title").blur()
  await page.getByLabel("Prompt").fill("Act under the restatement policy.")
  await page.getByLabel("Prompt").blur()
  // An admitted law, which the stand-in cell refuses agents to run under.
  await page.getByRole("button", { name: "Law", exact: true }).click()
  await expect(page.getByText("Checker: admitted")).toBeVisible()
  await page.getByRole("button", { name: "Save to server" }).click()
  await expect(page.getByText(/refused to install it: core.analytics.restatement_policy/)).toBeVisible()
  await expect(page.getByText(/agents may not run under this law \(law core.analytics.restatement_policy\)/).first()).toBeVisible()
  await expect(page.locator('.react-flow__node[data-id^="law:"]')).toContainText("problem")
  // The refused drawing cannot be sent again as it is.
  await expect(page.getByRole("button", { name: /Save to server/ })).toBeDisabled()
  await expect(page.getByRole("button", { name: /Save to server/ })).toHaveAccessibleName(/Fix 1 problem first/)
  // Any edit makes the refusal stale: Save is offered again, and the cell decides again.
  await node(page, "agent").click()
  await page.getByLabel("Title").fill("Refused, revised")
  await page.getByLabel("Title").blur()
  await expect(page.getByRole("button", { name: "Save to server" })).toBeEnabled()
})

test("agent, before the cell's definition routes are live: the panel says so, saving to the cell is unavailable, the draft stays", async ({ page }) => {
  if (process.env.E2E_BASE) test.skip(await routesLive(page), "this deployment's definition routes are live")
  else {
    await page.route("**/api/connectors/bindings", (route) => route.fulfill({ json: { manage: true, bindings: [] } }))
    await page.route("**/api/host/v1/definitions**", (route) => route.fulfill({ status: 404, json: {} }))
  }
  await open(page)
  await page.getByRole("button", { name: "Agents", exact: true }).click()
  await expect(page.getByText(/Saving agents needs the cell's definition routes/).first()).toBeVisible()
  await page.getByLabel("New agent name").fill("Draft only")
  await page.getByRole("button", { name: "New agent" }).click()
  await expect(page.getByRole("button", { name: /Save to server/ })).toBeDisabled()
  await expect(page.getByRole("button", { name: /Run the agent/ })).toBeDisabled()
  await node(page, "agent").click()
  await page.getByLabel("Title").fill("Kept as a draft")
  await page.getByLabel("Title").blur()
  await page.reload()
  await page.getByRole("button", { name: "Agents", exact: true }).click()
  await page.getByLabel("New agent name").fill("Draft only")
  await page.getByRole("button", { name: "New agent" }).click()
  await expect(node(page, "agent")).toContainText("Kept as a draft")
})

test("agent live check: the browser runs the checker's agent admission on every edit, with pinned bindings", async ({ page }) => {
  await cell(page)
  await open(page)
  await page.getByRole("button", { name: "Agents", exact: true }).click()
  await page.getByLabel("New agent name").fill("Live check")
  await page.getByRole("button", { name: "New agent" }).click()
  // A new agent has no title or prompt yet: the checker says so on the agent, alongside everything else,
  // and Save to server waits until they are fixed.
  await expect(page.getByText("title must not be blank").first()).toBeVisible()
  await expect(page.getByText("prompt must not be blank").first()).toBeVisible()
  await expect(page.getByRole("button", { name: /Save to server/ })).toHaveAccessibleName(/Fix 2 problems first/)
  await node(page, "agent").click()
  await page.getByLabel("Title").fill("Live check")
  await page.getByLabel("Title").blur()
  await page.getByLabel("Prompt").fill("Check things.")
  await page.getByLabel("Prompt").blur()
  // A pinned binding is bound: no finding.
  await page.getByRole("button", { name: "Tool", exact: true }).click()
  await expect(page.getByText("Checker: admitted")).toBeVisible()
  // A binding saved before pinning is named as such, on its node.
  await page.getByRole("combobox", { name: "Tool binding" }).selectOption("bind_legacy")
  await expect(page.getByText("canvas.agent.unpinned_binding").first()).toBeVisible()
  // A budget that is not a whole number is the checker's budget finding.
  await node(page, "agent").click()
  await page.getByLabel("Most runs").fill("0")
  await page.getByLabel("Most runs").blur()
  await expect(page.getByText("agent.validation.budget_unbounded").first()).toBeVisible()
})
