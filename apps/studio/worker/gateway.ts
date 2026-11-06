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

/** A refusal from the gateway, with its own error tag when it sent one. */
export class GatewayError extends Error {
  constructor(readonly status: number, readonly tag: string, message: string) {
    super(message)
  }
}

const text = (value: unknown): string | null => (typeof value === "string" ? value : null)
const record = (value: unknown): Record<string, unknown> => (typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {})
const list = (value: unknown): readonly unknown[] => (Array.isArray(value) ? value : [])
const segment = (value: string) => encodeURIComponent(value)

export class Gateway {
  constructor(private readonly origin: string, private readonly token: string, private readonly fetcher: typeof fetch = fetch) {}

  private async request(method: "GET" | "POST", path: string, body?: unknown): Promise<unknown> {
    const headers = new Headers({ authorization: `Bearer ${this.token}`, accept: "application/json" })
    if (body !== undefined) headers.set("content-type", "application/json")
    const response = await this.fetcher(`${this.origin}${path}`, { method, headers, ...body === undefined ? {} : { body: JSON.stringify(body) } })
    const parsed: unknown = await response.json().catch(() => null)
    if (!response.ok) {
      const error = record(parsed)
      throw new GatewayError(response.status, text(error._tag) ?? text(error.tag) ?? "GatewayError", text(error.message) ?? `The gateway answered ${response.status}.`)
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
      return name === null ? [] : [{ app, name, description: text(tool.description), inputSchema: tool.inputSchema ?? null, outputSchema: tool.outputSchema ?? null }]
    })
    return { tools, next: text(body.next) }
  }

  async tool(organization: string, app: string, name: string): Promise<GatewayTool> {
    const tool = record(await this.request("GET", `/api/organizations/${segment(organization)}/apps/${segment(app)}/tools/${segment(name)}`))
    return { app, name, description: text(tool.description), inputSchema: tool.inputSchema ?? null, outputSchema: tool.outputSchema ?? null }
  }

  /** Call a tool as the grant's user; the gateway applies its own approval rules first. */
  call(organization: string, app: string, tool: string, input: unknown): Promise<unknown> {
    return this.request("POST", `/api/organizations/${segment(organization)}/apps/${segment(app)}/tools/call`, { tool, input })
  }
}
