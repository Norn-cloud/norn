// One end-to-end test per object kind on Connections, in a real browser against a deployment.
// Each test records the verbs it exercised; a ✓ verb of the matrix left unexercised fails the run.
// Usage (on Boat only): E2E_BASE=https://next.norn.cloud E2E_EMAIL=… E2E_PASSWORD=… \
//   E2E_GATEWAY_EMAIL=… E2E_GATEWAY_PASSWORD=… E2E_APP=… bun test/e2e/connections.e2e.ts
// Credentials come from Infisical by name into the environment; nothing is printed or written.
import { MATRIX, type VerbSupport } from "../../src/lib/connectors.ts"

type Page = {
  goto(url: string): Promise<unknown>
  url(): string
  waitForURL(url: string | RegExp, options?: { timeout?: number }): Promise<void>
  getByRole(role: string, options?: { name?: string | RegExp; exact?: boolean }): Locator
  getByLabel(text: string | RegExp): Locator
  getByText(text: string | RegExp): Locator
  locator(selector: string): Locator
  on(event: "dialog", listener: (dialog: { accept(): Promise<void> }) => void): void
  request: { get(url: string): Promise<{ status(): number; json(): Promise<{ code?: string; action?: { kind: string } }> }> }
}
type Locator = { click(): Promise<void>; fill(value: string): Promise<void>; first(): Locator; waitFor(o?: { timeout?: number; state?: string }): Promise<void>; count(): Promise<number>; textContent(): Promise<string | null>; getByRole: Page["getByRole"] }

const env = (name: string) => {
  const value = process.env[name]
  if (value === undefined || value === "") throw new Error(`${name} is required`)
  return value
}
const base = env("E2E_BASE")
if (!/^https:\/\/[a-z0-9.-]+$/.test(base)) throw new Error("E2E_BASE must be an https origin")

async function signIn(page: Page) {
  await page.goto(`${base}/`)
  await page.getByLabel(/email/i).fill(env("E2E_EMAIL"))
  await page.getByLabel(/password/i).fill(env("E2E_PASSWORD"))
  await page.getByRole("button", { name: /sign in/i }).click()
  await page.getByRole("link", { name: /connections/i }).first().waitFor()
}

/** The gateway's own sign-in and consent pages: credentials are typed there, never into Norn. */
async function gatewaySignIn(page: Page) {
  await page.waitForURL(/\/api\/auth\/oauth2\/authorize|\/login|\/consent/, { timeout: 20_000 })
  if (await page.getByLabel(/email/i).count() > 0) {
    await page.getByLabel(/email/i).fill(env("E2E_GATEWAY_EMAIL"))
    await page.getByLabel(/password/i).fill(env("E2E_GATEWAY_PASSWORD"))
    await page.getByRole("button", { name: /sign in|continue|log in/i }).click()
  }
  const allow = page.getByRole("button", { name: /allow|approve|authorize|accept/i })
  if (await allow.count() > 0) await allow.first().click()
  await page.waitForURL(`${base}/connections`, { timeout: 30_000 })
}

const open = async (page: Page) => { await page.goto(`${base}/connections`); await page.getByRole("heading", { name: "Connections" }).waitFor() }
const app = () => env("E2E_APP")

type Spec = (page: Page, did: (verb: string) => void) => Promise<void>
const SPECS: Record<keyof typeof MATRIX, Spec> = {
  "Gateway sign-in": async (page, did) => {
    await open(page)
    const disconnect = page.getByRole("button", { name: "Disconnect", exact: true }).first()
    if (await disconnect.count() > 0) await disconnect.click()
    await page.getByRole("button", { name: "Sign in to the gateway" }).click()
    await gatewaySignIn(page); did("connect")
    await page.getByText("Signed in").waitFor(); did("view")
    await page.getByRole("button", { name: "Reconnect" }).first().click()
    await gatewaySignIn(page); await page.getByText("Signed in").waitFor(); did("reconnect")
    await page.getByRole("button", { name: "Disconnect" }).first().click()
    await page.getByRole("button", { name: "Sign in to the gateway" }).waitFor(); did("disconnect")
    // Leave the gateway signed in for the next kinds.
    await page.getByRole("button", { name: "Sign in to the gateway" }).click(); await gatewaySignIn(page)
  },
  "App account": async (page, did) => {
    page.on("dialog", (dialog) => void dialog.accept())
    await open(page)
    const section = page.getByRole("region", { name: app() })
    await section.waitFor()
    const connect = section.getByRole("button", { name: "Connect", exact: true })
    if (await connect.count() === 0) { await section.getByRole("button", { name: "Disconnect" }).first().click(); await connect.first().waitFor() }
    await connect.first().click()
    // The gateway's own form for the test app's API key, then back.
    await page.getByLabel(/api key|token/i).first().fill(env("E2E_APP_KEY"))
    await page.getByRole("button", { name: /connect|save|continue/i }).first().click()
    await open(page); await section.getByRole("button", { name: "Reconnect" }).first().waitFor(); did("connect")
    await section.getByRole("button", { name: "View" }).first().click()
    await section.getByText("Provider").waitFor(); did("view")
    await section.getByRole("button", { name: "Rename" }).first().click()
    const name = `e2e ${Date.now()}`
    await page.getByLabel("Account name").fill(name)
    await section.getByRole("button", { name: "Save" }).click()
    await section.getByText(name).waitFor(); did("rename")
    await section.getByRole("button", { name: "Reconnect" }).first().click()
    await page.getByLabel(/api key|token/i).first().fill(env("E2E_APP_KEY"))
    await page.getByRole("button", { name: /connect|save|continue/i }).first().click()
    await open(page); await section.getByText(name).waitFor(); did("reconnect")
    await section.getByRole("button", { name: "Disconnect" }).first().click()
    await section.getByRole("button", { name: "Connect", exact: true }).waitFor(); did("disconnect")
  },
  "Connector error": async (page, did) => {
    // A signed-out gateway is the reproducible refusal: the server names it and its fix.
    await open(page)
    const disconnect = page.getByRole("button", { name: "Disconnect", exact: true }).first()
    if (await disconnect.count() > 0) await disconnect.click()
    const response = await page.request.get(`${base}/api/connectors/inventory`)
    const body = await response.json()
    if (response.status() !== 409 || body.code !== "connectors.not_linked" || body.action?.kind !== "sign_in_gateway") throw new Error(`expected a typed not_linked refusal, got ${response.status()} ${body.code}`)
    did("show")
    await page.getByRole("button", { name: "Sign in to the gateway" }).click()
    await gatewaySignIn(page); await page.getByText("Signed in").waitFor(); did("fix")
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
