import type { Page, Route, WebSocketRoute } from "@playwright/test"
import { refuse } from "./action-cell.ts"

/**
 * A stand-in for the cell's approval-gated calls (APPROVAL-1, APPROVAL-RECOVERY-1,
 * APPROVAL-REQUESTED-1) over one gated action, shared by every page of a test, so a requester and
 * an approver act on the same cell from their own browsers. It follows the served contract that
 * crates/norn-cell-do/tests/approvals.rs pins (`the_requester_reads_and_runs_their_approved_call`):
 *
 * - A call of the gated action parks (202 `{outcome: "pending", decision}`); the same call parks once.
 * - `GET /v1/decisions` lists it with its `call` to a decider who is not its requester, never to
 *   the requester; `:decide` from the requester is 404 `decision.not_found`.
 * - Approving runs nothing: the decide answers `call: {committed: false, state: "not_executed"}`.
 * - `GET /v1/decisions:requested` serves the requester's own calls, the call exactly as it waits
 *   (`encodedInput`, `idempotencyKey`) and `canRun`; anyone else gets no item, even by id.
 * - The requester's retry of exactly that call runs it (handed to the action stand-in, which
 *   commits); a role revoked since it parked gets the opaque action denial and nothing runs. A
 *   rejected call's retry is 409 `approval.rejected`.
 */

export const GATED = "canon.holding.adjust"
const DEFINITION = "canon.holding.adjust_approval"
const HOUR = 3_600_000

export interface Actor { readonly id: string; readonly name: string; readonly role: string }
export const REQUESTER: Actor = { id: "u_requester", name: "Salma Requester", role: "member" }
export const APPROVER: Actor = { id: "u_approver", name: "Omar Approver", role: "admin" }

interface Parked {
  readonly id: string; readonly input: string; readonly key: string; readonly requester: string
  status: "pending" | "approved" | "rejected"; state: "not_executed" | "committed" | null
  readonly requestedAtMs: number; readonly expiresAtMs: number; resolvedAtMs: number | null
}

export interface ApprovalCell {
  readonly parked: Parked[]
  /** The requester's role is revoked after the call parked: their retry is decided under it. */
  revoke(actor: Actor): void
  /** Every call of the gated action, by who sent it, with its body. */
  readonly calls: { readonly by: string; readonly body: string }[]
  /** Install the stand-in on `page`, signed in as `actor`. */
  install(page: Page, actor: Actor): Promise<void>
}

export function approvalCell(now = Date.UTC(2026, 10, 21, 9)): ApprovalCell {
  const parked: Parked[] = []
  const revoked = new Set<string>()
  const calls: { by: string; body: string }[] = []
  const subs = new Map<string, { ws: WebSocketRoute; id: string; path: string; digest: string; actor: string }>()
  let seq = 0

  const mine = (actor: string) => parked.filter((p) => p.requester === actor)
  const document = (p: Parked) => ({
    id: p.id, question: "Adjust this holding's counted quantity?", risk: "medium", requestedAtMs: p.requestedAtMs, expiresAtMs: p.expiresAtMs, status: p.status,
    ...p.resolvedAtMs === null ? {} : { resolvedAtMs: p.resolvedAtMs },
    call: { action: GATED, definition: DEFINITION, input: JSON.parse(p.input) as unknown, encodedInput: p.input, idempotencyKey: p.key, ...p.state === null ? {} : { state: { committed: p.state === "committed", state: p.state } } },
    canRun: p.status === "approved" && p.state === "not_executed",
  })
  const waiting = (actor: string) => mine(actor).filter((p) => p.status === "pending" || (p.status === "approved" && p.state === "not_executed"))
  const decidable = (actor: string) => parked.filter((p) => p.status === "pending" && p.requester !== actor)
  const decision = (p: Parked) => ({
    id: p.id, run: { id: `norn.approval/${p.id}`, generation: 1, state: null }, flow: { ruleId: DEFINITION, version: "1", title: null }, step: { id: "approval", label: null },
    question: "Adjust this holding's counted quantity?", risk: "medium",
    options: [
      { id: "approve", label: "Approve", scope: "This call", effect: { action: GATED } },
      { id: "reject", label: "Reject", scope: "This call", effect: { binding: DEFINITION } },
    ],
    agentPick: null, requestedAtMs: p.requestedAtMs, expiresAtMs: p.expiresAtMs, context: { subjects: [] }, inputSchema: null,
    call: { action: GATED, definition: DEFINITION, input: JSON.parse(p.input) as unknown },
  })
  // Live reads (UI-LIVE-1): each subscription hears when its route's answer changes, for its own actor.
  const versionOf = (path: string, actor: string): string => {
    const url = new URL(path, "https://cell")
    if (url.pathname === "/v1/decisions:requested") {
      const id = url.searchParams.get("id")
      return JSON.stringify((id === null ? waiting(actor) : mine(actor).filter((p) => p.id === id)).map(document))
    }
    if (url.pathname === "/v1/decisions") return decidable(actor).map((p) => p.id).join(",")
    return "404"
  }
  const push = () => {
    for (const sub of subs.values()) {
      const digest = versionOf(sub.path, sub.actor)
      if (digest === sub.digest) continue
      sub.digest = digest
      sub.ws.send(JSON.stringify({ type: "snapshot", id: sub.id, status: digest === "404" ? 404 : 200, digest, body: null }))
    }
  }

  async function install(page: Page, actor: Actor) {
    await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: actor.id, name: actor.name, email: `${actor.id}@example.com` }, organization: { id: "org_demo", name: "Demo", role: actor.role }, environmentId: "analytics" } }))
    // Registered after the action stand-in, so the gated action's calls reach this route first.
    await page.route(`**/api/host/v1/actions/${GATED}**`, async (route: Route) => {
      if (route.request().method() !== "POST") return route.fallback()
      const body = route.request().postData() ?? ""
      calls.push({ by: actor.id, body })
      const sent = JSON.parse(body) as { input: unknown; idempotencyKey: string }
      const input = JSON.stringify(sent.input)
      const same = parked.find((p) => p.key === sent.idempotencyKey && p.requester === actor.id)
      if (same !== undefined && same.input !== input) return refuse(route, "conflict")
      if (same === undefined) {
        seq += 1
        parked.push({ id: `dec_${seq.toString().padStart(6, "0")}QXBwcm92ZWQ`, input, key: sent.idempotencyKey, requester: actor.id, status: "pending", state: null, requestedAtMs: now, expiresAtMs: now + 7 * 24 * HOUR, resolvedAtMs: null })
        push()
        const fresh = parked.at(-1)!
        return route.fulfill({ status: 202, json: { outcome: "pending", decision: { id: fresh.id, expiresAtMs: fresh.expiresAtMs } } })
      }
      if (same.status === "pending") return route.fulfill({ status: 202, json: { outcome: "pending", decision: { id: same.id, expiresAtMs: same.expiresAtMs } } })
      if (same.status === "rejected") return route.fulfill({ status: 409, json: { format: "norn/refusal@2", code: "approval.rejected", category: "conflict", retryable: false, message: "a decider rejected this call", diagnostics: [] } })
      // Approved: the requester's own retry, decided again under their current authority.
      if (revoked.has(actor.id)) return refuse(route, "denied")
      same.state = "committed"
      push()
      return route.fallback()
    })
    await page.route("**/api/host/v1/decisions**", async (route: Route) => {
      const url = new URL(route.request().url())
      const path = url.pathname.slice(url.pathname.indexOf("/v1/decisions"))
      if (path === "/v1/decisions:requested") {
        const id = url.searchParams.get("id")
        const items = (id === null ? waiting(actor.id) : mine(actor.id).filter((p) => p.id === id)).map(document)
        return route.fulfill({ json: { items, serverNowMs: now } })
      }
      if (path === "/v1/decisions") {
        const open = decidable(actor.id)
        const limit = Number(url.searchParams.get("limit") ?? "50")
        return route.fulfill({ json: { items: open.slice(0, limit).map(decision), total: open.length, nextCursor: null, serverNowMs: now } })
      }
      const id = decodeURIComponent(path.slice("/v1/decisions/".length).replace(/:decide$/, ""))
      const target = parked.find((p) => p.id === id)
      // The requester never decides their own call, and the cell does not say it exists.
      if (target === undefined || target.requester === actor.id) return route.fulfill({ status: 404, json: { format: "norn/refusal@2", code: "decision.not_found", category: "visibility", retryable: false, message: "no such decision", diagnostics: [] } })
      const { choice } = JSON.parse(route.request().postData() ?? "{}") as { choice: string }
      if (target.status !== "pending") return route.fulfill({ status: 409, json: { format: "norn/refusal@2", code: "decision.resolved", category: "conflict", retryable: false, message: "someone resolved it first", diagnostics: [], resolution: { resolution: "decided", by: "another decider", atMs: target.resolvedAtMs, choice: null } } })
      target.status = choice === "approve" ? "approved" : "rejected"
      target.state = choice === "approve" ? "not_executed" : null
      target.resolvedAtMs = now + 60_000
      seq += 1
      push()
      return route.fulfill({ json: {
        outcome: "decided", decision: { id, choice, note: null, decidedBy: "you", decidedAtMs: now + 60_000 },
        receipt: { receiptHash: seq.toString(16).padStart(64, "d"), commitSeq: seq, recordedAtMs: now + 60_000 }, run: { id: `norn.approval/${id}`, state: null },
        ...choice === "approve" ? { call: { committed: false, state: "not_executed" } } : {},
      } })
    })
    await page.routeWebSocket("**/api/host/v1/subscribe**", (ws) => {
      ws.onMessage((raw) => {
        const message = JSON.parse(String(raw)) as { type: string; id: string; path: string }
        // Ids are unique per socket, and every page has its own.
        if (message.type === "unsubscribe") { for (const [key, sub] of subs) if (sub.ws === ws && sub.id === message.id) subs.delete(key); return }
        subs.set(`${actor.id}:${message.id}:${subs.size}`, { ws, id: message.id, path: message.path, digest: "", actor: actor.id })
        push()
      })
      ws.onClose(() => { for (const [id, sub] of subs) if (sub.ws === ws) subs.delete(id) })
    })
  }

  return { parked, calls, revoke: (actor) => { revoked.add(actor.id) }, install }
}
