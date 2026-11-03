import assert from "node:assert/strict"
import { chromium } from "playwright"

let input = ""
for await (const chunk of process.stdin) input += chunk
const request = JSON.parse(input)
assert(["http", "mcp", "ui"].includes(request.surface))
if (request.surface !== "ui") {
  const args = { actionId: request.actionId, input: request.input, idempotencyKey: request.idempotencyKey, version: request.version }
  const mcp = request.surface === "mcp"
  const response = await fetch(`${request.url}${mcp ? "/mcp" : "/v1/actions"}`, {
    method: "POST", signal: AbortSignal.timeout(30_000),
    headers: { "content-type": "application/json", "x-norn-org-id": request.org, "x-norn-environment-id": request.environment, "x-norn-actor": request.actor },
    body: JSON.stringify(mcp ? { jsonrpc: "2.0", id: 1, method: "tools/call", params: {
      name: "call_action", arguments: args, _meta: { "norn/idempotencyKey": request.idempotencyKey },
    } } : args),
  })
  process.stdout.write(JSON.stringify({ status: response.status, body: await response.json() }))
} else {
  const browser = await chromium.launch({ headless: true })
  try {
    const context = await browser.newContext({ extraHTTPHeaders: { "x-norn-actor": "demo" } })
    const page = await context.newPage()
    const errors = []
    page.on("pageerror", error => errors.push(error.message))
    const document = await page.goto(`${request.url}/v1/defs?orgId=org_surfaces&environmentId=dev`)
    assert.equal(document.status(), 200)
    const form = page.locator("form").filter({ hasText: request.actionId })
    const controls = await form.locator("[data-field-name]").all()
    assert.equal(controls.length, Object.keys(request.input).length)
    for (const control of controls) {
      const name = await control.getAttribute("data-field-name")
      assert(Object.hasOwn(request.input, name))
      await control.fill(String(request.input[name]))
    }
    // Pin the UI's retry identity so independent fresh tenants have identical intent.
    await form.evaluate((element, key) => { element.dataset.key = key }, request.idempotencyKey)
    // A document read by one principal grants no submission authority to another.
    await context.setExtraHTTPHeaders({ "x-norn-actor": request.actor })
    const [response] = await Promise.all([
      page.waitForResponse(response => response.url().endsWith("/v1/actions") && response.request().method() === "POST"),
      form.getByRole("button").click(),
    ])
    const text = await response.text()
    await form.getByRole("status").filter({ hasText: text }).waitFor()
    assert.equal(await form.getByRole("status").textContent(), text)
    assert.deepEqual(errors, [])
    if (process.env.NORN_SURFACE_SCREENSHOT && request.idempotencyKey === "melt") {
      await page.screenshot({ path: process.env.NORN_SURFACE_SCREENSHOT, fullPage: true })
    }
    process.stdout.write(JSON.stringify({ status: response.status(), body: JSON.parse(text) }))
  } finally {
    await browser.close()
  }

}
