/**
 * Studio's Cloudflare API calls, through distilled's typed Effect client (@distilled.cloud/cloudflare).
 *
 * Every call takes the checked credentials (deploy-target.ts cloudflareCredentials, or the legacy
 * cleanup token prod-parity names): the token is sent as `Authorization: Bearer`, never read from
 * the environment, so a Global API Key or another account's token is never used. No call retries
 * (Retry.none): each operation sends exactly one request, as the hand-written fetches did. Each call
 * is bounded; on the deadline it rejects with the same TimeoutError `AbortSignal.timeout` raised.
 *
 * A call answers `{ ok: true, value }` or `{ ok: false, status, error }`, where `status` is the HTTP
 * status Cloudflare answered (the scripts print it) and `error` distilled's typed error. A request
 * that got no answer (DNS, connection) rejects with the transport error, as fetch did.
 */
import * as Credentials from "@distilled.cloud/cloudflare/Credentials"
import * as d1 from "@distilled.cloud/cloudflare/d1"
import * as Retry from "@distilled.cloud/cloudflare/Retry"
import * as workers from "@distilled.cloud/cloudflare/workers"
import * as Cause from "effect/Cause"
import * as Effect from "effect/Effect"
import * as Exit from "effect/Exit"
import * as FetchHttpClient from "effect/http/FetchHttpClient"
import type * as HttpClient from "effect/http/HttpClient"
import * as Layer from "effect/Layer"
import * as Redacted from "effect/Redacted"
import type { CloudflareCredentials, LiveBinding, LiveDeployment, LiveDomain, LiveSettings } from "./deploy-target.ts"

/** The checked credentials, and the fetch to send with (tests pass a recording one). */
export interface CloudflareClient {
  readonly credentials: CloudflareCredentials
  readonly fetch?: typeof fetch
}

export type Answer<A, E> = { readonly ok: true; readonly value: A } | { readonly ok: false; readonly status: number; readonly error: E }

/** Run one distilled operation: one request, no retry, bounded by `timeoutMs`. */
export async function call<A, E>(client: CloudflareClient, operation: Effect.Effect<A, E, Credentials.Credentials | HttpClient.HttpClient>, timeoutMs = 30_000): Promise<Answer<A, E>> {
  const send = client.fetch ?? globalThis.fetch
  let status: number | undefined
  const recording = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const response = await send(input, init)
    status = response.status
    return response
  }) as typeof fetch
  const layer = Layer.mergeAll(
    FetchHttpClient.layer,
    Layer.succeed(FetchHttpClient.Fetch, recording),
    Credentials.fromApiToken({ apiToken: Redacted.make(client.credentials.apiToken) }),
  )
  const deadline = AbortSignal.timeout(timeoutMs)
  const exit = await Effect.runPromiseExit(operation.pipe(Retry.none, Effect.provide(layer)), { signal: deadline })
  if (Exit.isSuccess(exit)) return { ok: true, value: exit.value }
  if (deadline.aborted) throw new DOMException("The operation timed out.", "TimeoutError")
  const failure = exit.cause.reasons.find(Cause.isFailReason)
  if (failure !== undefined && status !== undefined) return { ok: false, status, error: failure.error }
  throw failure !== undefined ? transportError(failure.error) : Cause.squash(exit.cause)
}

/** The error fetch itself raised, when distilled wrapped it. */
const transportError = (error: unknown): unknown => {
  const { reason, cause } = (error ?? {}) as { reason?: { cause?: unknown }; cause?: unknown }
  return reason?.cause instanceof Error ? reason.cause : cause instanceof Error ? cause : error
}

/** The `{ code, message }` Cloudflare answered with, as its envelope's `errors` carried it. */
export const answerErrors = (error: unknown): { code?: number; message: string }[] => {
  const { code, message } = (error ?? {}) as { code?: unknown; message?: unknown }
  return [{ ...(typeof code === "number" ? { code } : {}), message: typeof message === "string" ? message : String(error) }]
}

type Settings = workers.GetScriptScriptAndVersionSettingResponse
/** distilled answers camelCase; the parity checks read Cloudflare's wire names. */
export function liveSettings(settings: Settings): LiveSettings {
  return {
    ...(settings.compatibilityDate != null ? { compatibility_date: settings.compatibilityDate } : {}),
    ...(settings.compatibilityFlags != null ? { compatibility_flags: [...settings.compatibilityFlags] } : {}),
    ...(settings.bindings != null ? {
      bindings: settings.bindings.map((binding): LiveBinding => {
        const { name, type, className, scriptName } = binding as { name: string; type: string; className?: string | null; scriptName?: string | null }
        return { name, type, ...(className !== undefined ? { class_name: className } : {}), ...(scriptName !== undefined ? { script_name: scriptName } : {}) }
      }),
    } : {}),
  }
}

/** GET /accounts/{account}/workers/scripts/{script}/settings */
export const workerSettings = (client: CloudflareClient, script: string) =>
  call(client, workers.getScriptScriptAndVersionSetting({ accountId: client.credentials.accountId, scriptName: script }))

/** GET /accounts/{account}/workers/domains (the first page, as before) */
export async function workerDomains(client: CloudflareClient): Promise<Answer<LiveDomain[], workers.ListDomainsError>> {
  const answer = await call(client, workers.listDomains({ accountId: client.credentials.accountId }))
  if (!answer.ok) return answer
  return { ok: true, value: answer.value.result.map(({ hostname, service, environment, enabled }) => ({ hostname, service, environment, enabled })) }
}

/** GET /accounts/{account}/workers/scripts/{script}/deployments */
export async function workerDeployments(client: CloudflareClient, script: string): Promise<Answer<LiveDeployment[], workers.ListScriptDeploymentsError>> {
  const answer = await call(client, workers.listScriptDeployments({ accountId: client.credentials.accountId, scriptName: script }))
  if (!answer.ok) return answer
  return {
    ok: true,
    value: answer.value.deployments.map((deployment) => ({
      id: deployment.id,
      created_on: deployment.createdOn,
      versions: deployment.versions.map(({ versionId, percentage }) => ({ version_id: versionId, percentage })),
    })),
  }
}

/** POST /accounts/{account}/workers/scripts/{script}/tails with body `{}` */
export const openTail = (client: CloudflareClient, script: string) =>
  call(client, workers.createScriptTail({ accountId: client.credentials.accountId, scriptName: script }))

/** DELETE /accounts/{account}/workers/scripts/{script}/tails/{id} */
export const closeTail = (client: CloudflareClient, script: string, id: string) =>
  call(client, workers.deleteScriptTail({ accountId: client.credentials.accountId, scriptName: script, id }))

/** GET /accounts/{account}/d1/database?name={name} (the first page, as before) */
export const d1Databases = (client: CloudflareClient, name: string, timeoutMs = 15_000) =>
  call(client, d1.listDatabases({ accountId: client.credentials.accountId, name }), timeoutMs)
