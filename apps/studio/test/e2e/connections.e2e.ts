// One end-to-end test per object kind on Connections, in a real browser against a deployment.
// Each test records the verbs it exercised; a ✓ verb of the matrix left unexercised fails the run.
// Usage (on Boat only): E2E_BASE=https://next.norn.cloud E2E_EMAIL=… E2E_PASSWORD=… (an org admin) \
//   E2E_GATEWAY_EMAIL=… E2E_GATEWAY_PASSWORD=… E2E_APP=… E2E_APP_KEY=… E2E_TOOL=… E2E_TOOL_FIELD=… bun test/e2e/connections.e2e.ts
// Credentials come from Infisical by name into the environment; nothing is printed or written.
import { MATRIX, type VerbSupport } from "../../src/lib/connectors.ts"

type Page = {
  goto(url: string): Promise<unknown>
  url(): string
  waitForURL(url: string | RegExp, options?: { timeout?: number }): Promise<void>
  getByRole(role: string, options?: { name?: string | RegExp; exact?: boolean }): Locator
  getByLabel(text: string | RegExp, options?: { exact?: boolean }): Locator
  getByText(text: string | RegExp): Locator
  locator(selector: string): Locator
  on(event: "dialog", listener: (dialog: { accept(): Promise<void> }) => void): void
  route(url: string, handler: (route: { fulfill(response: { status: number; contentType: string; body: string }): Promise<void> }) => unknown): Promise<void>
  unroute(url: string): Promise<void>
}
type Locator = { click(): Promise<void>; press(key: string): Promise<void>; fill(value: string): Promise<void>; check(): Promise<void>; selectOption(value: string): Promise<unknown>; getByLabel: Page["getByLabel"]; getByText: Page["getByText"]; first(): Locator; waitFor(o?: { timeout?: number; state?: string }): Promise<void>; count(): Promise<number>; textContent(): Promise<string | null>; getByRole: Page["getByRole"] }

const env = (name: string) => {
  const value = process.env[name]
  if (value === undefined || value === "") throw new Error(`${name} is required`)
  return value
}
const base = env("E2E_BASE")
if (!/^https:\/\/[a-z0-9.-]+$/.test(base)) throw new Error("E2E_BASE must be an https origin")

async function signIn(page: Page) {
  await page.goto(`${base}/`)
  await page.getByLabel("Email").fill(env("E2E_EMAIL"))
  await page.getByLabel("Password").fill(env("E2E_PASSWORD"))
  await page.getByLabel("Password").press("Enter")
  await page.getByRole("link", { name: /connections/i }).first().waitFor()
}

/**
 * The gateway's own pages (Executor 2.0.0-beta.37): its sign-in at /login when it has no session,
 * then its consent at /mcp/authorize, whose Connect button returns to Studio. Credentials are
 * typed there, never into Norn.
 */
async function gatewaySignIn(page: Page) {
  await page.waitForURL(/gateway-next\.norn\.cloud\/(login|mcp\/authorize)/, { timeout: 30_000 })
  if (/\/login/.test(page.url())) {
    await page.getByLabel("Email").fill(env("E2E_GATEWAY_EMAIL"))
    await page.getByLabel("Password").fill(env("E2E_GATEWAY_PASSWORD"))
    await page.getByLabel("Password").press("Enter")
    await page.waitForURL(/\/mcp\/authorize/, { timeout: 30_000 })
  }
  await page.getByRole("button", { name: "Connect", exact: true }).click()
  await page.waitForURL(`${base}/connections`, { timeout: 30_000 })
}

/** The gateway's own connection form for the test app's API-key account, then back to Connections. */
async function gatewayAccountForm(page: Page) {
  await page.waitForURL(/gateway-next\.norn\.cloud\//, { timeout: 30_000 })
  await page.getByLabel(/api key|token/i).first().fill(env("E2E_APP_KEY"))
  await page.getByRole("button", { name: /^(connect|save|continue)$/i }).first().click()
  await page.waitForURL(/gateway-next\.norn\.cloud\/.*(done|complete|success)|next\.norn\.cloud\/connections/, { timeout: 30_000 })
}

/** The test app's account is connected, connecting it through the gateway's form when it is not. */
export async function ensureAccount(page: Page) {
  await open(page)
  const section = page.getByRole("region", { name: app() })
  await section.waitFor()
  const connect = section.getByRole("button", { name: "Connect", exact: true })
  if (await connect.count() > 0) { await connect.first().click(); await gatewayAccountForm(page); await open(page) }
  await section.getByRole("button", { name: "Reconnect", exact: true }).first().waitFor()
}

const open = async (page: Page) => { await page.goto(`${base}/connections`); await page.getByRole("heading", { name: "Connections" }).waitFor() }
const app = () => env("E2E_APP")

type Spec = (page: Page, did: (verb: string) => void) => Promise<void>
const SPECS: Record<keyof typeof MATRIX, Spec> = {
  "Gateway sign-in": async (page, did) => {
    await open(page)
    const disconnect = page.getByRole("button", { name: "Disconnect", exact: true }).first()
    if (await disconnect.count() > 0) await disconnect.click()
    await page.getByRole("button", { name: "Sign in to the gateway", exact: true }).click()
    await gatewaySignIn(page); did("connect")
    await page.getByText("Signed in").waitFor(); did("view")
    await page.getByRole("button", { name: "Reconnect" }).first().click()
    await gatewaySignIn(page); await page.getByText("Signed in").waitFor(); did("reconnect")
    await page.getByRole("button", { name: "Disconnect" }).first().click()
    await page.getByRole("button", { name: "Sign in to the gateway", exact: true }).waitFor(); did("disconnect")
    // Leave the gateway signed in for the next kinds.
    await page.getByRole("button", { name: "Sign in to the gateway", exact: true }).click(); await gatewaySignIn(page)
  },
  "App account": async (page, did) => {
    page.on("dialog", (dialog) => void dialog.accept())
    await open(page)
    const section = page.getByRole("region", { name: app() })
    await section.waitFor()
    const connect = section.getByRole("button", { name: "Connect", exact: true })
    if (await connect.count() === 0) { await section.getByRole("button", { name: "Disconnect", exact: true }).first().click(); await connect.first().waitFor() }
    await connect.first().click()
    await gatewayAccountForm(page)
    await open(page); await section.getByRole("button", { name: "Reconnect", exact: true }).first().waitFor(); did("connect")
    await section.getByRole("button", { name: "View" }).first().click()
    await section.getByText("Provider").waitFor(); did("view")
    await section.getByRole("button", { name: "Rename" }).first().click()
    const name = `e2e ${Date.now()}`
    await page.getByLabel("Account name").fill(name)
    await section.getByRole("button", { name: "Save" }).click()
    await section.getByText(name).waitFor(); did("rename")
    await section.getByRole("button", { name: "Reconnect", exact: true }).first().click()
    await gatewayAccountForm(page)
    await open(page); await section.getByText(name).waitFor(); did("reconnect")
    await section.getByRole("button", { name: "Disconnect", exact: true }).first().click()
    await section.getByRole("button", { name: "Connect", exact: true }).waitFor(); did("disconnect")
  },
  "Connector error": async (page, did) => {
    // The page receives a typed refusal (an expired gateway grant on its inventory read), shows it
    // with its code, and its fix button walks the gateway sign-in back to Connections.
    const inventory = `${base}/api/connectors/inventory`
    await page.route(inventory, (route) => route.fulfill({ status: 401, contentType: "application/json", body: JSON.stringify({ code: "connectors.reauth", error: "Your gateway sign-in has expired. Sign in to the gateway again.", action: { kind: "sign_in_gateway" } }) }))
    await open(page)
    // Both the apps panel and the bindings panel read the inventory, so the refusal shows in each.
    const alert = page.locator('[role="alert"][data-code="connectors.reauth"]').first()
    await alert.waitFor(); did("show")
    await page.unroute(inventory)
    await alert.getByRole("button", { name: "Sign in to the gateway again" }).click()
    await gatewaySignIn(page)
    await page.getByText("Signed in").waitFor()
    await page.getByRole("region", { name: app() }).waitFor(); did("fix")
  },
  "Tool binding": async (page, did) => {
    // E2E_TOOL is a tool of E2E_APP with a string input E2E_TOOL_FIELD; the signed-in person is an admin.
    page.on("dialog", (dialog) => void dialog.accept())
    // The gateway lists an app's tools only for a caller with its account connected.
    await ensureAccount(page)
    const tool = env("E2E_TOOL"), field = env("E2E_TOOL_FIELD"), name = `e2e binding ${Date.now()}`
    await page.getByLabel("App").selectOption(app())
    await page.getByRole("button", { name: `Bind ${tool}` }).click()
    const editor = page.getByRole("form", { name: `Bind ${tool}` })
    await editor.getByLabel("Binding name").fill(name)
    await editor.getByLabel(`Fix ${field}`).check()
    await editor.getByLabel(field).fill("fixed-by-e2e")
    await editor.getByRole("button", { name: "Add binding" }).click()
    const row = page.locator(`[data-binding]:has-text("${name}")`)
    await row.waitFor(); did("add")
    await row.getByRole("button", { name: "View" }).click()
    await row.getByText(`${field} = fixed-by-e2e`).waitFor(); did("view")
    await row.getByRole("button", { name: "Edit" }).click()
    await page.getByRole("form", { name: `Edit ${name}` }).getByLabel("Who may run it").selectOption("admin")
    await page.getByRole("form", { name: `Edit ${name}` }).getByRole("button", { name: "Save binding" }).click()
    await row.getByText("Admins only").waitFor(); did("edit")
    await row.getByRole("button", { name: "Test run" }).click()
    await page.getByRole("form", { name: `Test run ${name}` }).getByRole("button", { name: "Run" }).click()
    await row.getByLabel("Result").waitFor({ timeout: 30_000 }); did("test-run")
    await row.getByRole("button", { name: "Remove" }).click()
    await row.waitFor({ state: "detached" }); did("remove")
  },
}

const supported = (verbs: Readonly<Record<string, VerbSupport>>) => Object.entries(verbs).filter(([, support]) => support === "supported").map(([verb]) => verb)
const { chromium } = await import("playwright-core" as string) as { chromium: { launch(o: object): Promise<{ newContext(o: object): Promise<{ newPage(): Promise<unknown>; close(): Promise<void> }>; close(): Promise<void> }> } }
const browser = await chromium.launch({ executablePath: process.env.CHROME ?? "/usr/bin/google-chrome", args: ["--no-sandbox"] })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 } })
const page = await context.newPage() as Page
await signIn(page)
let failed = 0
for (const [kind, verbs] of Object.entries(MATRIX) as [keyof typeof MATRIX, Readonly<Record<string, VerbSupport>>][]) {
  const done = new Set<string>()
  const started = Date.now()
  try {
    await SPECS[kind](page, (verb) => done.add(verb))
    const missing = supported(verbs).filter((verb) => !done.has(verb))
    if (missing.length > 0) throw new Error(`verbs not exercised: ${missing.join(", ")}`)
    console.log(`PASS ${kind}: ${[...done].join(", ")} (${Date.now() - started}ms)`)
  } catch (error) {
    failed += 1
    console.log(`FAIL ${kind} after [${[...done].join(", ")}]: ${error instanceof Error ? error.message.split("\n")[0] : String(error)}`)
  }
}
await context.close()
await browser.close()
console.log(failed === 0 ? "every kind passes" : `${failed} kind(s) failed`)
process.exit(failed === 0 ? 0 : 1)
