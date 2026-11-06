import type { FnRef } from "../../../../../tools/norn-ts/src/builder/index.ts"
import { loadInvoice, notify } from "../../../../../tools/norn-ts/fixtures/flows/support/billing.builder.ts"
import type { CanvasEdge, CanvasNode, Graph } from "./graph.ts"

/**
 * The tools an automation may call, each a typed handle whose input and
 * output schemas type the step. Recorded billing tools until the connector
 * gateway (lane CONNECT) serves a signed-in tool surface.
 */
export const RECORDED_TOOLS: readonly FnRef[] = [loadInvoice, notify]

/** The workspace an automation is lowered in: the recorded world's, until Studio reads the session's scope. The server scopes it again on save. */
export const RECORDED_SCOPE = { scope: { orgId: "org_demo", environmentId: "dev" }, sourcePath: "apps/studio/canvas" } as const

const node = (id: string, kind: string, x: number, y: number, config: Record<string, string> = {}): CanvasNode => ({ id, kind, position: { x, y }, config })
const edge = (source: string, sourcePort: string, target: string): CanvasEdge => ({ id: `${source}.${sourcePort}`, source, sourcePort, target, targetPort: "in", config: {} })

/** A first automation to edit: load an invoice, stop if it is settled, remind when it is overdue. */
export const EXAMPLE: Graph = {
  nodes: [
    node("start", "start", 0, 140),
    node("load", "perform", 230, 140, { tool: "billing.load_invoice", bind: "invoice", input: JSON.stringify({ invoiceId: "inv-1" }) }),
    node("settled", "route", 470, 140, { branches: JSON.stringify([[{ left: "invoice.status", op: "==", right: "paid" }], [{ left: "invoice.status", op: "==", right: "draft" }]]) }),
    node("paid", "succeed", 790, 0, { outcome: "already-paid" }),
    node("draft", "succeed", 790, 110, { outcome: "not-issued" }),
    node("overdue", "route", 790, 250, { branches: JSON.stringify([[{ left: "invoice.overdue", op: "==", right: "true" }, { left: "invoice.amountMinor", op: ">", right: "0" }]]) }),
    node("remind", "perform", 1100, 200, { tool: "billing.notify", input: JSON.stringify({ invoiceId: "invoice.id", outcome: "overdue" }) }),
    node("open", "succeed", 1100, 330, { outcome: "open" }),
  ],
  edges: [
    edge("start", "next", "load"), edge("load", "next", "settled"),
    edge("settled", "b0", "paid"), edge("settled", "b1", "draft"), edge("settled", "next", "overdue"),
    edge("overdue", "b0", "remind"), edge("overdue", "next", "open"),
  ],
}
