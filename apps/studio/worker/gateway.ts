/**
 * The tool gateway (Executor v2, #772) over its HTTP product API. This module is the only place
 * that knows the gateway's routes and shapes; going native later replaces it alone. Calls carry
 * the signed-in user's gateway grant as a bearer token, so the gateway applies that user's
 * accounts, audiences and approval rules. Upstream credentials never leave the gateway.
 */

/** A tool as Studio shows it: its JSON Schemas are passed through for the checker to type. */
export interface GatewayTool {
  readonly app: string
  readonly name: string
  readonly description: string | null
  readonly inputSchema: unknown
  readonly outputSchema: unknown | null
  /** The app version the gateway evaluated this tool from; null when it did not say. */
  readonly version: string | null
}

export interface GatewayApp {
  readonly id: string
  readonly name: string
}

export interface GatewayAccount {
  readonly id: string
  readonly provider: string
  readonly label: string
}

export interface Inventory {
  readonly apps: readonly GatewayApp[]
  readonly accounts: readonly GatewayAccount[]
  /** Which of the user's accounts each app uses. */
  readonly links: readonly { readonly app: string; readonly account: string }[]
}

/** One of the user's accounts linked to an app's requirement. */
export interface RequirementLink {
  readonly requirement: string
  readonly account: string | null
  /** The gateway's sign-in state for it, e.g. `connected` or `reconnect_required`. */
  readonly state: string | null
}

/** A refusal from the gateway: its error tag, and the identity fields it names (never credentials). */
export class GatewayError extends Error {
  constructor(
    readonly status: number,
    readonly tag: string,
    message: string,
    readonly detail: { readonly reason?: string; readonly account?: string; readonly app?: string; readonly requirement?: string; readonly upstreamCode?: string } = {},
  ) {
    super(message)
  }
}

const text = (value: unknown): string | null => (typeof value === "string" ? value : null)
const record = (value: unknown): Record<string, unknown> => (typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {})
const list = (value: unknown): readonly unknown[] => (Array.isArray(value) ? value : [])
const segment = (value: string) => encodeURIComponent(value)

/** How long one gateway call may take before it is a typed timeout. */
export const GATEWAY_TIMEOUT_MS = 20_000

export class Gateway {
  constructor(private readonly origin: string, private readonly token: string, private readonly fetcher: typeof fetch = fetch) {}

  private async request(method: "GET" | "POST" | "PATCH" | "DELETE", path: string, body?: unknown): Promise<unknown> {
    const headers = new Headers({ authorization: `Bearer ${this.token}`, accept: "application/json" })
    if (body !== undefined) headers.set("content-type", "application/json")
    // Called without a receiver: workerd's native fetch throws "Illegal invocation" when called as a method.
    const { fetcher } = this
    let response: Response
    try {
      response = await fetcher(`${this.origin}${path}`, { method, headers, signal: AbortSignal.timeout(GATEWAY_TIMEOUT_MS), ...body === undefined ? {} : { body: JSON.stringify(body) } })
    } catch (error) {
      const timedOut = error instanceof Error && (error.name === "TimeoutError" || error.name === "AbortError")
      throw new GatewayError(timedOut ? 504 : 502, timedOut ? "GatewayTimeout" : "GatewayUnreachable", timedOut ? `The gateway did not answer within ${GATEWAY_TIMEOUT_MS / 1000} s.` : "The gateway could not be reached.")
    }
    const parsed: unknown = await response.json().catch(() => null)
    if (!response.ok) {
      const error = record(parsed)
      const upstream = record(error.upstream)
      const detail = Object.fromEntries(Object.entries({
        reason: text(error.reason), account: text(error.accountId) ?? text(error.account), app: text(error.app),
        requirement: text(error.requirement) ?? text(error.slot), upstreamCode: text(upstream.code),
      }).filter(([, value]) => value !== null)) as GatewayError["detail"]
      throw new GatewayError(response.status, text(error._tag) ?? text(error.tag) ?? "GatewayError", text(error.message) ?? `The gateway answered ${response.status}.`, detail)
    }
    return parsed
  }

  /** The gateway organization this grant was approved for. */
  async organization(): Promise<string> {
    const organization = text(record(await this.request("GET", "/api/context")).organization)
    if (organization === null) throw new GatewayError(502, "GatewayShape", "The gateway did not name an organization.")
    return organization
  }

  async inventory(organization: string): Promise<Inventory> {
    const body = record(await this.request("GET", `/api/organizations/${segment(organization)}/inventory`))
    return {
      apps: list(body.apps).map(record).flatMap((app) => {
        const id = text(app.id) ?? text(app.slug)
        return id === null ? [] : [{ id, name: text(app.name) ?? id }]
      }),
      // Only identity fields: the gateway never sends credentials here, and Studio keeps it that way.
      accounts: list(body.accounts).map(record).flatMap((account) => {
        const id = text(account.id)
        return id === null ? [] : [{ id, provider: text(account.provider) ?? "", label: text(account.label) ?? text(account.name) ?? id }]
      }),
      links: list(body.links).map(record).flatMap((link) => {
        const app = text(link.app)
        const account = text(link.account)
        return app === null || account === null ? [] : [{ app, account }]
      }),
    }
  }

  /** One page of an app's tools and the cursor of the next, if any. */
  async tools(organization: string, app: string, cursor?: string): Promise<{ readonly tools: readonly GatewayTool[]; readonly next: string | null }> {
    const query = cursor === undefined ? "" : `?cursor=${encodeURIComponent(cursor)}`
    const body = record(await this.request("GET", `/api/organizations/${segment(organization)}/apps/${segment(app)}/tools${query}`))
    const tools = list(body.items).map(record).flatMap((tool) => {
      const name = text(tool.name)
      return name === null ? [] : [{ app, name, description: text(tool.description), inputSchema: tool.inputSchema ?? null, outputSchema: tool.outputSchema ?? null, version: text(tool.version) ?? text(body.version) }]
    })
    return { tools, next: text(body.next) }
  }

  async tool(organization: string, app: string, name: string): Promise<GatewayTool> {
    const tool = record(await this.request("GET", `/api/organizations/${segment(organization)}/apps/${segment(app)}/tools/${segment(name)}`))
    return { app, name, description: text(tool.description), inputSchema: tool.inputSchema ?? null, outputSchema: tool.outputSchema ?? null, version: text(tool.version) }
  }

  /**
   * Each of the app's per-user account requirements, with the account the user linked to it, if
   * any. The requirements are the app's declared ones (`GET /apps/{app}` → requirements.accounts,
   * scope "user"); `…/accounts/user` lists only links, so a requirement nothing fills yet still
   * appears, with `account: null`, and can be connected.
   */
  async requirements(organization: string, app: string): Promise<readonly RequirementLink[]> {
    const base = `/api/organizations/${segment(organization)}/apps/${segment(app)}`
    const [detail, linked] = await Promise.all([this.request("GET", base), this.request("GET", `${base}/accounts/user`)])
    const declared = Object.entries(record(record(record(detail).requirements).accounts))
      .filter(([, requirement]) => text(record(requirement).scope) === "user")
      .map(([name]) => name)
    const links = list(linked).map(record).flatMap((link) => {
      const requirement = text(link.requirement)
      return requirement === null ? [] : [{ requirement, account: text(link.account) ?? text(record(link.account).id), state: text(link.state) ?? text(record(link.signIn).state) }]
    })
    // Every declared requirement once per linked account, or once unfilled; links to requirements
    // the app no longer declares are still shown, so they can be disconnected.
    const names = [...new Set([...declared, ...links.map((link) => link.requirement)])]
    return names.flatMap((requirement) => {
      const mine = links.filter((link) => link.requirement === requirement)
      return mine.length > 0 ? mine : [{ requirement, account: null, state: null }]
    })
  }

  /**
   * Start connecting an account for a requirement, or replacing `account`'s credentials. The
   * person finishes in the gateway's own form at the returned URL, so credentials never pass Norn.
   */
  async connect(organization: string, app: string, requirement: string, account?: string): Promise<string> {
    const body = record(await this.request("POST", `/api/organizations/${segment(organization)}/apps/${segment(app)}/accounts/user/${segment(requirement)}/connections`, {
      destination: { kind: "personal" }, ...account === undefined ? {} : { account },
    }))
    const url = text(body.url)
    if (url === null) throw new GatewayError(502, "GatewayShape", "The gateway did not return a connection link.")
    return url
  }

  async account(organization: string, account: string): Promise<GatewayAccount & { readonly apps: readonly string[]; readonly health: string | null }> {
    const body = record(await this.request("GET", `/api/organizations/${segment(organization)}/accounts/${segment(account)}`))
    const detail = record(body.account)
    return {
      id: text(detail.id) ?? account, provider: text(record(body.provider).name) ?? text(detail.provider) ?? "", label: text(detail.label) ?? account,
      apps: list(body.apps).map(record).flatMap((app) => text(app.id) ?? []), health: text(record(body.health).status) ?? text(body.health),
    }
  }

  async rename(organization: string, account: string, label: string): Promise<void> {
    await this.request("PATCH", `/api/organizations/${segment(organization)}/accounts/${segment(account)}`, { label })
  }

  /**
   * Disconnect an account from one app: unlink it from the requirement, then delete it from the
   * gateway unless another app still uses it (the gateway refuses deleting a linked account).
   * Returns whether the account itself was deleted.
   */
  async disconnect(organization: string, app: string, requirement: string, account: string): Promise<boolean> {
    await this.request("DELETE", `/api/organizations/${segment(organization)}/apps/${segment(app)}/accounts/user/${segment(requirement)}/${segment(account)}`)
    try {
      await this.request("DELETE", `/api/organizations/${segment(organization)}/accounts/${segment(account)}`)
      return true
    } catch (error) {
      if (error instanceof GatewayError && ["AccountLinkedToApp", "AccountWebhooksActive", "AccountWorkflowsActive"].includes(error.tag)) return false
      throw error
    }
  }

  /** Call a tool as the grant's user; the gateway applies its own approval rules first. */
  call(organization: string, app: string, tool: string, input: unknown): Promise<unknown> {
    return this.request("POST", `/api/organizations/${segment(organization)}/apps/${segment(app)}/tools/call`, { tool, input })
  }
}
