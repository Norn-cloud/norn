// Screenshots and a recording of Studio on the loaded distributor-1 cell.
//   bun shoot.ts STUDIO_URL OUT_DIR   (run from apps/studio so @playwright/test resolves)
import { chromium } from "@playwright/test"
import { mkdirSync, writeFileSync } from "node:fs"
import { join } from "node:path"

const [base, out] = process.argv.slice(2)
mkdirSync(out, { recursive: true })
const browser = await chromium.launch()
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, recordVideo: { dir: out, size: { width: 1440, height: 900 } } })
const page = await context.newPage()
const shots: Record<string, string> = {}
const pages: Array<[string, string]> = [
  ["01-home", "/"],
  ["02-sales-invoices", "/objects/distributor.SalesInvoice"],
  ["03-holdings", "/objects/canon.Holding"],
  ["04-bank-receipts", "/objects/distributor.BankReceipt"],
  ["05-stock-ledgers", "/objects/distributor.StockLedger"],
  ["06-decisions", "/decisions"],
  ["07-sources", "/sources"],
  ["08-metrics", "/metrics"],
  ["09-guarantees", "/guarantees"],
  ["10-p2p-events", "/objects/p2p.Event"],
]
for (const [name, path] of pages) {
  try {
    await page.goto(base + path, { waitUntil: "networkidle", timeout: 30_000 })
    await page.waitForTimeout(1500)
    const file = join(out, `${name}.png`)
    await page.screenshot({ path: file, fullPage: false })
    shots[name] = `${path} -> ${(await page.title()) || "(untitled)"}`
  } catch (error) {
    shots[name] = `${path} -> FAILED ${String(error).slice(0, 200)}`
  }
}
// open the first holding, where a cross-system disagreement shows
try {
  await page.goto(base + "/objects/canon.Holding", { waitUntil: "networkidle" })
  const row = page.locator("a[href*='/objects/canon.Holding/'], [role=row]").nth(1)
  await row.click({ timeout: 5000 })
  await page.waitForTimeout(1500)
  await page.screenshot({ path: join(out, "11-holding-detail.png") })
  shots["11-holding-detail"] = page.url()
} catch (error) {
  shots["11-holding-detail"] = `FAILED ${String(error).slice(0, 200)}`
}
await context.close()
await browser.close()
writeFileSync(join(out, "shots.json"), JSON.stringify(shots, null, 1))
console.log(JSON.stringify(shots))
