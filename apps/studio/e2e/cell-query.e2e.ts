import { expect, test, type Page, type Route, type WebSocketRoute } from "@playwright/test"

// #1374 phase 1a: one record read through `useCellQuery` (harness.html), against a stand-in cell
// that speaks `norn.live.v1` as `crates/norn-api/src/live.rs` types it. A push writes the cell's
// document into the cache: the screen changes, exact decimals kept, and nothing is read again.

const PATH = "/v1/objects/demo.Item/i1?orgId=org_demo&environmentId=analytics"
// 28 significant digits: a double keeps 15 to 17.
const FIRST = "1234567890123456.789012345678"
const SECOND = "9876543210987654.321098765400"
const record = (version: number, amount: string, id = "i1") =>
  `{"objectType":"demo.Item","id":"${id}","scope":"org_demo/analytics","version":${version},"values":{"amount":${amount},"label":"first"},"withheld":false,"withheldProperties":[]}`
/** The record a read path names (`/v1/objects/demo.Item/<id>?…`). */
const idOf = (path: string): string => decodeURIComponent(/^\/v1\/objects\/[^/]+\/([^?]+)/.exec(path)?.[1] ?? "")

/** A frame the page sent on one of its sockets. */
interface Frame { readonly type: string; readonly id: string; readonly path?: string }

interface StandIn {
  readonly reads: string[]
  readonly subscribed: Promise<{ readonly socket: WebSocketRoute; readonly id: string; readonly path: string }>
  /** Every frame the page sent, on any socket, in order. */
  readonly frames: Frame[]
  /** The socket of each subscription id. */
  readonly sockets: Map<string, WebSocketRoute>
}

const ME = { user: { id: "u_e2e", name: "Reader", email: "reader@example.com" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" }

/** The cell's answer to one record read: the record by default. */
type ObjectAnswer = (route: Route, path: string, count: number) => Promise<void>
const answerRecord: ObjectAnswer = (route, path) => route.fulfill({ status: 200, contentType: "application/json", body: record(1, FIRST, idOf(path)) })

/** The stand-in cell; `me` answers the n-th scope discovery (`/api/me`), `objects` the n-th record read. */
async function standIn(page: Page, me: (route: Route, count: number) => Promise<void> = (route) => route.fulfill({ json: ME }), objects: ObjectAnswer = answerRecord): Promise<StandIn> {
  const reads: string[] = []
  const frames: Frame[] = []
  const sockets = new Map<string, WebSocketRoute>()
  let asked = 0
  await page.route("**/api/me", (route) => me(route, ++asked))
  await page.route("**/api/host/v1/objects/**", (route) => {
    const url = new URL(route.request().url())
    const path = `${url.pathname.replace(/^\/api\/host/, "")}${url.search}`
    reads.push(path)
    return objects(route, path, reads.length)
  })
  let subscribe: (subscription: { socket: WebSocketRoute; id: string; path: string }) => void = () => {}
  const subscribed = new Promise<{ socket: WebSocketRoute; id: string; path: string }>((resolve) => { subscribe = resolve })
  // Routed before the page loads, so every socket the page opens reaches this stand-in.
  await page.routeWebSocket("**/api/host/v1/subscribe**", (socket) => {
    socket.onMessage((raw) => {
      const message = JSON.parse(String(raw)) as { type: string; id: string; path: string }
      frames.push(message)
      if (message.type !== "subscribe") return
      sockets.set(message.id, socket)
      // The cell answers a new subscription with the GET's own answer.
      socket.send(`{"type":"snapshot","id":"${message.id}","status":200,"digest":"d1","body":${record(1, FIRST, idOf(message.path))}}`)
      subscribe({ socket, id: message.id, path: message.path })
    })
  })
  return { reads, subscribed, frames, sockets }
}

for (const scheme of ["light", "dark"] as const) {
  test(`cell query (${scheme}): a live push updates the record in place, exact decimals kept, without reading again`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme })
    const cell = await standIn(page)
    await page.goto("/harness.html?type=demo.Item&id=i1")
    const view = page.getByTestId("record-read")
    await expect(view.getByTestId("record-version")).toHaveText("1")
    await expect(view.getByTestId("record-value-amount")).toHaveText(FIRST)
    const { socket, id, path } = await cell.subscribed
    expect(path).toBe(PATH)
    await page.screenshot({ path: `test-results/cell-query/read-${scheme}.png` })

    socket.send(`{"type":"snapshot","id":"${id}","status":200,"digest":"d2","body":${record(2, SECOND)}}`)
    await expect(view.getByTestId("record-version")).toHaveText("2")
    await expect(view.getByTestId("record-value-amount")).toHaveText(SECOND)
    await expect(view.getByTestId("record-source")).toHaveText("pushed")
    expect(cell.reads).toEqual([PATH])
    await page.screenshot({ path: `test-results/cell-query/pushed-${scheme}.png` })
  })
}

test("cell query: a pushed refusal shows the refusal state in place of the record", async ({ page }) => {
  const cell = await standIn(page)
  await page.goto("/harness.html?type=demo.Item&id=i1")
  const view = page.getByTestId("record-read")
  await expect(view.getByTestId("record-version")).toHaveText("1")
  const { socket, id } = await cell.subscribed
  socket.send(`{"type":"snapshot","id":"${id}","status":404,"digest":"gone","body":{"format":"norn/refusal@2","retryable":false,"reason":null,"code":"object.not_found","category":"visibility","message":"not found","explanation":"diagnostic detail is unavailable under the caller's disclosure authority"}}`)
  await expect(view.locator('[data-load-state="error"]')).toBeVisible()
  await expect(view.getByText("object.not_found")).toBeAttached()
  await expect(view.getByTestId("record-version")).toHaveCount(0)
  expect(cell.reads).toHaveLength(1)
  await page.screenshot({ path: "test-results/cell-query/refused-light.png" })
})

test("cell query: a read whose subscription the cell refuses says it is not followed, and reads again on request", async ({ page }) => {
  const cell = await standIn(page)
  await page.goto("/harness.html?type=demo.Item&id=i1")
  const view = page.getByTestId("record-read")
  await expect(view.getByTestId("record-updates")).toHaveText("live")
  const { socket, id } = await cell.subscribed
  socket.send(`{"type":"refused","id":"${id}","code":"live.too_many_subscriptions","message":"at most 8 subscriptions per socket"}`)
  const updates = view.getByTestId("record-updates")
  await expect(updates).toHaveAttribute("data-stale", "")
  await expect(updates).toContainText("Not followed: the last answer read, as of")
  // The record stays on screen as the last answer, never as live.
  await expect(view.getByTestId("record-value-amount")).toHaveText(FIRST)
  await updates.getByRole("button", { name: "Read again" }).click()
  await expect.poll(() => cell.reads.length).toBe(2)
  await page.screenshot({ path: "test-results/cell-query/stale-light.png" })
})

test("cell query: a workspace with no data cell says so, and a failed scope discovery is Workspace unavailable with Retry", async ({ page }) => {
  await standIn(page, (route) => route.fulfill({ json: { ...ME, organization: null, environmentId: null } }))
  await page.goto("/harness.html?type=demo.Item&id=i1")
  await expect(page.getByTestId("record-read").getByText("No data cell", { exact: true })).toBeVisible()

  const second = await page.context().newPage()
  const cell = await standIn(second, (route, count) => count === 1 ? route.fulfill({ status: 503, json: { message: "the host is restarting" } }) : route.fulfill({ json: ME }))
  await second.goto("/harness.html?type=demo.Item&id=i1")
  const view = second.getByTestId("record-read")
  const unavailable = view.locator("[data-workspace-unavailable]")
  await expect(unavailable).toBeVisible()
  expect(cell.reads).toHaveLength(0)
  await unavailable.getByRole("button", { name: "Retry" }).click()
  await expect(view.getByTestId("record-version")).toHaveText("1")
  await second.screenshot({ path: "test-results/cell-query/scope-retried-light.png" })
})

const NEXT = "/v1/objects/demo.Item/i2?orgId=org_demo&environmentId=analytics"

test("cell query: the hook unsubscribes what it no longer shows, and a live refusal belongs to the read it refused", async ({ page }) => {
  const cell = await standIn(page)
  await page.goto("/harness.html?type=demo.Item&id=i1&next=i2")
  const view = page.getByTestId("record-read")
  const shown = view.getByTestId("record")
  await expect(shown).toHaveAttribute("data-record-id", "i1")
  const { socket, id } = await cell.subscribed
  // The cell refuses the first read's subscription: that read is not followed.
  socket.send(`{"type":"refused","id":"${id}","code":"live.too_many_subscriptions","message":"at most 8 subscriptions per socket"}`)
  await expect(shown).toHaveAttribute("data-live-refusal-code", "live.too_many_subscriptions")
  await expect(view.getByTestId("record-updates")).toHaveAttribute("data-stale", "")

  // The same mounted view reads another record: the first subscription is ended, the second made,
  // and the refusal, which was the first read's, is gone.
  await view.getByRole("button", { name: "Next record" }).click()
  await expect(shown).toHaveAttribute("data-record-id", "i2")
  await expect.poll(() => cell.frames.find((frame) => frame.type === "subscribe" && frame.path === NEXT)?.id).toBeDefined()
  expect(cell.frames).toContainEqual({ type: "unsubscribe", id })
  await expect(shown).not.toHaveAttribute("data-live-refusal-code")
  await expect(view.getByTestId("record-updates")).toHaveText("live")
  await expect(view.getByTestId("record-version")).toHaveText("1")
  const next = cell.frames.find((frame) => frame.type === "subscribe" && frame.path === NEXT)?.id ?? ""

  // It still follows the cell: a push for the second record lands.
  cell.sockets.get(next)?.send(`{"type":"snapshot","id":"${next}","status":200,"digest":"d2","body":${record(2, SECOND, "i2")}}`)
  await expect(view.getByTestId("record-version")).toHaveText("2")
  await expect(view.getByTestId("record-value-amount")).toHaveText(SECOND)

  // Closing the record unmounts the read: its subscription is ended, and nothing is read again.
  await view.getByRole("button", { name: "Close record" }).click()
  await expect(shown).toHaveCount(0)
  await expect.poll(() => cell.frames.filter((frame) => frame.type === "unsubscribe").map((frame) => frame.id)).toEqual([id, next])
  expect(cell.reads).toEqual([PATH, NEXT])
})

for (const scheme of ["light", "dark"] as const) {
  test(`cell query (${scheme}): a read a gateway answers 503 is Workspace unavailable with Retry, never "not served", and Retry reads it`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme })
    let up = false
    // A proxy's HTML 503 while the host restarts: no refusal, and not the deployment's no-host answer.
    const cell = await standIn(page, undefined, (route, path) => up
      ? answerRecord(route, path, 0)
      : route.fulfill({ status: 503, contentType: "text/html", body: "<html><body><h1>503 Service Unavailable</h1></body></html>" }))
    await page.goto("/harness.html?type=demo.Item&id=i1")
    const view = page.getByTestId("record-read")
    const unavailable = view.locator("[data-workspace-unavailable]")
    // A read the cell did not answer is asked again, as Studio's reads are: three retries, then the state.
    await expect(unavailable).toBeVisible({ timeout: 20_000 })
    await expect(unavailable).toHaveAttribute("data-retryable", "true")
    await expect(unavailable.getByText("Workspace unavailable", { exact: true })).toBeVisible()
    await expect(view.locator('[data-load-state="not-served"]')).toHaveCount(0)
    expect(cell.reads).toEqual([PATH, PATH, PATH, PATH])
    await page.screenshot({ path: `test-results/cell-query/unavailable-${scheme}.png` })

    up = true
    await unavailable.getByRole("button", { name: "Retry" }).click()
    await expect(view.getByTestId("record-version")).toHaveText("1")
    await expect(view.getByTestId("record-value-amount")).toHaveText(FIRST)
    await expect(unavailable).toHaveCount(0)
    expect(cell.reads).toHaveLength(5)
    await page.screenshot({ path: `test-results/cell-query/unavailable-retried-${scheme}.png` })
  })
}
