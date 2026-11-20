import { readFileSync } from "node:fs"
import type { Page } from "@playwright/test"
import { session } from "./object-cell.ts"

/**
 * A replay of the cell's own documents for one refused correction: the scanned invoice
 * SCAN-INV-1041 and its reads, its two printed lines and theirs, the actions offered on it, what
 * each role may do with scanned documents, and the cell's refusal of a total that breaks its laws. Every document is what
 * `crates/norn-cell-do/tests/refusal_detail.rs` (`retained_refusal_documents_are_the_cells_own`)
 * records from a tenant cell for an admin (who holds the refusal-detail grant) and a member (who
 * does not). Each is served with the status the cell answered; nothing here decides anything.
 */

/** The reader: an admin reads which law refused and its values; a member gets the redacted refusal. */
export type Reader = "admin" | "member"

export const RECORD = { objectType: "docs.ScannedDocument", id: "SCAN-INV-1041" }
export const CORRECT = "docs.scanned_document.correct"

interface Retained { readonly status: number; readonly body: unknown }
const retained = (name: string): Retained =>
  JSON.parse(readFileSync(new URL(`../../../fixtures/refusal/${name}`, import.meta.url), "utf8")) as Retained

export interface RefusalCell {
  /** Each call's input, as the form sent it. */
  readonly calls: unknown[]
}

export async function refusalCell(page: Page, reader: Reader): Promise<RefusalCell> {
  const calls: unknown[] = []
  const serve = (name: string) => {
    const { status, body } = retained(name)
    return { status, json: body }
  }
  await page.route("**/api/**", (route) => route.fulfill({ status: 404, json: { code: "route.not_found", message: "not found" } }))
  await session(page)
  await page.route("**/api/host/v1/definitions**", (route) => {
    const { body } = retained("definitions.json")
    return route.fulfill({ json: body })
  })
  await page.route("**/api/host/v1/access**", (route) => {
    const typed = new URL(route.request().url()).searchParams.get("objectType")
    return route.fulfill(serve(typed === null ? `access.${reader}.json` : `type-access.${reader}.json`))
  })
  await page.route("**/api/host/v1/actions**", (route) => {
    const url = new URL(route.request().url())
    if (route.request().method() === "POST" && url.pathname.endsWith(`/v1/actions/${CORRECT}`)) {
      const body = JSON.parse(route.request().postData() ?? "null") as { input?: { total?: unknown } } | null
      calls.push(body?.input)
      // The total the lines sum to is admitted (recorded for the admin); any other is the recorded refusal.
      return route.fulfill(serve(reader === "admin" && body?.input?.total === "30.50" ? "receipt.admin.json" : `refusal.${reader}.json`))
    }
    if (route.request().method() === "GET" && url.pathname.endsWith("/v1/actions")) return route.fulfill(serve(`actions.${reader}.json`))
    return route.fulfill({ status: 404, json: { code: "route.not_found", message: "not found" } })
  })
  const notFound = { status: 404, json: { code: "object.not_found", message: "No such readable object." } }
  await page.route("**/api/host/v1/objects/**", (route) => {
    const path = decodeURIComponent(new URL(route.request().url()).pathname.replace(/^.*\/api\/host/, ""))
    const record = `/v1/objects/${RECORD.objectType}/${RECORD.id}`
    if (path === record || path.startsWith(`${record}/`)) {
      const leaf = path === record ? "object" : path.slice(record.length + 1)
      return ["object", "lineage", "links", "history", "findings"].includes(leaf) ? route.fulfill(serve(`${leaf}.${reader}.json`)) : route.fulfill(notFound)
    }
    // The printed lines the law sums: their own record and where each value was read from.
    const line = /^\/v1\/objects\/docs\.DocumentLine\/(SCAN-INV-1041~line-[12])(?:\/(links|lineage))?$/.exec(path)
    if (line !== null) return route.fulfill(serve(`${line[1]}.${line[2] ?? "object"}.${reader}.json`))
    return route.fulfill(notFound)
  })
  return { calls }
}
