import { readFileSync } from "node:fs"
import { expect, test, type FrameLocator, type Page } from "@playwright/test"
import { viewPage } from "../worker/widget-page.ts"

/**
 * The chat widget (MCP Apps) in a stand-in host, the way Claude and ChatGPT load it: the built
 * page (dist/widgets/norn.html, from `vite build`) in a sandboxed frame, its view named the way the
 * Worker names it, the tool's input and result posted over the bridge. The page bytes are served
 * with no charset in the header, so only the page's own declaration can make the browser read
 * UTF-8; every view must render without mojibake. Documents are the engine's recorded answer and a
 * trace over it in the host's shape.
 */

const WIDGET = readFileSync(new URL("../dist/widgets/norn.html", import.meta.url), "utf8")
const ANSWER = readFileSync(new URL("../../../fixtures/demo/answers/scrap-yield.k1.answer.json", import.meta.url), "utf8")
const answer = JSON.parse(ANSWER) as { lineageRef: string; rows: Record<string, { members: Record<string, { version: number; amounts: number[] }> }> }

/** One bucket of the answer, record by record, in NornNumericTrace's shape, with every receipt resolved. */
function trace(bucket: string): string {
  const row = answer.rows[bucket]
  if (row === undefined) throw new Error(`no bucket ${bucket}`)
  const contributions = Object.entries(row.members).map(([key, { version, amounts }], index) =>
    `{"fact":{"key":${JSON.stringify(key)},"objectType":"demo.Conversion","version":${version}},"amounts":[${amounts.join(",")}],"receiptHash":"rcpt_${index}a9f3c2e81b77d0","factDigest":null,"source":{"status":"resolved"},"commitSeq":${index + 3},"recordedAtMs":1792972800000,"restricted":false,"values":null}`)
  return `{"result":${ANSWER},"bucket":${JSON.stringify(bucket)},"contributions":[${contributions.join(",")}]}`
}

const BUCKET = '"2026-07-27"'
const CASES = {
  answer: { view: "answer", input: { question: "demo.yield_week" }, text: ANSWER, isError: false },
  lineage: { view: "lineage", input: { question: "demo.yield_week", lineageRef: answer.lineageRef, bucket: BUCKET }, text: trace(BUCKET), isError: false },
  receipt: { view: "receipt", input: { id: "melt-9" }, text: '{"outcome":"committed","objectType":"constitution.Melt","objectId":"melt-9","version":1,"receipt":{"receiptHash":"h_8c1f0e7d2a9b44e1","commitSeq":12,"recordedAtMs":1792972800000}}', isError: false },
  refused: { view: "receipt", input: { id: "melt-10" }, text: '{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"action.precondition_failed","category":"law","message":"the output fine weight exceeds the input fine weight","explanation":"melt conservation: output_fine_g ≤ input_fine_g"}', isError: true },
  proposal: { view: "dashboard", input: { kind: "dashboard", id: "yield-by-week" }, text: '{"admitted":true,"diagnostics":[],"restatement":"A dashboard named Yield by week with one chart: weekly gold yield for the last 16 weeks — exact to the gram.","restatementSha256":"ab"}', isError: false },
} as const

/** JSON inside an inline script: "</" must not close it. */
const inline = (value: unknown) => JSON.stringify(value).replaceAll("</", "<\\/")

function hostPage(name: keyof typeof CASES): string {
  const delivery = CASES[name]
  const traces = Object.fromEntries(Object.keys(answer.rows).map((bucket) => [bucket, trace(bucket)]))
  return `<!doctype html><html><head><meta charset="utf-8"><title>host</title></head><body>
<iframe id="widget" sandbox="allow-scripts" src="/__widget/${delivery.view}" style="width:720px;height:400px;border:0"></iframe>
<script>
const delivery = ${inline(delivery)}, traces = ${inline(traces)}
const frame = document.getElementById("widget")
window.calls = []
addEventListener("message", (event) => {
  if (event.source !== frame.contentWindow) return
  const message = event.data
  const reply = (result) => frame.contentWindow.postMessage({ jsonrpc: "2.0", id: message.id, result }, "*")
  if (message.method === "ui/initialize") return reply({ protocolVersion: "2026-01-26", hostContext: { theme: "light" } })
  if (message.method === "ui/notifications/initialized") {
    frame.contentWindow.postMessage({ jsonrpc: "2.0", method: "ui/notifications/tool-input", params: { arguments: delivery.input } }, "*")
    frame.contentWindow.postMessage({ jsonrpc: "2.0", method: "ui/notifications/tool-result", params: { content: [{ type: "text", text: delivery.text }], structuredContent: JSON.parse(delivery.text), isError: delivery.isError } }, "*")
    return
  }
  if (message.method === "ui/notifications/size-changed") { frame.style.height = message.params.height + "px"; return }
  if (message.method === "tools/call") {
    window.calls.push(message.params)
    const text = traces[message.params.arguments.bucket]
    return reply({ content: [{ type: "text", text }], structuredContent: JSON.parse(text), isError: false })
  }
  if (message.id !== undefined) reply({})
})
</script></body></html>`
}

async function open(page: Page, name: keyof typeof CASES): Promise<FrameLocator> {
  await page.route("**/__host/*", (route) => route.fulfill({ status: 200, headers: { "content-type": "text/html; charset=utf-8" }, body: hostPage(name) }))
  // The widget's bytes carry no charset in the header: only the page's own declaration can say UTF-8.
  await page.route("**/__widget/*", (route) => {
    const view = new URL(route.request().url()).pathname.split("/").pop() ?? ""
    const html = viewPage(WIDGET, view, "https://studio.example")
    if (html === null) throw new Error("the built widget page does not declare UTF-8 first")
    return route.fulfill({ status: 200, headers: { "content-type": "text/html" }, body: Buffer.from(html, "utf8") })
  })
  await page.goto(`/__host/${name}`)
  return page.frameLocator("#widget")
}

/** Text that only appears when UTF-8 was read as Latin-1 or Windows-1252. */
const MOJIBAKE = /â€|Ã.|Â/

for (const name of Object.keys(CASES) as (keyof typeof CASES)[]) {
  test(`${name}: renders from the tool result with every character as the server wrote it`, async ({ page }) => {
    const frame = await open(page, name)
    const body = frame.locator("body")
    if (name === "answer") {
      await expect(frame.getByRole("cell", { name: "2026-07-27", exact: true })).toBeVisible()
      await frame.getByRole("cell", { name: "2026-07-27", exact: true }).click()
      // The bucket's records come from the trace tool, called through the host as the person.
      await expect(frame.getByText("SYN-MELT-03-0")).toBeVisible()
      expect(await page.evaluate(() => (window as unknown as { calls: { name: string }[] }).calls.map((call) => call.name))).toEqual(["norn_trace_answer"])
      await expect(frame.getByText("rcpt_0a9f3…")).toBeVisible()
    }
    if (name === "lineage") {
      await expect(frame.getByText("Where 2026-07-27 comes from")).toBeVisible()
      await expect(frame.getByText("49.517 / 50.579")).toBeVisible()
    }
    if (name === "receipt") await expect(frame.getByText("melt-9")).toBeVisible()
    if (name === "refused") {
      await expect(frame.getByText("Nothing was changed.")).toBeVisible()
      await expect(frame.getByText(/output_fine_g ≤ input_fine_g/)).toBeVisible()
    }
    if (name === "proposal") await expect(frame.getByText(/16 weeks — exact to the gram/)).toBeVisible()
    expect(await body.innerText()).not.toMatch(MOJIBAKE)
  })
}
