import { expect, test, type Page, type WebSocketRoute } from "@playwright/test"

// #1374 phase 1a: one record read through `useCellQuery` (harness.html), against a stand-in cell
// that speaks `norn.live.v1` as `crates/norn-api/src/live.rs` types it. A push writes the cell's
// document into the cache: the screen changes, exact decimals kept, and nothing is read again.

const PATH = "/v1/objects/demo.Item/i1?orgId=org_demo&environmentId=analytics"
// 28 significant digits: a double keeps 15 to 17.
const FIRST = "1234567890123456.789012345678"
const SECOND = "9876543210987654.321098765400"
const record = (version: number, amount: string) =>
  `{"objectType":"demo.Item","id":"i1","scope":"org_demo/analytics","version":${version},"values":{"amount":${amount},"label":"first"},"withheld":false,"withheldProperties":[]}`

interface StandIn {
  readonly reads: string[]
  readonly subscribed: Promise<{ readonly socket: WebSocketRoute; readonly id: string; readonly path: string }>
}

async function standIn(page: Page): Promise<StandIn> {
  const reads: string[] = []
  await page.route("**/api/me", (route) => route.fulfill({ json: { user: { id: "u_e2e", name: "Reader", email: "reader@example.com" }, organization: { id: "org_demo", name: "Demo", role: "member" }, environmentId: "analytics" } }))
  await page.route("**/api/host/v1/objects/**", (route) => {
    const url = new URL(route.request().url())
    reads.push(`${url.pathname.replace(/^\/api\/host/, "")}${url.search}`)
    return route.fulfill({ status: 200, contentType: "application/json", body: record(1, FIRST) })
  })
  let subscribe: (subscription: { socket: WebSocketRoute; id: string; path: string }) => void = () => {}
  const subscribed = new Promise<{ socket: WebSocketRoute; id: string; path: string }>((resolve) => { subscribe = resolve })
  // Routed before the page loads, so the page's one socket always reaches this stand-in.
  await page.routeWebSocket("**/api/host/v1/subscribe**", (socket) => {
    socket.onMessage((raw) => {
      const message = JSON.parse(String(raw)) as { type: string; id: string; path: string }
      if (message.type !== "subscribe") return
      // The cell answers a new subscription with the GET's own answer.
      socket.send(`{"type":"snapshot","id":"${message.id}","status":200,"digest":"d1","body":${record(1, FIRST)}}`)
      subscribe({ socket, id: message.id, path: message.path })
    })
  })
  return { reads, subscribed }
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
