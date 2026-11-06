import { expect, test, type Page } from "@playwright/test"
import { node, open, press } from "./canvas.ts"

/**
 * Locally there is no cell: an in-memory stand-in answers the definition routes (CELL-DO's contract) and
 * CONNECT's bindings, refusing any agent under the law `demo.refuse_me` the way the cell names a law.
 * Against a deployment (E2E_BASE) the real routes answer.
 */
async function cell(page: Page) {
  if (process.env.E2E_BASE) return
  const saved = new Map<string, { version: number; payload: unknown; retired: boolean }>()
  await page.route("**/api/connectors/bindings", (route) => route.fulfill({ json: { manage: true, bindings: [
    { id: "bind_tickets", app: "helpdesk", tool: "list_tickets", label: "Read tickets", inputMapping: {}, runRole: "member", updatedBy: "u", updatedAt: 0 },
    { id: "bind_chat", app: "chat", tool: "post_message", label: "Post to #support", inputMapping: {}, runRole: "member", updatedBy: "u", updatedAt: 0 },
  ] } }))
  await page.route("**/api/host/v1/definitions**", async (route) => {
    const url = new URL(route.request().url())
    const body = route.request().postDataJSON() as { id?: string; payload?: { laws?: string[] }; expectedVersion?: number } | null
    if (url.pathname.endsWith("definitions:list")) return route.fulfill({ json: { items: [...saved].filter(([, one]) => !one.retired).map(([id, one]) => ({ id, title: (one.payload as { title: string }).title, version: one.version, lifecycle: "active" })), cursor: null } })
    if (url.pathname.endsWith("definitions:install") && body?.id) {
      if (body.payload?.laws?.includes("demo.refuse_me")) return route.fulfill({ status: 422, json: { code: "definition.refused", diagnostics: [{ code: "law.refused", message: "agents may not run under this law", law: "demo.refuse_me", span: { pointer: "/laws/0" } }] } })
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
  await open(page, "Agents")
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
  // wire a tool (create edge), then undo/redo it
  await page.getByRole("button", { name: "Tool", exact: true }).click()
  await expect(node(page, "tool:bind_tickets")).toContainText("Read tickets")
  await press(page, "z")
  await expect(node(page, "tool:bind_tickets")).toHaveCount(0)
  await press(page, "Shift+z")
  await expect(node(page, "tool:bind_tickets")).toContainText("helpdesk/list_tickets")
  // save: the cell installs it
  await page.getByRole("button", { name: "Save to server" }).click()
  await expect(page.getByText(/Saved as version 1/)).toBeVisible()
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
  await expect(page.getByText(/Saved as version 1/)).toBeVisible()
  await page.getByRole("button", { name: "Retire studio.agent.support_triage_copy" }).click()
  await expect(page.getByText("Retired studio.agent.support_triage_copy.")).toBeVisible()
})

test("agent refusal: the cell's refusal names the law and lands on the law node", async ({ page }) => {
  await cell(page)
  await open(page, "Agents")
  await page.getByLabel("New agent name").fill("Refused")
  await page.getByRole("button", { name: "New agent" }).click()
  await page.getByRole("button", { name: "Law", exact: true }).click()
  // The admitted laws come from the definitions; point the new law node at the one the stand-in refuses.
  await page.getByLabel("Law").evaluate((select: HTMLSelectElement) => {
    const option = document.createElement("option"); option.value = "demo.refuse_me"; option.text = "demo.refuse_me"; select.add(option)
  })
  await page.getByLabel("Law").selectOption("demo.refuse_me")
  await page.getByRole("button", { name: "Save to server" }).click()
  await expect(page.getByText(/refused to install it: demo.refuse_me/)).toBeVisible()
  await expect(page.getByText(/agents may not run under this law \(law demo.refuse_me\)/).first()).toBeVisible()
  await expect(page.locator('.react-flow__node[data-id^="law:"]')).toContainText("problem")
})
