import { hostToken, type NornActor } from "./actor.ts"
import type { Auth } from "./auth.ts"
import type { ConnectorRef } from "./bindings.ts"

/**
 * Tool bindings as cell definitions (coordinator decision, 2026-11-06): Studio saves each binding
 * through DEFS's `POST /v1/definitions:install` as `kind: "binding"`, as the signed-in actor.
 * The cell recomputes `manifestDigest` from the payload with the same canonical algorithm and
 * refuses a mismatch. Agents reference a binding by its definition id.
 *
 * Until DEFS serves the binding kind on a stage, the cell answers 404 `route.not_found`: the
 * binding is then kept as the Worker's local record only, and says so (`definition: null`).
 */

export const BINDING_KIND = "binding"

/** The definition payload DEFS admits for a binding. */
export interface BindingPayload {
  readonly connector: ConnectorRef
  readonly inputSchema: unknown
  readonly outputSchema: unknown
  readonly manifestDigest: string
}

export interface Finding { readonly code: string; readonly message: string; readonly pointer: string | null }

export type Install =
  | { readonly kind: "installed"; readonly id: string; readonly version: number }
  | { readonly kind: "refused"; readonly findings: readonly Finding[] }
  | { readonly kind: "conflict" }
  /** The cell has no definition route (or no host is bound): keep the local record. */
  | { readonly kind: "local" }

/** The definition id of a binding: stable for the binding's life. */
export const bindingDefinitionId = (bindingId: string) => `studio.binding.${bindingId}`

const findingsOf = (body: unknown): readonly Finding[] => {
  const diagnostics = (body as { diagnostics?: unknown } | null)?.diagnostics
  return Array.isArray(diagnostics)
    ? diagnostics.map((d: { code?: unknown; message?: unknown; span?: { pointer?: unknown } | null }) => ({ code: String(d.code ?? ""), message: String(d.message ?? ""), pointer: typeof d.span?.pointer === "string" ? d.span.pointer : null }))
    : []
}

/** A cell that cannot be reached or answers outside DEFS's contract: nothing was saved there. */
export class CellUnavailable extends Error {}

/** The cell's definition writes, as one actor, over the NORN_HOST service binding. */
export class BindingDefinitions {
  constructor(private readonly host: Fetcher | undefined, private readonly auth: Auth, private readonly actor: NornActor) {}

  private async post(path: string, body: unknown): Promise<{ readonly status: number; readonly body: unknown } | null> {
    const { host } = this
    if (host === undefined) return null
    const headers = new Headers({ authorization: `Bearer ${await hostToken(this.auth, this.actor)}`, "content-type": "application/json", accept: "application/json" })
    let response: Response
    try {
      response = await host.fetch(new Request(`https://norn-host${path}`, { method: "POST", headers, body: JSON.stringify(body) }))
    } catch {
      throw new CellUnavailable("The cell could not be reached; the binding was not saved.")
    }
    return { status: response.status, body: await response.json().catch(() => null) as unknown }
  }

  private static outcome(answer: { readonly status: number; readonly body: unknown } | null): Install {
    if (answer === null) return { kind: "local" }
    const { status, body } = answer
    const b = body as { installed?: unknown; id?: unknown; version?: unknown; code?: unknown } | null
    if (status === 200 && b?.installed === true && typeof b.id === "string" && typeof b.version === "number") return { kind: "installed", id: b.id, version: b.version }
    if (status === 422 && b?.code === "definition.refused") return { kind: "refused", findings: findingsOf(b) }
    if (status === 409 && b?.code === "definition.version_conflict") return { kind: "conflict" }
    // No definition route on this stage yet, or the binding kind is not served: the local record stands.
    if (status === 404 && (b?.code === "route.not_found" || b?.code === "definition.kind_unknown")) return { kind: "local" }
    throw new CellUnavailable(`The cell answered ${status} (${typeof b?.code === "string" ? b.code : "no code"}); the binding was not saved.`)
  }

  /** Install a new version of the binding's definition; without `expectedVersion`, a new definition. */
  async install(id: string, payload: BindingPayload, expectedVersion?: number): Promise<Install> {
    return BindingDefinitions.outcome(await this.post("/v1/definitions:install", { kind: BINDING_KIND, id, payload, ...expectedVersion === undefined ? {} : { expectedVersion } }))
  }

  /** Retire the binding's definition: agents can no longer reference it; its history stays. */
  async retire(id: string, expectedVersion: number): Promise<Install> {
    return BindingDefinitions.outcome(await this.post(`/v1/definitions/${encodeURIComponent(id)}:retire`, { expectedVersion }))
  }
}
