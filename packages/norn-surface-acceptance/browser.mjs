import assert from "node:assert/strict"
import { chromium } from "playwright"

let input = ""
for await (const chunk of process.stdin) input += chunk
const request = JSON.parse(input)
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
  // A document loaded before revocation grants no authority to submit afterward.
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
