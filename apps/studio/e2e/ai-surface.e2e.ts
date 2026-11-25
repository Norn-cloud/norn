import { readFileSync } from "node:fs"
import AxeBuilder from "@axe-core/playwright"
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * The AI surface (DESIGN.md 19): the Ask Norn bar, Sidekick's AI Elements composer over useChat, the
 * approval prompt the bridge asks through, and the mascot. A stand-in bridge speaks ACP on the page's
 * own origin like apps/sidekick-bridge does; each row checks what Studio draws and what it sends back.
 * With AI_SHOTS=<dir> the last rows also save the review screenshots (light/dark, desktop/390).
 */

const ANSWER = readFileSync(new URL("../../../fixtures/demo/answers/scrap-yield.k1.answer.json", import.meta.url), "utf8")
const GRANT = "e2e.sidekick.grant"
const shots = process.env.AI_SHOTS
/** The WCAG 2.2 A and AA rules axe checks (its best-practice rules are page-level and left out). */
const WCAG = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]
const RECEIPT = JSON.stringify({ outcome: "committed", objectType: "demo.Record", objectId: "R-1", version: 2, receipt: { receiptHash: "sha256:5f0c3a9be12d4471a0c6e3f2b8d94e7c1a2b3c4d5e6f708192a3b4c5d6e7f809", commitSeq: 41, recordedAtMs: 1764000000000 } })
const REFUSAL = { code: "law.refused", message: "Closing R-1 needs a review first.", explanation: "The workspace's law asks for a reviewer before a record closes.", reason: { law: "demo.close_after_review", values: { reviewed: "false" } } }

type Sent = { id?: number | string; method?: string; params?: Record<string, unknown>; result?: unknown }

async function studio(page: Page, grant: "ok" | "refused" = "ok") {
  await session(page)
  const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 10000, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  await page.route("**/sidekick/config.json", (route) => route.fulfill({ json: { sidekick: 1, socket: "/sidekick/ws" } }))
  const state = { grant }
  await page.route("**/api/sidekick/grant", (route) => state.grant === "ok"
    ? route.fulfill({ json: { token: GRANT, expiresAt: Date.now() + 600_000 } })
    : route.fulfill({ status: 403, json: { error: "Your role in this workspace does not include Sidekick." } }))
  return state
}

/**
 * The stand-in bridge. `turn` plays one prompt: it gets helpers to send updates, views, and a
 * permission question (resolving with the page's answer), and returns the stop reason.
 */
async function bridge(page: Page, turn: (io: {
  readonly update: (value: unknown) => void
  readonly view: (tool: string, view: string, result: unknown) => void
  readonly ask: (tool: string, input: unknown) => Promise<unknown>
  readonly cancelled: Promise<void>
}) => Promise<string>) {
  const sent: Sent[] = []
  await page.routeWebSocket("**/sidekick/ws", (ws: WebSocketRoute) => {
    const send = (message: unknown) => ws.send(JSON.stringify(message))
    const answers = new Map<string, (result: unknown) => void>()
    let cancel: () => void = () => undefined
    let asks = 0
    ws.onMessage((raw) => {
      const message = JSON.parse(String(raw)) as Sent
      sent.push(message)
      if (message.method === undefined && typeof message.id === "string") return answers.get(message.id)?.(message.result)
      const reply = (result: unknown) => send({ jsonrpc: "2.0", id: message.id, result })
      if (message.method === "initialize") return reply({ protocolVersion: 1, agentCapabilities: { loadSession: false, promptCapabilities: {}, mcpCapabilities: {} }, authMethods: [] })
      if (message.method === "session/new") return reply({ sessionId: "s1" })
      if (message.method === "session/cancel") return cancel()
      if (message.method !== "session/prompt") return
      const cancelled = new Promise<void>((resolve) => { cancel = resolve })
      void turn({
        update: (value) => send({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: value } }),
        view: (tool, view, result) => send({ jsonrpc: "2.0", method: "_norn/tool_result", params: { tool, view, input: {}, result } }),
        ask: (tool, input) => new Promise((resolve) => {
          const id = `p${++asks}`
          answers.set(id, resolve)
          send({ jsonrpc: "2.0", id, method: "session/request_permission", params: { sessionId: "s1", toolCall: { toolCallId: "t2", title: `mcp__norn__${tool}`, rawInput: input, _meta: { claudeCode: { toolName: `mcp__norn__${tool}` } } }, options: [{ optionId: "allow", name: "Allow", kind: "allow_once" }, { optionId: "reject", name: "Reject", kind: "reject_once" }] } })
        }),
        cancelled,
      }).then(reply)
    })
  })
  return sent
}

const tool = (id: string, name: string, extra: Record<string, unknown> = {}) => ({ sessionUpdate: "tool_call", toolCallId: id, title: `mcp__norn__${name}`, kind: "other", status: "pending", _meta: { claudeCode: { toolName: `mcp__norn__${name}` } }, ...extra })
const words = (text: string) => ({ sessionUpdate: "agent_message_chunk", content: { type: "text", text } })

/** Open Sidekick from the rail (its mascot button) at a desktop width. */
async function openSidekick(page: Page) {
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto("/")
  await page.getByRole("button", { name: "Sidekick", exact: true }).first().click()
  const panel = page.locator("[data-slot=sidekick]")
  await expect(panel.getByText("Ask Sidekick")).toBeVisible()
  return panel
}

test("ask norn: the bar leads with the mascot, never a sparkle, and Enter hands the question to Sidekick", async ({ page }) => {
  await studio(page)
  const sent = await bridge(page, async ({ update }) => { update(words("Here is what needs you.")); return "end_turn" })
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto("/")
  const bar = page.locator("[data-slot=ask-norn]")
  await expect(bar.locator("[data-slot=mascot]")).toHaveCount(1)
  await expect(page.locator("svg.lucide-sparkles, svg.lucide-sparkle, svg.lucide-wand-sparkles")).toHaveCount(0)
  await expect(bar.getByRole("button", { name: "Ask" })).toBeDisabled()
  await page.getByLabel("Ask or tell Norn anything").fill("What needs me today?")
  await page.getByLabel("Ask or tell Norn anything").press("Enter")
  const panel = page.locator("[data-slot=sidekick]")
  await expect(panel.getByText("Here is what needs you.")).toBeVisible()
  await expect(page.getByLabel("Ask or tell Norn anything")).toHaveValue("")
  expect(sent.find((message) => message.method === "session/prompt")?.params?.prompt).toEqual([{ type: "text", text: "What needs me today?" }])
})

test("composer: Enter sends, Shift+Enter is a new line, the turn draws its tool and view, and the mascot answers", async ({ page }) => {
  await studio(page)
  const sent = await bridge(page, async ({ update, view }) => {
    update(tool("t1", "norn_answer"))
    view("norn_answer", "answer", { content: [{ type: "text", text: ANSWER }] })
    update({ sessionUpdate: "tool_call_update", toolCallId: "t1", status: "completed" })
    update(words("Here is weekly yield from Norn."))
    return "end_turn"
  })
  const panel = await openSidekick(page)
  const box = panel.getByLabel("Message Sidekick")
  await box.fill("Weekly yield")
  await box.press("Shift+Enter")
  await box.pressSequentially("please")
  await expect(box).toHaveValue("Weekly yield\nplease")
  await box.press("Enter")
  await expect(panel.locator("[data-role=user]")).toContainText("Weekly yield")
  await expect(panel.locator("[data-tool=norn_answer][data-status=completed]")).toContainText("Answered the question")
  await expect(panel.locator("[data-view=answer]").getByRole("table")).toBeVisible()
  await expect(panel.getByText("Here is weekly yield from Norn.")).toBeVisible()
  await expect(panel.locator("[data-role=assistant] [data-slot=mascot]").first()).toHaveAttribute("data-state", "answered")
  await expect(panel.getByRole("button", { name: "Send" })).toBeVisible()
  expect(sent.filter((message) => message.method === "session/prompt").map((message) => message.params?.prompt)).toEqual([[{ type: "text", text: "Weekly yield\nplease" }]])
  expect(JSON.stringify(sent.slice(1))).not.toContain(GRANT)
})

test("composer: Stop cancels the turn at the bridge and the composer is ready again", async ({ page }) => {
  await studio(page)
  const sent = await bridge(page, async ({ update, cancelled }) => {
    update(words("Reading the inbox"))
    await cancelled
    return "cancelled"
  })
  const panel = await openSidekick(page)
  await panel.getByLabel("Message Sidekick").fill("What needs my attention?")
  await panel.getByLabel("Message Sidekick").press("Enter")
  await expect(panel.getByText("Reading the inbox")).toBeVisible()
  await expect(panel.locator("[data-role=assistant] [data-slot=mascot]").first()).toHaveAttribute("data-state", "thinking")
  await panel.getByRole("button", { name: "Stop" }).click()
  await expect.poll(() => sent.some((message) => message.method === "session/cancel")).toBe(true)
  await expect(panel.getByRole("button", { name: "Send" })).toBeVisible()
  // A new question goes out on the same session after a stop.
  await panel.getByLabel("Message Sidekick").fill("And now?")
  await panel.getByLabel("Message Sidekick").press("Enter")
  await expect.poll(() => sent.filter((message) => message.method === "session/prompt").length).toBe(2)
})

for (const answer of ["Allow", "Decline"] as const) {
  test(`approval: the bridge asks before a call that can change something; ${answer} goes back as the offered option`, async ({ page }) => {
    await studio(page)
    let picked: unknown
    const sent = await bridge(page, async ({ update, view, ask }) => {
      update(tool("t2", "norn_call_action", { rawInput: { action: "demo.record.close", input: { id: "R-1" } } }))
      picked = await ask("norn_call_action", { action: "demo.record.close", input: { id: "R-1" } })
      const allowed = (picked as { outcome?: { optionId?: string } }).outcome?.optionId === "allow"
      if (allowed) {
        view("norn_call_action", "receipt", { content: [{ type: "text", text: RECEIPT }] })
        update({ sessionUpdate: "tool_call_update", toolCallId: "t2", status: "completed" })
        update(words("Closed R-1."))
      } else {
        update({ sessionUpdate: "tool_call_update", toolCallId: "t2", status: "failed" })
        update(words("I left R-1 open."))
      }
      return "end_turn"
    })
    const panel = await openSidekick(page)
    await panel.getByLabel("Message Sidekick").fill("Close R-1")
    await panel.getByLabel("Message Sidekick").press("Enter")
    const call = panel.locator("[data-tool=norn_call_action]")
    await expect(call).toHaveAttribute("data-status", "awaiting_approval")
    await expect(call).toContainText("Waiting for you")
    await expect(call).toContainText("demo.record.close")
    await expect(panel.locator("[data-role=assistant] [data-slot=mascot]").first()).toHaveAttribute("data-state", "asking")
    // The prompt is reachable by keyboard: the two answers are buttons with their words.
    await expect(call.getByRole("button", { name: "Allow" })).toBeVisible()
    await expect(call.getByRole("button", { name: "Decline" })).toBeVisible()
    const violations = (await new AxeBuilder({ page }).withTags(WCAG).include("[data-slot=sidekick]").analyze()).violations
    expect(violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(" ")).join(", ")}`)).toEqual([])
    if (shots) await shoot(page, "sidekick-approval")
    await call.getByRole("button", { name: answer }).click()
    await expect.poll(() => picked).toEqual({ outcome: { outcome: "selected", optionId: answer === "Allow" ? "allow" : "reject" } })
    if (answer === "Allow") {
      await expect(call).toHaveAttribute("data-status", "completed")
      await expect(call).toContainText("You allowed this call.")
      await expect(panel.locator("[data-view=receipt]")).toContainText("R-1")
      await expect(panel.getByText("Closed R-1.")).toBeVisible()
    } else {
      await expect(call).toHaveAttribute("data-status", "declined")
      await expect(call).toContainText("You declined; nothing ran.")
      await expect(panel.locator("[data-view=receipt]")).toHaveCount(0)
      await expect(panel.getByText("I left R-1 open.")).toBeVisible()
    }
    // Only the answer went back: the page sent no tool call of its own.
    expect(sent.filter((message) => message.method === "_norn/call_tool")).toEqual([])
  })
}

test("refusal: a call the cell refuses draws the server's words; a refused grant says so and Retry asks again", async ({ page }) => {
  const grant = await studio(page, "refused")
  await bridge(page, async ({ update, view }) => {
    update(tool("t3", "norn_call_action"))
    view("norn_call_action", "receipt", { isError: true, content: [], structuredContent: REFUSAL })
    update({ sessionUpdate: "tool_call_update", toolCallId: "t3", status: "completed" })
    update(words("The workspace refused it."))
    return "end_turn"
  })
  const panel = await openSidekick(page)
  await panel.getByLabel("Message Sidekick").fill("Close R-1")
  await panel.getByLabel("Message Sidekick").press("Enter")
  const error = panel.locator("[data-slot=sidekick-error]")
  await expect(error).toContainText("Sidekick couldn't answer.")
  await expect(error).toContainText("Your role in this workspace does not include Sidekick.")
  await expect(panel.locator("[data-role=assistant] [data-slot=mascot]").first()).toHaveAttribute("data-state", "unavailable")
  grant.grant = "ok"
  await error.getByRole("button", { name: "Retry" }).click()
  await expect(error).toHaveCount(0)
  const refused = panel.locator("[data-view=receipt]")
  await expect(refused).toContainText("Closing R-1 needs a review first.")
  await expect(refused).toContainText("law.refused")
  await expect(refused).toContainText("Nothing was changed.")
})

test("a11y: Sidekick idle and mid-conversation and the Ask bar have no axe violations; the composer is reached by keyboard", async ({ page }) => {
  await studio(page)
  await bridge(page, async ({ update, view }) => {
    update(tool("t1", "norn_answer"))
    view("norn_answer", "answer", { content: [{ type: "text", text: ANSWER }] })
    update({ sessionUpdate: "tool_call_update", toolCallId: "t1", status: "completed" })
    update(words("Here is weekly yield from Norn."))
    return "end_turn"
  })
  const panel = await openSidekick(page)
  const check = async (selector: string) => (await new AxeBuilder({ page }).withTags(WCAG).include(selector).analyze()).violations.map(({ id, nodes }) => `${id}: ${nodes.map((node) => node.target.join(" ")).join(", ")}`)
  expect(await check("[data-slot=ask-norn]")).toEqual([])
  expect(await check("[data-slot=sidekick]")).toEqual([])
  await panel.getByLabel("Message Sidekick").focus()
  await expect(panel.getByLabel("Message Sidekick")).toBeFocused()
  await page.keyboard.type("Weekly yield")
  await page.keyboard.press("Enter")
  await expect(panel.getByText("Here is weekly yield from Norn.")).toBeVisible()
  expect(await check("[data-slot=sidekick]")).toEqual([])
  for (const scheme of ["dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    expect(await check("[data-slot=sidekick]")).toEqual([])
  }
  if (shots) await shoot(page, "sidekick-conversation")
})

test("mascot: every state draws on the contact sheet; it moves only when motion is allowed and mirrors in Arabic", async ({ page }) => {
  await page.goto("/sheet.html?state=mascot%2Fstates")
  const states = page.locator("[data-state='mascot/states'] [data-mascot-state]")
  await expect(states).toHaveCount(5)
  for (const state of ["idle", "thinking", "answered", "unavailable", "asking"]) await expect(page.locator(`[data-state='mascot/states'] svg[data-slot=mascot][data-state=${state}]`).first()).toBeVisible()
  const eyes = page.locator("[data-state='mascot/states'] svg[data-state=idle] [data-part=eyes]").first()
  await page.emulateMedia({ reducedMotion: "no-preference" })
  await expect(eyes).toHaveCSS("animation-name", "mascot-blink")
  await page.emulateMedia({ reducedMotion: "reduce" })
  await expect(eyes).toHaveCSS("animation-name", "none")
  await page.goto("/sheet.html?state=mascot%2Frtl")
  const mirrored = page.locator("[data-state='mascot/rtl'] svg[data-slot=mascot]").first()
  await expect(mirrored).toHaveCSS("scale", "-1 1")
  if (shots) {
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
      await page.goto("/sheet.html?state=mascot%2Fcandidates")
      await page.locator("[data-state='mascot/candidates']").screenshot({ path: `${shots}/mascot-candidates-${scheme}.png` })
    }
  }
})

/** Light and dark at a desktop width and at 390px, for review. */
async function shoot(page: Page, name: string) {
  for (const [width, height, label] of [[1440, 900, "desktop"], [390, 844, "390"]] as const) {
    await page.setViewportSize({ width, height })
    for (const scheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
      await page.waitForTimeout(300)
      await page.screenshot({ path: `${shots}/${name}-${label}-${scheme}.png` })
    }
  }
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.emulateMedia({ colorScheme: "light" })
}

test("screenshots: home with the Ask Norn bar and a few core pages in Luma geometry", async ({ page }) => {
  test.skip(!shots, "review screenshots only with AI_SHOTS=<dir>")
  await studio(page)
  await bridge(page, async () => "end_turn")
  for (const [path, name, ready] of [["/", "home", "[data-slot=ask-norn]"], ["/rules", "rules", "main"], ["/sources", "sources", "main"], ["/dashboards", "dashboards", "main"]] as const) {
    for (const [width, height, label] of [[1440, 900, "desktop"], [390, 844, "390"]] as const) {
      await page.setViewportSize({ width, height })
      await page.goto(path)
      await expect(page.locator(ready).first()).toBeVisible()
      for (const scheme of ["light", "dark"] as const) {
        await page.emulateMedia({ colorScheme: scheme, reducedMotion: "reduce" })
        await page.waitForTimeout(500)
        await page.screenshot({ path: `${shots}/${name}-${label}-${scheme}.png`, fullPage: label === "390" })
      }
    }
  }
})
