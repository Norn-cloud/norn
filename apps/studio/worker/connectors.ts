import { AccessUnavailable, decide, toolBinding, workspace } from "./access.ts"
import { originAllowed, resolveActor } from "./actor.ts"
import type { Auth } from "./auth.ts"
import { BindingDefinitions, bindingDefinitionId, CellUnavailable, type Install } from "./binding-definitions.ts"
import { cellScope } from "./scope.ts"
import { abandonSave, adoptCell, beginCreate, beginEdit, connectorString, connectorVersion, finishSave, getBinding, listBindings, manifestDigest, readDraft, removeBinding, runInput, toolManifest, type Binding } from "./bindings.ts"
import { Gateway, GatewayError } from "./gateway.ts"

/**
 * `/api/connectors/*` (#772): the signed-in person's tool gateway, and the organization's tool
 * bindings. The Worker links the person's gateway grant through Better Auth (encrypted at rest,
 * refreshed server-side, never sent to the browser). Accounts are connected in the gateway's own
 * form. A tool runs only through a binding the organization made (default deny). Every refusal is
 * typed and, where the person can fix it, names the fix.
 */

export const GATEWAY_PROVIDER = "executor"

export interface ConnectorEnv {
  /** The gateway origin, e.g. https://gateway.example; absent when this stage has no gateway. */
  readonly TOOL_GATEWAY_URL?: string
  readonly BETTER_AUTH_URL: string
}

/** What the routes reach outside the request: the bindings table, the person's grant and the network. */
export interface ConnectorDeps {
  readonly db: D1Database
  readonly grant?: (auth: Auth, request: Request) => Promise<string | null>
  readonly fetch?: typeof fetch
  /** The Norn cell host (NORN_HOST); bindings are installed there as definitions when it serves them. */
  readonly host?: Fetcher
}

/** How the person fixes a refusal; the page renders one button per kind. */
export type FixAction =
  | { readonly kind: "sign_in_gateway" }
  | { readonly kind: "connect_account"; readonly app: string; readonly requirement: string }
  | { readonly kind: "reconnect_account"; readonly app: string | null; readonly account: string }
  | { readonly kind: "ask_admin" }

const NO_STORE = { "cache-control": "no-store" }
/** A typed refusal: `code` is stable, `error` is for people, `action` is the fix when there is one. */
export const refusal = (status: number, code: string, error: string, action?: FixAction, field?: string) =>
  Response.json({ code, error, ...field === undefined ? {} : { field }, ...action === undefined ? {} : { action } }, { status, headers: NO_STORE })
const ok = (body: unknown) => Response.json(body, { headers: NO_STORE })

/** The Better Auth row of the person's linked gateway account, if any. */
async function gatewayAccount(auth: Auth, request: Request): Promise<string | null> {
  const accounts = await auth.api.listUserAccounts({ headers: request.headers })
  return accounts.find((account) => account.providerId === GATEWAY_PROVIDER)?.id ?? null
}

/** The person's gateway grant, refreshed by Better Auth when it has expired; null when not linked. */
async function grant(auth: Auth, request: Request): Promise<string | null> {
  const accountId = await gatewayAccount(auth, request)
  if (accountId === null) return null
  try {
    const { accessToken } = await auth.api.getAccessToken({ body: { accountId }, headers: request.headers })
    return accessToken ?? null
  } catch {
    // A grant that can no longer be refreshed is the same as none: the person signs in again.
    return null
  }
}

/** A gateway refusal as a Norn one, with the fix the person can make. */
export function gatewayRefusal(error: unknown, app: string | null = null): Response {
  if (!(error instanceof GatewayError)) throw error
  const { tag, status, detail } = error
  const account = detail.account
  if (status === 401) return refusal(401, "connectors.reauth", "Your gateway sign-in has expired. Sign in to the gateway again.", { kind: "sign_in_gateway" })
  if (detail.reason === "insufficient_scope") return refusal(403, "connectors.missing_scope", "Studio's gateway sign-in does not include this permission. Sign in to the gateway again and allow it.", { kind: "sign_in_gateway" })
  if (detail.upstreamCode?.toLowerCase() === "insufficient_scope" && account !== undefined) return refusal(403, "connectors.account_missing_scope", "The connected account does not grant a permission this tool needs. Reconnect it and allow that permission.", { kind: "reconnect_account", app: detail.app ?? app, account })
  if ((tag === "OAuthReconnectRequired" || tag === "OAuthRenewalFailed") && account !== undefined) return refusal(409, "connectors.account_reconnect", "The connected account's sign-in has expired. Reconnect it.", { kind: "reconnect_account", app: detail.app ?? app, account })
  if (tag === "AccountRequired" && detail.requirement !== undefined) return refusal(409, "connectors.account_required", "This tool needs an account you have not connected yet.", { kind: "connect_account", app: detail.app ?? app ?? "", requirement: detail.requirement })
  if (tag === "ToolApprovalRequired") return refusal(409, "connectors.approval_required", "The gateway needs this call approved first. Approve it in the gateway, then run it again.")
  if (tag === "InputInvalid") return refusal(422, "connectors.input_invalid", error.message)
  if (status === 403) return refusal(403, "connectors.gateway_forbidden", error.message)
  if (status === 404) return refusal(404, "connectors.not_found", error.message)
  return refusal(502, "connectors.gateway", `The gateway refused (${tag}): ${error.message}`)
}

const json = async (request: Request): Promise<Record<string, unknown> | null> => {
  const body: unknown = await request.json().catch(() => null)
  return typeof body === "object" && body !== null && !Array.isArray(body) ? body as Record<string, unknown> : null
}
/** A path segment that is not valid percent-encoding. */
class BadPath extends Error {}
const part = (match: RegExpExecArray, index: number) => {
  try {
    return decodeURIComponent(match[index] ?? "")
  } catch {
    throw new BadPath(match[index] ?? "")
  }
}
const label = (value: unknown) => (typeof value === "string" && value.trim() !== "" && value.length <= 120 ? value.trim() : null)

/** Route one `/api/connectors/*` request; `path` is what follows `/api/connectors`. Every answer is JSON. */
export async function connectors(request: Request, env: ConnectorEnv, auth: Auth, path: string, deps: ConnectorDeps): Promise<Response> {
  try {
    return await route(request, env, auth, path, deps)
  } catch (error) {
    if (error instanceof BadPath) return refusal(400, "connectors.invalid_request", "The path is not valid percent-encoding.")
    if (error instanceof GatewayError) return gatewayRefusal(error)
    // Better Auth's own refusals (an expired session during link or unlink) carry an HTTP status.
    const status = (error as { statusCode?: unknown } | null)?.statusCode
    if (typeof status === "number" && status >= 400 && status < 500) return refusal(status, "connectors.auth_refused", error instanceof Error ? error.message : "The sign-in service refused.", status === 401 ? { kind: "sign_in_gateway" } : undefined)
    console.error("connectors: unexpected failure", error)
    return refusal(500, "connectors.internal", "Connectors failed unexpectedly; nothing was changed in the gateway.")
  }
}

async function route(request: Request, env: ConnectorEnv, auth: Auth, path: string, deps: ConnectorDeps): Promise<Response> {
  const { db } = deps
  const actor = await resolveActor(request, auth)
  if (actor === null) return refusal(401, "connectors.sign_in", "Sign in first.")
  // Service keys cannot hold a person's gateway grant.
  if (actor.actorKind !== "user") return refusal(403, "connectors.user_only", "Connectors act for a signed-in person, not an API key.")
  if (!originAllowed(request, actor, env.BETTER_AUTH_URL)) return refusal(403, "connectors.origin", "Cross-origin request refused.")
  // The cell scope is the session's: its active organization and that organization's environment.
  const { environmentId } = await cellScope(auth, actor.orgId)
  // Every connector verb is the host PDP's decision (#1036); the Worker holds no role logic.
  const ask = async (checks: Parameters<typeof decide>[4]) => {
    try {
      return await decide(deps.host, auth, actor, environmentId, checks)
    } catch (error) {
      if (error instanceof AccessUnavailable) return error
      throw error
    }
  }
  const unavailable = (error: AccessUnavailable) => refusal(503, "connectors.access_unavailable", error.message)
  const denied = (what: string) => refusal(403, "connectors.forbidden", `Your organization's policy does not let you ${what}.`, { kind: "ask_admin" })
  // Without an environment there is no cell scope to ask in (refused, never defaulted).
  if (deps.host !== undefined && environmentId === null) return refusal(409, "connectors.no_environment", "Your organization has no cell environment yet, so connectors cannot be used.", { kind: "ask_admin" })
  const workspaceDecisions = await ask([{ action: "connector.read", resource: workspace(actor.orgId) }, { action: "connector.bind", resource: workspace(actor.orgId) }])
  if (workspaceDecisions instanceof AccessUnavailable) return unavailable(workspaceDecisions)
  if (workspaceDecisions[0]?.decision !== "permit") return denied("use connectors")
  const manage = workspaceDecisions[1]?.decision === "permit"
  const route = `${request.method} ${path}`
  const bindingRoute = /^\/bindings\/([^/]+)$/.exec(path)

  // Bindings are Norn's own records: viewing and removing them needs no gateway.
  const shown = (binding: Binding) => ({ ...binding, connectorRef: connectorString(binding.connector) })
  // Without a cell host there is nothing to scope: the binding stays a local record, as before.
  const cellFor = () => deps.host === undefined ? new BindingDefinitions(undefined, auth, actor, "") : environmentId === null ? null : new BindingDefinitions(deps.host, auth, actor, environmentId)
  const noEnvironment = () => refusal(409, "connectors.no_environment", "Your organization has no cell environment yet, so tool bindings cannot be saved there.", { kind: "ask_admin" })
  /** A definition write that did not land is a typed refusal; `null` when it landed or the stage keeps bindings locally. */
  const cellRefusal = (outcome: Install) =>
    outcome.kind === "refused" ? Response.json({ code: "connectors.binding_refused", error: "The cell refused this binding.", findings: outcome.findings }, { status: 422, headers: NO_STORE })
      : outcome.kind === "conflict" ? refusal(409, "connectors.binding_conflict", "Someone changed this binding since it was opened. Reload it and try again.")
        : outcome.kind === "denied" ? refusal(403, "connectors.binding_denied", `The cell does not let you save or remove tool bindings (${outcome.code}).`, { kind: "ask_admin" })
        : null
  /**
   * Settle a binding a save left pending, from what the cell holds: the cell's version and fields
   * when it has the definition, otherwise undo the save (a create is deleted, an edit restored).
   * Returns the settled binding, null when it was deleted, or the binding still pending when the
   * cell cannot be asked now.
   */
  const reconcile = async (binding: Binding): Promise<Binding | null> => {
    if (binding.state !== "pending" || binding.pendingDefinition === null) return binding
    const cell = cellFor()
    if (cell === null) return binding
    let held: Awaited<ReturnType<BindingDefinitions["get"]>>
    try {
      held = await cell.get(binding.pendingDefinition)
    } catch (error) {
      if (error instanceof CellUnavailable) return binding
      throw error
    }
    if (held.kind === "found" && (binding.definition === null || held.version > binding.definition.version)) await adoptCell(db, actor.orgId, binding.id, binding.pendingDefinition, held.version, held.payload)
    else if (await abandonSave(db, actor.orgId, binding.id, binding.pendingSave ?? "edit", binding.definition) === "deleted") return null
    return await getBinding(db, actor.orgId, binding.id)
  }
  const settled = async (id: string) => { const binding = await getBinding(db, actor.orgId, id); return binding === null ? null : await reconcile(binding) }
  const stillPending = () => refusal(409, "connectors.binding_pending", "This binding is still being saved. Try again in a moment.")
  if (route === "GET /bindings") {
    const rows = await Promise.all((await listBindings(db, actor.orgId)).map(reconcile))
    return ok({ bindings: rows.filter((b): b is Binding => b !== null).map(shown), manage })
  }
  if (request.method === "DELETE" && bindingRoute !== null) {
    if (!manage) return denied("remove tool bindings")
    const id = part(bindingRoute, 1)
    const existing = await settled(id)
    if (existing === null) return refusal(404, "connectors.binding_not_found", "No such tool binding.")
    if (existing.state === "pending") return stillPending()
    // Retire the cell definition first: agents must stop referencing it before the record goes.
    if (existing.definition !== null) {
      const cell = cellFor()
      if (cell === null) return noEnvironment()
      const retired = await cell.retire(existing.definition.id, existing.definition.version).catch((error: unknown) => { if (error instanceof CellUnavailable) return error; throw error })
      if (retired instanceof CellUnavailable) return refusal(503, "connectors.cell_unavailable", retired.message)
      const refused = cellRefusal(retired)
      if (refused !== null) return refused
    }
    await removeBinding(db, actor.orgId, id)
    return ok({ removed: id, retired: existing.definition?.id ?? null })
  }

  const gatewayOrigin = env.TOOL_GATEWAY_URL
  if (route === "GET /status" && gatewayOrigin === undefined) return ok({ gateway: null, linked: false })
  if (gatewayOrigin === undefined) return refusal(503, "connectors.not_configured", "No tool gateway is configured for this deployment.")
  if (route === "POST /link") {
    // Connect and reconnect: the generic OAuth provider is a social provider to Better Auth; linking needs the session.
    // Better Auth keeps the OAuth state in a signed cookie it sets here; the browser must receive
    // it, or the callback refuses with a state mismatch.
    const { headers: set, response } = await auth.api.linkSocialAccount({ body: { provider: GATEWAY_PROVIDER, callbackURL: "/connections" }, headers: request.headers, returnHeaders: true })
    const answer = ok({ url: response.url })
    for (const cookie of set.getSetCookie()) answer.headers.append("set-cookie", cookie)
    return answer
  }
  if (route === "POST /unlink") {
    const accountId = await gatewayAccount(auth, request)
    if (accountId !== null) await auth.api.unlinkAccount({ body: { accountId }, headers: request.headers })
    return ok({ linked: false })
  }
  const token = await (deps.grant ?? grant)(auth, request)
  if (route === "GET /status") return ok({ gateway: gatewayOrigin, linked: token !== null })
  if (token === null) return refusal(409, "connectors.not_linked", "Sign in to the tool gateway first.", { kind: "sign_in_gateway" })
  const gateway = new Gateway(gatewayOrigin, token, deps.fetch ?? fetch)
  let app: string | null = null

  try {
    const organization = await gateway.organization()
    if (route === "GET /inventory") return ok(await gateway.inventory(organization))

    const requirementsRoute = /^\/apps\/([^/]+)\/accounts$/.exec(path)
    if (request.method === "GET" && requirementsRoute !== null) return ok({ requirements: await gateway.requirements(organization, (app = part(requirementsRoute, 1))) })
    const connectRoute = /^\/apps\/([^/]+)\/accounts\/([^/]+)\/connect$/.exec(path)
    if (request.method === "POST" && connectRoute !== null) {
      // A new account, or with `account`, a reconnect that replaces its credentials.
      const account = (await json(request))?.account
      if (account !== undefined && typeof account !== "string") return refusal(400, "connectors.invalid_request", "account must be an account id.")
      return ok({ url: await gateway.connect(organization, (app = part(connectRoute, 1)), part(connectRoute, 2), account) })
    }
    const disconnectRoute = /^\/apps\/([^/]+)\/accounts\/([^/]+)\/([^/]+)$/.exec(path)
    if (request.method === "DELETE" && disconnectRoute !== null) {
      const account = part(disconnectRoute, 3)
      const deleted = await gateway.disconnect(organization, (app = part(disconnectRoute, 1)), part(disconnectRoute, 2), account)
      return ok({ disconnected: account, deleted })
    }
    const accountRoute = /^\/accounts\/([^/]+)$/.exec(path)
    if (accountRoute !== null) {
      const account = part(accountRoute, 1)
      if (request.method === "GET") return ok(await gateway.account(organization, account))
      if (request.method === "PATCH") {
        const renamed = label((await json(request))?.label)
        if (renamed === null) return refusal(400, "connectors.invalid_request", "Send {label}, 1–120 characters.", undefined, "label")
        await gateway.rename(organization, account, renamed)
        return ok(await gateway.account(organization, account))
      }

    }

    const toolsRoute = /^\/apps\/([^/]+)\/tools$/.exec(path)
    if (request.method === "GET" && toolsRoute !== null) {
      app = part(toolsRoute, 1)
      const cursor = new URL(request.url).searchParams.get("cursor") ?? undefined
      const [page, bindings] = await Promise.all([gateway.tools(organization, app, cursor), listBindings(db, actor.orgId)])
      return ok({ next: page.next, tools: page.tools.map((tool) => ({ ...tool, bindings: bindings.filter((b) => b.app === tool.app && b.tool === tool.name).map((b) => b.id) })) })
    }

    // Adding or editing a binding checks its input mapping against the tool's live input schema.
    if (route === "POST /bindings" || (request.method === "PATCH" && bindingRoute !== null)) {
      if (!manage) return denied("add or edit tool bindings")
      const existing = bindingRoute === null ? undefined : await settled(part(bindingRoute, 1))
      if (existing === null) return refusal(404, "connectors.binding_not_found", "No such tool binding.")
      if (existing?.state === "pending") return stillPending()
      const body = await json(request)
      const target = { app: existing?.app ?? body?.app, tool: existing?.tool ?? body?.tool }
      if (typeof target.app !== "string" || typeof target.tool !== "string") return refusal(400, "connectors.binding_invalid", "Send {app, tool, label?, inputMapping?, runRole?}.", undefined, "tool")
      app = target.app
      const tool = await gateway.tool(organization, target.app, target.tool)
      const draft = readDraft(body, tool.inputSchema, existing)
      if ("error" in draft) return refusal(400, "connectors.binding_invalid", draft.error, undefined, draft.field)
      // Pinned to the tool as the gateway serves it now: the cell and the canvas admit by this digest, never by name.
      const digest = await manifestDigest(toolManifest(tool))
      const connector = { gateway: "executor" as const, origin: gatewayOrigin, app: tool.app, tool: tool.name, version: connectorVersion(tool.version, digest) }
      const id = existing?.id ?? crypto.randomUUID()
      const definitionId = existing?.definition?.id ?? bindingDefinitionId(id)
      const cell = cellFor()
      if (cell === null) return noEnvironment()
      // Record the intent first, so a failure at any point below leaves a pending row that
      // reconciliation settles against the cell, never a half-saved binding.
      if (existing === undefined) await beginCreate(db, actor.orgId, actor.subjectId, draft, { manifestDigest: digest, connector }, id, definitionId)
      else await beginEdit(db, actor.orgId, id, definitionId)
      let outcome: Install
      try {
        outcome = await cell.install(definitionId, { connector, inputSchema: tool.inputSchema, outputSchema: tool.outputSchema, manifestDigest: digest, label: draft.label, inputMapping: draft.inputMapping, runRole: draft.runRole }, existing?.definition?.version)
      } catch (error) {
        // The outcome is unknown (the cell may have installed it): keep the intent for reconciliation.
        if (error instanceof CellUnavailable) return refusal(503, "connectors.cell_unavailable", `${error.message} It will be settled against the cell before it is used.`)
        throw error
      }
      const refused = cellRefusal(outcome)
      if (refused !== null) {
        // The cell decided no: nothing was installed, so the intent is undone.
        await abandonSave(db, actor.orgId, id, existing === undefined ? "create" : "edit", existing?.definition ?? null)
        return refused
      }
      const definition = outcome.kind === "installed" ? { id: outcome.id, version: outcome.version } : null
      return ok({ binding: shown(await finishSave(db, actor.orgId, actor.subjectId, id, draft, { manifestDigest: digest, connector, definition })) })
    }

    const runRoute = /^\/bindings\/([^/]+)\/run$/.exec(path)
    if (request.method === "POST" && runRoute !== null) {
      const binding = await settled(part(runRoute, 1))
      if (binding === null) return refusal(404, "connectors.binding_not_found", "No such tool binding.")
      if (binding.state === "pending") return stillPending()
      // Running is decided on the binding's cell definition, whose runRole the host reads itself.
      if (binding.definition === null) return refusal(409, "connectors.binding_not_in_cell", `${binding.label} is not stored in the cell yet, so who may run it cannot be decided there.`)
      const runDecision = await ask([{ action: "connector.run", resource: toolBinding(binding.definition.id) }])
      if (runDecision instanceof AccessUnavailable) return unavailable(runDecision)
      if (runDecision[0]?.decision !== "permit") return denied(`run ${binding.label}`)
      const input = (await json(request))?.input
      if (typeof input !== "object" || input === null || Array.isArray(input)) return refusal(400, "connectors.invalid_request", "Send {input: {…}} for the fields the binding does not fix.", undefined, "input")
      const merged = runInput(binding, input as Record<string, unknown>)
      if (!merged.ok) return refusal(400, "connectors.binding_invalid", merged.problem.error, undefined, merged.problem.field)
      app = binding.app
      return ok({ binding: binding.id, result: await gateway.call(organization, binding.app, binding.tool, merged.input) })
    }
  } catch (error) {
    return gatewayRefusal(error, app)
  }
  return refusal(404, "connectors.not_found", "No such connectors endpoint.")
}
