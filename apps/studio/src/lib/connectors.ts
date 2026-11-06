/**
 * The Studio Worker's `/api/connectors/*` as the Connections page uses it (#772). The page holds no
 * token and decides nothing: every answer, including whether a verb is allowed, is the server's.
 */

/** How the person fixes a refusal; the server names it, the page shows a button for it. */
export type FixAction =
  | { readonly kind: "sign_in_gateway" }
  | { readonly kind: "connect_account"; readonly app: string; readonly requirement: string }
  | { readonly kind: "reconnect_account"; readonly app: string | null; readonly account: string }
  | { readonly kind: "ask_admin" }

/** A typed refusal from the server. */
export class ConnectorError extends Error {
  constructor(readonly status: number, readonly code: string, message: string, readonly action: FixAction | null = null, readonly field: string | null = null) {
    super(message)
  }
}

const text = (value: unknown): value is string => typeof value === "string" && value !== ""

/** The server's fix action, only when it is one the page knows; anything else is no action. */
export function fixAction(value: unknown): FixAction | null {
  if (typeof value !== "object" || value === null) return null
  const action = value as Record<string, unknown>
  switch (action.kind) {
    case "sign_in_gateway": return { kind: "sign_in_gateway" }
    case "ask_admin": return { kind: "ask_admin" }
    case "connect_account": return text(action.app) && text(action.requirement) ? { kind: "connect_account", app: action.app, requirement: action.requirement } : null
    case "reconnect_account": return text(action.account) && (action.app === null || text(action.app)) ? { kind: "reconnect_account", app: action.app as string | null, account: action.account } : null
    default: return null
  }
}

export async function api<T>(path: string, init?: { readonly method: string; readonly body?: unknown }, fetcher: typeof fetch = fetch): Promise<T> {
  const response = await fetcher(`/api/connectors${path}`, {
    method: init?.method ?? "GET",
    credentials: "same-origin",
    headers: init?.body === undefined ? {} : { "content-type": "application/json" },
    ...init?.body === undefined ? {} : { body: JSON.stringify(init.body) },
  })
  const body = (await response.json().catch(() => null)) as Record<string, unknown> | null
  if (!response.ok) {
    const error = typeof body?.error === "string" ? body.error : `The server answered ${response.status}.`
    throw new ConnectorError(response.status, typeof body?.code === "string" ? body.code : "connectors.unreadable", error, fixAction(body?.action), typeof body?.field === "string" ? body.field : null)
  }
  // A success that is not a JSON object (an HTML fallback, an empty body) is not the server's answer.
  if (body === null || typeof body !== "object" || Array.isArray(body)) throw new ConnectorError(response.status, "connectors.unreadable", "The server's answer could not be read.")
  return body as T
}

export interface Status { readonly gateway: string | null; readonly linked: boolean }
export interface App { readonly id: string; readonly name: string }
export interface Account { readonly id: string; readonly provider: string; readonly label: string }
export interface Inventory {
  readonly apps: readonly App[]
  readonly accounts: readonly Account[]
  readonly links: readonly { readonly app: string; readonly account: string }[]
}
export interface Requirement { readonly requirement: string; readonly account: string | null; readonly state: string | null }
export interface AccountDetail extends Account { readonly apps: readonly string[]; readonly health: string | null }
export interface Tool {
  readonly app: string
  readonly name: string
  readonly description: string | null
  readonly inputSchema: unknown
  readonly outputSchema: unknown
  /** The ids of the bindings that let this tool run. */
  readonly bindings: readonly string[]
}

/** Fix labels, in the words the page's buttons use. */
export const fixLabel = (action: FixAction): string => {
  switch (action.kind) {
    case "sign_in_gateway": return "Sign in to the gateway again"
    case "connect_account": return "Connect an account"
    case "reconnect_account": return "Reconnect the account"
    case "ask_admin": return "Ask an organization admin"
  }
}

// The verb matrix: every object kind on Connections, every verb ✓ or N/A with its reason.
// The e2e suite (test/e2e/connections.e2e.ts) has one test per kind and fails when a ✓ verb goes unexercised.
export type VerbSupport = "supported" | { readonly na: string }
export const CONNECTION_VERBS = ["connect", "view", "reconnect", "rename", "disconnect"] as const
export const BINDING_VERBS = ["add", "view", "edit", "remove", "test-run"] as const
export const ERROR_VERBS = ["show", "fix"] as const

export const MATRIX = {
  "Gateway sign-in": {
    connect: "supported", view: "supported", reconnect: "supported",
    rename: { na: "one grant per gateway; it has no name of its own" }, disconnect: "supported",
  },
  "App account": { connect: "supported", view: "supported", reconnect: "supported", rename: "supported", disconnect: "supported" },
  "Connector error": { show: "supported", fix: "supported" },
} as const satisfies Record<string, Partial<Record<string, VerbSupport>>>

/** The matrix as Markdown, for PR bodies: ✓, or N/A with its reason. */
export function matrixTable(matrix: Readonly<Record<string, Readonly<Record<string, VerbSupport>>>>): string {
  const cell = (support: VerbSupport | undefined) => support === undefined ? "—" : support === "supported" ? "✓" : `N/A: ${support.na}`
  return Object.entries(matrix).map(([kind, verbs]) => [
    `| ${kind} | ${Object.keys(verbs).join(" | ")} |`,
    `|---|${Object.keys(verbs).map(() => "---").join("|")}|`,
    `| | ${Object.values(verbs).map(cell).join(" | ")} |`,
  ].join("\n")).join("\n\n")
}
