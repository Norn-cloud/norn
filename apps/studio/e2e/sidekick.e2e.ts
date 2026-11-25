import { readFileSync } from "node:fs"
import { expect, test, type Page, type WebSocketRoute } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * Sidekick in the shell, with a stand-in bridge: the page asks Studio for a grant, opens the
 * bridge's socket on its own origin and speaks ACP to it. The stand-in answers like the bridge
 * does (the agent's words, one Norn tool call, the tool's view), so the row checks what Studio
 * draws, and that only text and the grant leave the page. The bridge's own refusals are its tests.
 */

const ANSWER = readFileSync(new URL("../../../fixtures/demo/answers/scrap-yield.k1.answer.json", import.meta.url), "utf8")
const GRANT = "e2e.sidekick.grant"

async function studio(page: Page, bridge: boolean) {
  await session(page)
  const definitions = JSON.parse(readFileSync(new URL("../../../fixtures/demo/explorer/three-systems.definitions.json", import.meta.url), "utf8")) as unknown
  await page.route("**/api/host/v1/**", (route) => {
    const path = new URL(route.request().url()).pathname
    if (path.endsWith("/v1/definitions")) return route.fulfill({ json: { artifacts: definitions } })
    if (path.endsWith("/v1/inbox")) return route.fulfill({ json: { scanLimit: 10000, sections: [], items: [] } })
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } })
  })
  // Where no bridge serves Sidekick, the path is the SPA's fallback page.
  await page.route("**/sidekick/config.json", (route) => bridge
    ? route.fulfill({ json: { sidekick: 1, socket: "/sidekick/ws" } })
    : route.fulfill({ status: 200, contentType: "text/html", body: "<!doctype html><div id=root></div>" }))
  await page.route("**/api/sidekick/grant", (route) => route.fulfill({ json: { token: GRANT, expiresAt: Date.now() + 600_000 } }))
}

/** The stand-in bridge: records what the page sent, and answers one turn. */
async function bridge(page: Page) {
  const sent: { method?: string; params?: Record<string, unknown> }[] = []
  await page.routeWebSocket("**/sidekick/ws", (ws: WebSocketRoute) => {
    const send = (message: unknown) => ws.send(JSON.stringify(message))
    const update = (value: unknown) => send({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: value } })
    ws.onMessage((raw) => {
      const message = JSON.parse(String(raw)) as { id?: number; method?: string; params?: Record<string, unknown> }
      sent.push(message)
      const reply = (result: unknown) => send({ jsonrpc: "2.0", id: message.id, result })
      if (message.method === "initialize") return reply({ protocolVersion: 1, agentCapabilities: { loadSession: false, promptCapabilities: {}, mcpCapabilities: {} }, authMethods: [] })
      if (message.method === "session/new") {
        // The bridge sends the cell's own label for each tool it lists before the session starts.
        send({ jsonrpc: "2.0", method: "_norn/tools", params: { tools: [{ name: "norn_answer", title: "Answer a question" }] } })
        return reply({ sessionId: "s1" })
      }
      if (message.method !== "session/prompt") return
      update({ sessionUpdate: "tool_call", toolCallId: "t1", title: "mcp__norn__norn_answer", kind: "other", status: "pending", _meta: { claudeCode: { toolName: "mcp__norn__norn_answer" } } })
      send({ jsonrpc: "2.0", method: "_norn/tool_result", params: { tool: "norn_answer", view: "answer", input: { question: "demo.yield_week" }, result: { content: [{ type: "text", text: ANSWER }] } } })
      update({ sessionUpdate: "tool_call_update", toolCallId: "t1", status: "completed" })
      update({ sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Here is weekly yield from Norn. Pick a week to see the records behind it." } })
      reply({ stopReason: "end_turn" })
    })
  })
  return sent
}

test("sidekick: Home's Ask bar opens Sidekick, which answers through a Norn tool and draws its view", async ({ page }) => {
  await studio(page, true)
  const sent = await bridge(page)
  await page.setViewportSize({ width: 1440, height: 900 })
  await page.goto("/")
  await page.getByLabel("Ask or tell Norn anything").fill("What's our weekly yield?")
  await page.getByRole("button", { name: "Ask" }).click()
  const panel = page.locator("[data-slot=sidekick]")
  await expect(panel.getByText("What's our weekly yield?")).toBeVisible()
  await expect(panel.locator("[data-tool=norn_answer][data-status=completed]")).toContainText("Answer a question")
  await expect(panel.locator("[data-view=answer]")).toBeVisible()
  // The view is drawn, not left loading: Studio's own answer card, from the tool's document.
  await expect(panel.getByText("Drawing Norn's answer…")).toHaveCount(0)
  await expect(panel.locator("[data-view=answer]").getByRole("table")).toBeVisible()
  await expect(panel.getByText("Pick a week to see the records behind it.")).toBeVisible()
  // The page sent the grant once, in initialize, and the prompt as text.
  expect(sent.map((message) => message.method)).toEqual(["initialize", "session/new", "session/prompt"])
  expect((sent[0]?.params?._meta as Record<string, unknown>)["norn.cloud/grant"]).toBe(GRANT)
  expect(JSON.stringify(sent.slice(1))).not.toContain(GRANT)
  expect(sent[2]?.params?.prompt).toEqual([{ type: "text", text: "What's our weekly yield?" }])
  await page.screenshot({ path: "test-results/sidekick-light.png" })
  await page.emulateMedia({ colorScheme: "dark" })
  await page.screenshot({ path: "test-results/sidekick-dark.png" })
})

test("sidekick: where no bridge serves it, the rail has no Sidekick and Ask goes to Metrics", async ({ page }) => {
  await studio(page, false)
  await page.goto("/")
  await expect(page.getByLabel("Ask or tell Norn anything")).toBeVisible()
  await expect(page.getByRole("button", { name: "Sidekick" })).toHaveCount(0)
  await page.getByLabel("Ask or tell Norn anything").fill("What's our stock worth?")
  await page.getByRole("button", { name: "Ask" }).click()
  await expect(page).toHaveURL(/\/metrics$/)
})
