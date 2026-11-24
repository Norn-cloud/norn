import { readFileSync } from "node:fs"
import { expect, test, type Page, type Route, type WebSocketRoute } from "@playwright/test"
import { session } from "./object-cell.ts"

// The lifecycle view (UI-LIFECYCLE-2): both the contact-sheet specimens and the full Studio
// Records & Record page flows (Table | Lifecycle toggle, scoped `object_facets.rs` counts,
// honest `lowerBound` display, `otherRows` handling, real WASM checker witness stepping,
// WebSocket live refresh, Arabic RTL layout, and consistent record-level `LifecycleStepper`).
// SHOTS=<dir> saves light and dark screenshots of the real flows.

const shots = process.env.SHOTS
async function shoot(page: Page, name: string) {
  if (!shots) return
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme })
    await page.waitForTimeout(400)
    await page.screenshot({ path: `${shots}/lifecycle-${name}-${scheme}.png`, fullPage: true })
  }
  await page.emulateMedia({ colorScheme: "light" })
}

const specimen = async (page: Page, state: string, locale = "en") => {
  await page.goto(`/sheet.html?state=${encodeURIComponent(state)}&locale=${locale}`)
  return page.locator(`[data-state="${state}"] norn-lifecycle-view`)
}
const card = (view: ReturnType<Page["locator"]>, state: string) => view.locator(`[data-state="${state}"]`)

const readFixture = (relative: string) => JSON.parse(readFileSync(new URL(`../../../${relative}`, import.meta.url), "utf8")) as Record<string, unknown>

const PURCHASE_ACTION_ID = "commerce.submit_purchase_order.action"
const PURCHASE_WIDGET_ID = "commerce.submit_order.widget"

/** Build the real stuck-widget artifact set from `crates/norn-checker/tests/ui_lifecycle.rs`, plus Arabic labels. */
function purchaseOrderArtifacts(): unknown[] {
  const entity = readFixture("fixtures/ir/entity-v2/artifact-set/commerce-purchase-order.entity.json") as {
    payload: { objectType: { lifecycles: { property: string; transitions: { id: string; from: string[] }[] }[] } }
  }
  for (const lifecycle of entity.payload.objectType.lifecycles) {
    if (lifecycle.property !== "status") continue
    for (const transition of lifecycle.transitions) {
      if (transition.id === "cancel") transition.from = ["submitted", "approved"]
    }
  }
  const rule = readFixture("fixtures/rule-ir-v2/valid/purchase-order-approval.rule.json")
  const action = {
    irVersion: "0.1", artifactId: PURCHASE_ACTION_ID, kind: "action",
    namespace: "commerce", name: PURCHASE_ACTION_ID, version: "0.1.0", lifecycle: "draft",
    scope: { orgId: "org_demo", environmentId: "analytics" },
    dependencies: [], provenance: { source: "authoring_sdk" },
    payload: {
      actionId: "commerce.submit_purchase_order", scopeRequired: ["orgId", "environmentId"],
      idempotent: true, ledgered: true, ledgerCategory: "commerce",
      inputSchema: { type: "object", properties: {}, additionalProperties: false },
      outputSchema: { type: "object", properties: {}, additionalProperties: false },
      synchronousRule: { artifactId: "commerce.purchase_order_approval.rule", version: "0.1.0" },
    },
  }
  const outcome = (id: string) => ({ eventId: id, source: { kind: "outcome", outcome: id } })
  const back = (id: string) => ({ transitionId: `on-${id}`, from: "submitting", on: id, target: "open" })
  const widget = {
    irVersion: "0.1", artifactId: PURCHASE_WIDGET_ID, kind: "widget",
    namespace: "commerce", name: PURCHASE_WIDGET_ID, version: "0.1.0", lifecycle: "draft",
    scope: { orgId: "org_demo", environmentId: "analytics" },
    dependencies: [{ artifactId: PURCHASE_ACTION_ID, kind: "action", version: "0.1.0", required: true }],
    provenance: { source: "authoring_sdk" },
    payload: {
      widgetVersion: "0.1", widgetId: PURCHASE_WIDGET_ID,
      dataDependencies: [], viewStates: [{ stateId: "ready", kind: "ready" }],
      actionBindings: [], permissions: [],
      ux: {
        uxVersion: "0.1",
        pages: [{
          pageId: "main", name: "Main", contextParams: [],
          root: {
            nodeId: "root", semanticId: "root",
            component: { kind: "section", section: { sectionId: "main", kind: "section", label: { en: "Main" }, fields: [], order: 1 } },
            children: [{ nodeId: "invoke", semanticId: "invoke", component: { kind: "action", placementId: "invoke" }, children: [] }],
          },
          machine: {
            machineVersion: "0.1", initialStateId: "open", fields: [],
            events: [
              { eventId: "submit", source: { kind: "user", nodeId: "invoke" } },
              outcome("accepted"), outcome("refused"), outcome("pending_approval"), outcome("network_failure"), outcome("timeout"),
            ],
            states: [
              { stateId: "open", role: "idle" },
              { stateId: "submitting", role: "pending", deadlineMs: 15000, awaits: { kind: "action", bindingId: "invoke" } },
              { stateId: "done", role: "terminal" },
            ],
            transitions: [
              { transitionId: "submit", from: "open", on: "submit", target: "submitting", effects: [{ kind: "start", call: { kind: "action", bindingId: "invoke" } }] },
              { transitionId: "on-accepted", from: "submitting", on: "accepted", target: "done" },
              back("refused"), back("pending_approval"), back("network_failure"), back("timeout"),
            ],
            lifecycle: {
              objectType: "commerce.PurchaseOrder", property: "status",
              syncs: [{ transitionId: "on-accepted", lifecycleTransition: "submit" }],
            },
          },
        }],
        dataBindings: [], fields: [], forms: [], navigationItems: [], overrides: [],
        actionBindings: [{ bindingId: "invoke", action: { id: PURCHASE_ACTION_ID, version: "0.1.0" }, inputMapping: {}, inputs: {}, requiredCapability: "commerce.submit" }],
        actionPlacements: [{ placementId: "invoke", bindingId: "invoke", label: { en: "Submit" }, location: "toolbar", order: 1, selectionInputMapping: {}, confirmation: { severity: "safe", strength: "none" } }],
      },
    },
  }
  const labels = {
    irVersion: "0.1", artifactId: "commerce.PurchaseOrder.labels", kind: "labels",
    namespace: "commerce", name: "Purchase order labels", version: "0.1.0", lifecycle: "active",
    scope: {}, dependencies: [], provenance: { source: "system" },
    payload: {
      labelsVersion: "0.1",
      entries: [
        { target: { kind: "objectType", objectType: "commerce.PurchaseOrder" }, label: { en: "Purchase order", ar: "أمر شراء" } },
        { target: { kind: "property", objectType: "commerce.PurchaseOrder", property: "status" }, label: { en: "Status", ar: "الحالة" } },
        { target: { kind: "property", objectType: "commerce.PurchaseOrder", property: "payment_status" }, label: { en: "Payment status", ar: "حالة الدفع" } },
        ...Object.entries({ draft: "مسودة", submitted: "مُقدَّم", approved: "معتمد", rejected: "مرفوض", received: "مستلم", cancelled: "ملغى" })
          .map(([value, ar]) => ({ target: { kind: "enumValue", objectType: "commerce.PurchaseOrder", property: "status", value }, label: { ar } })),
      ],
    },
  }
  const plain = {
    irVersion: "0.1", artifactId: "commerce.Note.entity", kind: "entity",
    namespace: "commerce", name: "Note", version: "0.1.0", lifecycle: "active",
    scope: {}, dependencies: [], provenance: { source: "system" },
    payload: {
      entityVersion: "2.0",
      objectType: { name: "commerce.Note", properties: [{ id: "label", type: { kind: "text" } }] },
    },
  }
  return [entity, rule, action, widget, labels, plain]
}

interface FacetsState {
  status: number
  body: unknown
}

interface FacetsSubscription {
  readonly id: string
  readonly property: string
  readonly socket: WebSocketRoute
  readonly sendSnapshot: (digest?: string) => void
}

/** Stand-in live cell serving definitions, records, scoped facets (`object_facets.rs`), and WebSocket live refresh. */
async function lifecycleCell(
  page: Page,
  initial: Record<string, FacetsState>,
  options: {
    readonly includeWidget?: boolean
    readonly onFacetsSubscribe?: (sub: FacetsSubscription) => void
  } = {},
) {
  await session(page)
  const allArtifacts = purchaseOrderArtifacts()
  const artifacts = options.includeWidget === false
    ? allArtifacts.filter((artifact) => (artifact as { kind?: unknown }).kind !== "widget")
    : allArtifacts
  const facetsByProp: Record<string, FacetsState> = { ...initial }
  const facetRequests: URL[] = []
  const refusedPaths: string[] = []
  const subs = new Map<string, { ws: { send: (data: string) => void }; path: string; seq: number }>()
  let revision = 1

  await page.route("**/api/host/v1/definitions**", (route: Route) => {
    const url = new URL(route.request().url())
    const tail = url.pathname.split("/v1/definitions")[1] ?? ""
    if (tail === ":list") return route.fulfill({ json: { items: [], nextCursor: null } })
    return route.fulfill({ json: { artifacts } })
  })
  await page.route("**/api/host/v1/actions**", (route: Route) => route.fulfill({ json: { actions: [] } }))
  await page.route("**/api/host/v1/access**", (route: Route) => route.fulfill({ status: 404, json: { code: "route.not_found", message: "not served" } }))

  await page.route("**/api/host/v1/objects/**", (route: Route) => {
    const url = new URL(route.request().url())
    if (url.searchParams.get("orgId") !== "org_demo" || url.searchParams.get("environmentId") !== "analytics") {
      return route.fulfill({ status: 403, json: { code: "scope.denied", message: "Not your workspace." } })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder:facets")) {
      facetRequests.push(url)
      const prop = url.searchParams.get("properties") ?? "status"
      const state = facetsByProp[prop] ?? facetsByProp["status"]!
      return route.fulfill({ status: state.status, json: state.body })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder/PO-101/links")) {
      return route.fulfill({ json: { objectType: "commerce.PurchaseOrder", id: "PO-101", items: [], next: null, counts: { complete: true, bound: 1000, groups: [] } } })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder/PO-101/lineage") || url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder/PO-101/findings") || url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder/PO-101/history")) {
      return route.fulfill({ status: 404, body: "" })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder/PO-101")) {
      return route.fulfill({ json: { objectType: "commerce.PurchaseOrder", id: "PO-101", scope: "org_demo/analytics", version: 2, values: { po_number: "PO-101", status: "submitted", payment_status: "unpaid" }, withheld: false, withheldProperties: [] } })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.PurchaseOrder")) {
      return route.fulfill({
        json: {
          objectType: "commerce.PurchaseOrder",
          records: [
            { objectType: "commerce.PurchaseOrder", id: "PO-101", scope: "org_demo/analytics", version: 2, values: { po_number: "PO-101", status: "submitted", payment_status: "unpaid" }, withheld: false, withheldProperties: [] },
            { objectType: "commerce.PurchaseOrder", id: "PO-102", scope: "org_demo/analytics", version: 1, values: { po_number: "PO-102", status: "draft", payment_status: "unpaid" }, withheld: false, withheldProperties: [] },
          ],
          quarantineCount: 0, scannedThrough: null, query: { sort: [], filter: null }, next: null, total: { count: 2, approximate: false },
        },
      })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.Note:facets")) {
      return route.fulfill({ json: { objectType: "commerce.Note", rows: 0, withheldRows: 0, quarantinedRows: 0, lowerBound: false, scanLimit: 10000, facets: [] } })
    }
    if (url.pathname.endsWith("/v1/objects/commerce.Note")) {
      return route.fulfill({ json: { objectType: "commerce.Note", records: [], quarantineCount: 0, scannedThrough: null, query: { sort: [], filter: null }, next: null, total: { count: 0, approximate: false } } })
    }
    return route.fulfill({ status: 404, body: "" })
  })

  const snapshotFor = (path: string) => {
    const parsed = new URL(path, "https://cell")
    const prop = parsed.searchParams.get("properties") ?? "status"
    return facetsByProp[prop] ?? facetsByProp["status"]!
  }

  await page.routeWebSocket("**/api/host/v1/subscribe**", (ws) => {
    ws.onMessage((raw) => {
      const msg = JSON.parse(String(raw)) as { type: string; id: string; path: string }
      if (msg.type === "unsubscribe") { subs.delete(msg.id); return }
      if (msg.type !== "subscribe") return
      // Protocol-faithful check matching `apps/norn-cell-do/subscribe.mjs` and `crates/norn-cell-rivet/src/live.rs`.
      if (typeof msg.path !== "string" || msg.path.length > 512 || !msg.path.startsWith("/v1/") || msg.path.startsWith("/v1/subscribe") || msg.path.includes("..")) {
        refusedPaths.push(msg.path)
        ws.send(JSON.stringify({ type: "refused", id: msg.id, code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" }))
        return
      }
      subs.set(msg.id, { ws, path: msg.path, seq: revision })
      const sendSnapshot = (digest = `rev-${revision}`) => {
        const current = snapshotFor(msg.path)
        ws.send(JSON.stringify({ type: "snapshot", id: msg.id, status: current.status, digest, body: current.body }))
      }
      const parsed = new URL(msg.path, "https://cell")
      const property = parsed.searchParams.get("properties")
      if (options.onFacetsSubscribe && parsed.pathname === "/v1/objects/commerce.PurchaseOrder:facets" && property !== null) {
        options.onFacetsSubscribe({ id: msg.id, property, socket: ws, sendSnapshot })
      } else {
        sendSnapshot()
      }
    })
    ws.onClose(() => { for (const [id, sub] of subs) if (sub.ws === ws) subs.delete(id) })
  })

  return {
    facetRequests,
    refusedPaths,
    setFacets(property: string, next: FacetsState) {
      facetsByProp[property] = next
      revision += 1
    },
    publish(property: string, next: FacetsState) {
      facetsByProp[property] = next
      revision += 1
      for (const [id, sub] of subs) {
        const parsed = new URL(sub.path, "https://cell")
        if ((parsed.searchParams.get("properties") ?? "status") !== property) continue
        sub.ws.send(JSON.stringify({ type: "snapshot", id, status: next.status, digest: `rev-${revision}`, body: next.body }))
      }
    },
  }
}

const okFacets = (property: string, values: Record<string, number>, extra: { lowerBound?: boolean; scanLimit?: number; withheldRows?: number; quarantinedRows?: number; otherRows?: number; nulls?: number; rows?: number } = {}): FacetsState => ({
  status: 200,
  body: {
    objectType: "commerce.PurchaseOrder",
    rows: extra.rows ?? (Object.values(values).reduce((a, b) => a + b, 0) + (extra.otherRows ?? 0)),
    withheldRows: extra.withheldRows ?? 0,
    quarantinedRows: extra.quarantinedRows ?? 0,
    lowerBound: extra.lowerBound ?? false,
    scanLimit: extra.scanLimit ?? 10000,
    facets: [{
      property,
      values: Object.entries(values).map(([value, count]) => ({ value, count })),
      otherRows: extra.otherRows ?? 0,
      nulls: extra.nulls ?? 0,
    }],
  },
})

// --- Contact-sheet specimen rows ---

test("view: every declared state with the cell's count; a state no record is in reads 0 when otherRows is 0", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/counts")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("12")
  await expect(card(view, "rejected").locator("[data-count]")).toHaveText("0")
  await expect(view.getByText("54 records")).toBeVisible()
  await expect(view.locator(".norn-life-arrows > path")).toHaveCount(7)
})

test("validate: past the scan bound counts read as 'at least N'; otherRows marks omitted states Not listed; a refusal shows no counts", async ({ page }) => {
  let view = await specimen(page, "lifecycle-view/lower-bound")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("at least 6,200")
  await expect(card(view, "rejected").locator("[data-count]")).toHaveText("at least 0")
  await expect(view.getByText("at least 10,000 records")).toBeVisible()
  await expect(view.getByText(/Scan limit reached \(10,000 rows\)/)).toBeVisible()
  await expect(view.getByText(/at least 14 withheld from you/)).toBeVisible()
  await expect(view.getByText(/at least 2 held in quarantine/)).toBeVisible()

  view = await specimen(page, "lifecycle-view/other-rows")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("12")
  await expect(card(view, "rejected").locator("[data-count]")).toHaveText("Not listed")
  await expect(card(view, "rejected").locator("[data-count]")).toHaveAttribute("data-unlisted", "")
  await expect(view.getByText(/5 in unlisted values \(omitted states are not counted as 0\)/)).toBeVisible()

  view = await specimen(page, "lifecycle-view/refused")
  await expect(view.getByText("You can't see these records' counts.")).toBeVisible()
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("—")
})

test("live: a new answer on the socket changes the counts in place", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/live")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("11")
  await expect(card(view, "submitted").locator("[data-count]")).toHaveText("8")
})

test("stepper: a counterexample is walked step by step; taken transitions and the object's state follow", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/stepper")
  await expect(view.getByText("Step 3 of 4")).toBeVisible()
  await expect(card(view, "rejected")).toHaveAttribute("data-current", "")
  await expect(view.locator(".norn-life-arrows > path[data-active]")).toHaveCount(2)
  await view.getByRole("button", { name: "Next step" }).click()
  await expect(view.getByText("Step 4 of 4")).toBeVisible()
  await expect(card(view, "draft")).toHaveAttribute("data-current", "")
  await expect(view.getByRole("button", { name: "Next step" })).toBeDisabled()
  for (let step = 0; step < 4; step += 1) await view.getByRole("button", { name: "Previous step" }).click()
  await expect(view.getByText("Step 0 of 4")).toBeVisible()
  await expect(view.locator("[data-current]")).toHaveCount(0)
  // Closing the finding ends the walk.
  await view.getByRole("button", { name: "The page can show “Done” while the purchase order is still “Draft”, and nothing can finish it." }).click()
  await expect(view.getByText(/Step \d of 4/)).toBeHidden()
})

test("technical: the reader's sentence first, the code and the checker's own words under it; steps read as the definition names them", async ({ page }) => {
  let view = await specimen(page, "lifecycle-view/stepper")
  await expect(view.getByText("widget.lifecycle.stuck")).toHaveCount(0)
  await expect(view.locator('[data-step="3"]')).toContainText("Reject")
  view = await specimen(page, "lifecycle-view/technical")
  await expect(view.getByRole("button", { name: /The page can show “Done”/ })).toBeVisible()
  await expect(view.locator(".norn-life-technical")).toContainText("widget.lifecycle.stuck page state `done` with the object `draft`")
})

test("locale: Arabic runs right to left with Arabic digits, mirrored cards, and aligned SVG arrows", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/rtl", "ar")
  await expect(view.locator(".norn-life")).toHaveAttribute("dir", "rtl")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("١٢")
  await expect(view.getByText("الخطوة ٤ من ٤")).toBeVisible()
  await expect(view.getByRole("button", { name: "قد تعرض الصفحة «تم» بينما أمر شراء ما زال «مسودة»، ولا شيء يستطيع إنهاءه." })).toBeVisible()
  // Geometry check in RTL: draft sits to the RIGHT of submitted, and the first arrow starts on draft's left edge.
  const draftBox = await card(view, "draft").boundingBox()
  const submittedBox = await card(view, "submitted").boundingBox()
  const stageBox = await view.locator('[data-region="stage"]').boundingBox()
  expect(draftBox && submittedBox && stageBox).toBeTruthy()
  expect(draftBox!.x).toBeGreaterThan(submittedBox!.x)
  const firstPathD = await view.locator(".norn-life-arrows > path").first().getAttribute("d")
  const match = /^M([\d.-]+)\s+([\d.-]+)/.exec(firstPathD ?? "")
  expect(match).not.toBeNull()
  expect(Number(match![1])).toBeCloseTo(draftBox!.x - stageBox!.x, 0)
})

test("not served: a cell without the facets route says so, never counting forever; Try again asks again", async ({ page }) => {
  const view = await specimen(page, "lifecycle-view/not-served")
  await expect(view.getByText(/This cell does not serve lifecycle counts yet\./)).toBeVisible()
  await expect(view.locator(".norn-life")).toHaveAttribute("data-machine-state", "unavailable")
  await expect(card(view, "draft").locator("[data-count]")).toHaveText("—")
  await view.getByRole("button", { name: "Try again" }).click()
  await expect(view.locator(".norn-life")).toHaveAttribute("data-machine-state", "unavailable")
})

// --- Full Studio Records & Record page rows ---

test.describe("Records page Lifecycle view mode", () => {
  test.beforeEach(({ }, info) => { test.skip(Boolean(process.env.E2E_BASE), `${info.title}: uses the lifecycle stand-in cell`) })

  test("records page: Table | Lifecycle toggle, scoped counts, real WASM checker witness stepping, and live WebSocket refresh", async ({ page }) => {
    const cell = await lifecycleCell(page, {
      status: okFacets("status", { draft: 12, submitted: 7, approved: 4, received: 31 }),
      payment_status: okFacets("payment_status", { unpaid: 19, paid: 35 }),
    })

    // A type without a lifecycle offers no Lifecycle toggle.
    await page.goto("/objects/commerce.Note")
    await expect(page.getByRole("grid", { name: "Note records" })).toBeVisible()
    await expect(page.getByRole("radio", { name: "Lifecycle" })).toHaveCount(0)

    // A type with lifecycles offers Table | Lifecycle; switching opens the workbench Lifecycle panel and reads scoped facets.
    await page.goto("/objects/commerce.PurchaseOrder")
    await expect(page.getByRole("grid", { name: "Purchase order records" })).toBeVisible()
    await page.getByRole("radio", { name: "Lifecycle" }).click()

    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("12")
    await expect(card(statusView, "submitted").locator("[data-count]")).toHaveText("7")
    await expect(card(statusView, "rejected").locator("[data-count]")).toHaveText("0")
    await expect(statusView.getByText("54 records")).toBeVisible()

    // Every facets read carried the workspace scope and lifecycle property.
    expect(cell.facetRequests.some((url) =>
      url.searchParams.get("properties") === "status"
      && url.searchParams.get("orgId") === "org_demo"
      && url.searchParams.get("environmentId") === "analytics")).toBe(true)

    // Real WASM checker runs in-browser and surfaces the real `widget.lifecycle.stuck` witness.
    const stuckButton = statusView.getByRole("button", { name: "The page can show “Done” while the purchase order is still “Draft”, and nothing can finish it." })
    await expect(stuckButton).toBeVisible({ timeout: 20_000 })
    await stuckButton.click()
    await expect(statusView.getByText("Step 0 of 4")).toBeVisible()
    await statusView.getByRole("button", { name: "Next step" }).click()
    await statusView.getByRole("button", { name: "Next step" }).click()
    await expect(statusView.getByText("Step 2 of 4")).toBeVisible()
    await expect(card(statusView, "submitted")).toHaveAttribute("data-current", "")
    await expect(statusView.locator(".norn-life-arrows > path[data-active]")).toHaveCount(1)
    await statusView.getByRole("button", { name: "Next step" }).click()
    await expect(card(statusView, "rejected")).toHaveAttribute("data-current", "")
    await shoot(page, "records-witness")

    // Live WebSocket publish refreshes per-state counts in place without leaving Lifecycle mode.
    cell.publish("status", okFacets("status", { draft: 11, submitted: 8, approved: 4, received: 31 }))
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("11")
    await expect(card(statusView, "submitted").locator("[data-count]")).toHaveText("8")

    // Switching back to Table restores the records grid.
    await page.getByRole("radio", { name: "Table" }).click()
    await expect(page.getByRole("grid", { name: "Purchase order records" })).toBeVisible()
  })

  test("records page: honest lowerBound ('at least N') and otherRows ('Not listed', never 0)", async ({ page }) => {
    await lifecycleCell(page, {
      status: okFacets("status", { draft: 6200, submitted: 1900, received: 1900 }, { lowerBound: true, scanLimit: 10000, withheldRows: 14, quarantinedRows: 2 }),
      payment_status: okFacets("payment_status", { unpaid: 19 }, { otherRows: 5 }),
    })
    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    const views = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view')
    const statusView = views.nth(0)
    const paymentView = views.nth(1)

    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("at least 6,200")
    await expect(card(statusView, "rejected").locator("[data-count]")).toHaveText("at least 0")
    await expect(statusView.getByText("at least 10,000 records")).toBeVisible()
    await expect(statusView.getByText(/Scan limit reached \(10,000 rows\)/)).toBeVisible()

    // With otherRows > 0, omitted declared states must never display 0.
    await expect(card(paymentView, "unpaid").locator("[data-count]")).toHaveText("19")
    await expect(card(paymentView, "paid").locator("[data-count]")).toHaveText("Not listed")
    await expect(card(paymentView, "refunded").locator("[data-count]")).toHaveText("Not listed")
    await expect(paymentView.getByText(/5 in unlisted values \(omitted states are not counted as 0\)/)).toBeVisible()
    await shoot(page, "records-lower-bound-and-other")
  })

  test("records page: Arabic RTL layout on the real Records page keeps arrows aligned with cards", async ({ page }) => {
    await page.addInitScript(() => { window.localStorage.setItem("norn.studio.locale", "ar") })
    await lifecycleCell(page, {
      status: okFacets("status", { draft: 6200, submitted: 1900, received: 1900 }, { lowerBound: true, scanLimit: 10000 }),
      payment_status: okFacets("payment_status", { unpaid: 10, paid: 20 }),
    })
    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    await expect(page.locator('[data-slot="tabs"]')).toHaveAttribute("dir", "rtl")
    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(statusView.locator(".norn-life")).toHaveAttribute("dir", "rtl")
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("٦٬٢٠٠ على الأقل")
    await expect(statusView.getByText("١٠٬٠٠٠ سجل على الأقل")).toBeVisible()
    // Wait for the WASM checker findings to settle before measuring card/arrow geometry and taking the screenshot.
    await expect(statusView.getByRole("button", { name: "قد تعرض الصفحة «Done» بينما أمر شراء ما زال «مسودة»، ولا شيء يستطيع إنهاءه." })).toBeVisible({ timeout: 20_000 })
    await expect.poll(async () => {
      const draftBox = await card(statusView, "draft").boundingBox()
      const submittedBox = await card(statusView, "submitted").boundingBox()
      const stageBox = await statusView.locator('[data-region="stage"]').boundingBox()
      const firstPathD = await statusView.locator(".norn-life-arrows > path").first().getAttribute("d")
      const match = /^M([\d.-]+)\s+([\d.-]+)/.exec(firstPathD ?? "")
      if (!draftBox || !submittedBox || !stageBox || !match) return false
      return draftBox.x > submittedBox.x && Math.abs(Number(match[1]) - (draftBox.x - stageBox.x)) < 1
    }).toBe(true)
    await shoot(page, "records-rtl")
  })

  test("record page: LifecycleStepper shares labels, roles and ending styling and links to the type-wide Lifecycle view", async ({ page }) => {
    await lifecycleCell(page, {
      status: okFacets("status", { draft: 12, submitted: 7, approved: 4, received: 31 }),
      payment_status: okFacets("payment_status", { unpaid: 19, paid: 35 }),
    })
    await page.goto("/sources/commerce.PurchaseOrder/PO-101")
    const statusPanel = page.locator('[data-slot="panel"]').filter({ hasText: "Lifecycle · Status" })
    await expect(statusPanel).toBeVisible()
    await expect(statusPanel.locator('[data-state="submitted"]')).toHaveAttribute("aria-current", "step")
    await expect(statusPanel.locator('[data-state="draft"]')).toContainText("start")
    await expect(statusPanel.locator('[data-state="received"]')).toHaveAttribute("data-ending", "")
    await expect(statusPanel.locator('[data-state="received"]')).toContainText("ending")
    await shoot(page, "record-stepper")

    await statusPanel.getByRole("link", { name: "Type-wide view" }).click()
    await expect(page).toHaveURL(/\/objects\/commerce\.PurchaseOrder#lifecycle$/)
    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("12")
  })

  test("records page: a 409 object.snapshot_moved retries once and recovers instead of hanging", async ({ page }) => {
    let statusReads = 0
    await lifecycleCell(page, {
      status: okFacets("status", { draft: 15, submitted: 5 }),
      payment_status: okFacets("payment_status", { unpaid: 20 }),
    })
    await page.route("**/api/host/v1/objects/commerce.PurchaseOrder:facets**", (route: Route) => {
      const url = new URL(route.request().url())
      if (url.searchParams.get("properties") === "status" && statusReads++ === 0) {
        return route.fulfill({ status: 409, json: { code: "object.snapshot_moved", message: "Snapshot moved" } })
      }
      return route.fallback()
    })
    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("15")
    await expect(statusView.locator(".norn-life")).toHaveAttribute("data-machine-state", "showing")
    expect(statusReads).toBeGreaterThanOrEqual(2)
  })

  test("records page: newer first socket snapshot arriving before older initial GET reconciles to newest count", async ({ page }) => {
    let resolveSocketSent!: () => void
    const socketSent = new Promise<void>((resolve) => { resolveSocketSent = resolve })
    let statusGetCalls = 0
    await lifecycleCell(
      page,
      {
        status: okFacets("status", { draft: 2, submitted: 0 }),
        payment_status: okFacets("payment_status", { unpaid: 2 }),
      },
      {
        includeWidget: false,
        onFacetsSubscribe: (sub) => {
          sub.sendSnapshot()
          if (sub.property === "status") resolveSocketSent()
        },
      },
    )
    await page.route("**/api/host/v1/objects/commerce.PurchaseOrder:facets**", async (route: Route) => {
      const url = new URL(route.request().url())
      if (url.searchParams.get("properties") === "status") {
        statusGetCalls += 1
        if (statusGetCalls === 1) {
          // Hold the stale initial GET (draft=1) until after the newer WebSocket snapshot (draft=2) is delivered on the page's event loop.
          await socketSent
          await page.evaluate(() => new Promise<void>((resolve) => queueMicrotask(resolve)))
          const stale = okFacets("status", { draft: 1, submitted: 0 })
          return route.fulfill({ status: stale.status, json: stale.body })
        }
      }
      return route.fallback()
    })
    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("2")
    expect(statusGetCalls).toBe(2)
  })

  test("records page: older initial GET finishing before newer first socket snapshot reconciles to newest count", async ({ page }) => {
    let sendHeldSnapshot: (() => void) | null = null
    let statusGetCalls = 0
    const cell = await lifecycleCell(
      page,
      {
        status: okFacets("status", { draft: 1, submitted: 0 }),
        payment_status: okFacets("payment_status", { unpaid: 1 }),
      },
      {
        includeWidget: false,
        onFacetsSubscribe: (sub) => {
          if (sub.property === "status") {
            sendHeldSnapshot = sub.sendSnapshot
          } else {
            sub.sendSnapshot()
          }
        },
      },
    )
    await page.route("**/api/host/v1/objects/commerce.PurchaseOrder:facets**", (route: Route) => {
      const url = new URL(route.request().url())
      if (url.searchParams.get("properties") === "status") statusGetCalls += 1
      return route.fallback()
    })
    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("1")
    await expect.poll(() => sendHeldSnapshot !== null).toBe(true)
    cell.setFacets("status", okFacets("status", { draft: 2, submitted: 0 }))
    sendHeldSnapshot!()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("2")
    expect(statusGetCalls).toBe(2)
  })

  test("records page: filter paths past the 512-byte live limit surface live-updates-off and support manual Refresh", async ({ page }) => {
    const cell = await lifecycleCell(page, {
      status: okFacets("status", { draft: 4, submitted: 2 }),
      payment_status: okFacets("payment_status", { unpaid: 6 }),
    })
    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    const statusView = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("4")
    await expect(statusView.locator('[data-region="live-off"]')).toBeHidden()

    // Apply 6 short equality clauses via the real Filter editor (produces a >512-byte scoped live path).
    await page.getByRole("button", { name: /^Filter/ }).click()
    for (let i = 1; i < 6; i += 1) await page.getByRole("button", { name: "Add condition" }).click()
    for (let i = 1; i <= 6; i += 1) {
      await page.getByLabel(`Condition ${i} value`).fill(`PO-10${i}`)
    }
    await page.getByRole("button", { name: "Apply" }).click()

    await expect.poll(() => cell.refusedPaths.some((p) => p.length > 512 && p.includes("properties=status"))).toBe(true)
    await expect(statusView.locator(".norn-life")).toHaveAttribute("data-live-updates", "off")
    await expect(statusView.locator(".norn-life")).toHaveAttribute("data-live-refusal-code", "live.not_subscribable")
    await expect(statusView.locator('[data-region="live-off"]')).toBeVisible()
    await expect(statusView.locator('[data-region="live-off"]')).toContainText("Live updates are off; counts show the last read.")
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("4")
    await shoot(page, "records-live-off")

    // Cell changes while live updates are off; clicking Refresh reads the latest counts.
    cell.setFacets("status", okFacets("status", { draft: 9, submitted: 3 }))
    await statusView.getByRole("button", { name: "Refresh" }).click()
    await expect(card(statusView, "draft").locator("[data-count]")).toHaveText("9")
    await expect(card(statusView, "submitted").locator("[data-count]")).toHaveText("3")
  })

  test("records page: recovery from live subscription capacity refreshes counts when the digest repeats", async ({ page }) => {
    const original = okFacets("status", { draft: 1 })
    const statusSubscriptions: FacetsSubscription[] = []
    const cell = await lifecycleCell(page, { status: okFacets("status", { draft: 0 }) }, {
      includeWidget: false,
      onFacetsSubscribe: (subscription) => {
        const { id, property, socket, sendSnapshot } = subscription
        if (property !== "status") return sendSnapshot()
        statusSubscriptions.push(subscription)
        if (statusSubscriptions.length === 2) {
          socket.send(JSON.stringify({ type: "refused", id, code: "live.too_many_subscriptions", message: "this socket's subscriptions are full" }))
        } else if (statusSubscriptions.length > 2) {
          sendSnapshot("original")
        }
      },
    })

    await page.goto("/objects/commerce.PurchaseOrder#lifecycle")
    const view = page.locator('[data-testid="lifecycle-view"] norn-lifecycle-view').first()
    const count = card(view, "draft").locator("[data-count]")
    const banner = view.locator('[data-region="live-off"]')
    await expect(count).toHaveText("0")
    await expect.poll(() => statusSubscriptions.length).toBe(1)
    cell.setFacets("status", original)
    statusSubscriptions[0]!.sendSnapshot("original")
    await expect(count).toHaveText("1")
    await statusSubscriptions[0]!.socket.close({ code: 4401 })
    await expect(banner).toBeVisible()

    cell.setFacets("status", okFacets("status", { draft: 2 }))
    await view.getByRole("button", { name: "Refresh" }).click()
    await expect(count).toHaveText("2")
    await shoot(page, "records-live-capacity-refused")

    cell.setFacets("status", original)
    await statusSubscriptions[1]!.socket.close({ code: 4401 })
    await expect(banner).toBeHidden()
    await expect(count).toHaveText("1")
    await shoot(page, "records-live-capacity-recovered")
  })

  test("unattributed findings render without a witness stepper and never highlight states", async ({ page }) => {
    const view = await specimen(page, "lifecycle-view/counts")
    await expect(card(view, "draft").locator("[data-count]")).toHaveText("12")
    await view.evaluate((element) => {
      const host = element as HTMLElement & { options: Record<string, unknown> }
      host.options = {
        ...host.options,
        findings: [{
          code: "widget.lifecycle.stuck",
          message: "page state `done` with the object `draft`",
          witness: null,
          unattributed: true,
        }],
      }
    })
    await expect(view.locator("[data-finding-unattributed]")).toHaveCount(1)
    await expect(view.locator("[data-finding-unattributed]")).toContainText("Unattributed finding")
    await expect(view.locator("[data-pick]")).toHaveCount(0)
    await expect(view.locator('[data-region="stepper"]')).toHaveCount(0)
    await expect(view.locator("[data-current]")).toHaveCount(0)
  })
})
