/**
 * `norn seed`: replay a project's seed (`seed/`, as `norn init --template`
 * lays it out) into its running `norn dev`, through the cell's own routes.
 *
 * - `seed/calls.json` (`norn-calls/1`): governed action calls, each sent by
 *   its `actor`, a local principal of the session (the developer, the
 *   reviewer or a named one, DEV-PRINCIPALS-1), with its own idempotency key.
 *   The cell decides every call by the pack's access policy, laws and
 *   lifecycles; a call it refuses stops the seed with the cell's refusal.
 *   Sending the seed again replays: the same keys commit nothing new.
 * - `seed/definitions/*.json` (`{ kind, id, payload }`, questions before
 *   dashboards): admitted and installed as the developer over
 *   `/v1/definitions:admit` and `:install`, the path Studio saves them by,
 *   with the sign-off the admission restated. One already installed with the
 *   same payload is left alone.
 *
 * It decides nothing and holds no policy: the dev host is the only authority.
 */
import { existsSync, readdirSync, readFileSync } from "node:fs"
import { isAbsolute, join, relative, resolve } from "node:path"
import { canonicalJson, type Json } from "./canonical.ts"
import { ProjectError, readProject } from "./project.ts"

const LOOPBACK_URL = /^http:\/\/(127\.0\.0\.1|\[::1\]):\d{1,5}$/

/** One local principal of the running session. */
export interface SeedSession {
  readonly url: string
  readonly scope: { readonly orgId: string; readonly environmentId: string }
  readonly developer: string
  /** Every local principal's token, by actor. */
  readonly tokens: ReadonlyMap<string, string>
}

/** The running `norn dev` of `root`, as its descriptor (`norn-dev/1`) names it. */
export function seedSession(root: string): SeedSession {
  const path = join(root, ".norn/dev.json")
  if (!existsSync(path)) throw new ProjectError("cli.project.io", `No running norn dev in ${root}: start it there first (${path} is missing).`, 3)
  const descriptor = JSON.parse(readFileSync(path, "utf8")) as Record<string, any>
  if (descriptor["format"] !== "norn-dev/1" || typeof descriptor["url"] !== "string" || !LOOPBACK_URL.test(descriptor["url"])) {
    throw new ProjectError("cli.project.io", `${path} is not a loopback norn-dev/1 descriptor.`, 3)
  }
  const token = (file: unknown): string => {
    if (typeof file !== "string") throw new ProjectError("cli.project.io", `${path} names a session without a token file.`, 3)
    const at = resolve(root, file)
    const inside = relative(resolve(root), at)
    if (inside.startsWith("..") || isAbsolute(inside)) throw new ProjectError("cli.project.io", `The token file ${file} is outside the project.`, 3)
    return readFileSync(at, "utf8").trim()
  }
  const tokens = new Map<string, string>([[descriptor["actor"], token(descriptor["tokenFile"])]])
  if (descriptor["reviewer"]) tokens.set(descriptor["reviewer"].actor, token(descriptor["reviewer"].tokenFile))
  for (const principal of (descriptor["principals"] ?? []) as Array<{ actor: string; tokenFile: string }>) tokens.set(principal.actor, token(principal.tokenFile))
  return { url: descriptor["url"], scope: descriptor["scope"], developer: descriptor["actor"], tokens }
}

export interface SeedCall {
  readonly actor: string
  readonly action: string
  readonly idempotencyKey: string
  readonly input: Record<string, unknown>
}

export interface SeedDefinition {
  readonly kind: "question" | "dashboard"
  readonly id: string
  readonly payload: Json
}

/** The project's seed: its calls and its retained definitions, questions first. */
export function readSeed(root: string): { calls: SeedCall[]; definitions: SeedDefinition[] } {
  const callsPath = join(root, "seed/calls.json")
  const calls = existsSync(callsPath) ? (JSON.parse(readFileSync(callsPath, "utf8")) as { format?: unknown; calls?: SeedCall[] }) : { format: "norn-calls/1", calls: [] }
  if (calls.format !== "norn-calls/1" || !Array.isArray(calls.calls)) throw new ProjectError("cli.project.io", `${callsPath} is not a norn-calls/1 document.`, 3)
  const dir = join(root, "seed/definitions")
  const order = { question: 0, dashboard: 1 } as const
  const definitions = existsSync(dir)
    ? readdirSync(dir).filter((file) => file.endsWith(".json")).sort().map((file) => JSON.parse(readFileSync(join(dir, file), "utf8")) as SeedDefinition)
    : []
  for (const definition of definitions) {
    if (!(definition.kind in order) || typeof definition.id !== "string") throw new ProjectError("cli.project.io", `seed/definitions holds a document that is not a question or dashboard { kind, id, payload }.`, 3)
  }
  return { calls: calls.calls, definitions: definitions.sort((a, b) => order[a.kind] - order[b.kind]) }
}

/** What the seed did, or the cell's refusal that stopped it. */
export type SeedResult =
  | { readonly ok: true; readonly calls: number; readonly definitions: { readonly installed: string[]; readonly unchanged: string[] } }
  | { readonly ok: false; readonly step: string; readonly status: number; readonly refusal: unknown }

/** Send the seed to the running session; stop at the first refusal. */
export async function seed(session: SeedSession, plan: { calls: readonly SeedCall[]; definitions: readonly SeedDefinition[] }, fetcher: typeof fetch = fetch): Promise<SeedResult> {
  const query = new URLSearchParams({ orgId: session.scope.orgId, environmentId: session.scope.environmentId }).toString()
  const send = async (actor: string, method: string, path: string, body?: unknown) => {
    const token = session.tokens.get(actor)
    if (token === undefined) throw new ProjectError("cli.project.invalid_manifest", `The seed sends a call as ${actor}, who is not a local principal of this norn dev: name it in norn.json dev.principals.`)
    const response = await fetcher(`${session.url}${path}?${query}`, {
      method, headers: { authorization: `Bearer ${token}`, "content-type": "application/json", accept: "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    })
    const text = await response.text()
    return { status: response.status, body: text ? JSON.parse(text) as Record<string, any> : null }
  }
  for (const call of plan.calls) {
    const answer = await send(call.actor, "POST", `/v1/actions/${encodeURIComponent(call.action)}`, { input: call.input, idempotencyKey: call.idempotencyKey })
    if (answer.status !== 200) return { ok: false, step: `${call.action} as ${call.actor} (${call.idempotencyKey})`, status: answer.status, refusal: answer.body }
  }
  const installed: string[] = []
  const unchanged: string[] = []
  for (const { kind, id, payload } of plan.definitions) {
    const current = await send(session.developer, "GET", `/v1/definitions/${encodeURIComponent(id)}`)
    if (current.status === 200 && current.body?.["lifecycle"] !== "retired" && canonicalJson(current.body?.["payload"] as Json) === canonicalJson(payload)) {
      unchanged.push(id)
      continue
    }
    const admission = await send(session.developer, "POST", "/v1/definitions:admit", { kind, id, payload })
    if (admission.status !== 200 || admission.body?.["admitted"] !== true) return { ok: false, step: `admit ${kind} ${id}`, status: admission.status, refusal: admission.body }
    const sha256 = admission.body?.["restatementSha256"]
    const expectedVersion = current.status === 200 && typeof current.body?.["version"] === "number" ? { expectedVersion: current.body["version"] } : {}
    const install = await send(session.developer, "POST", "/v1/definitions:install", {
      kind, id, payload, ...(typeof sha256 === "string" ? { signoff: { restatementSha256: sha256 } } : {}), ...expectedVersion,
    })
    if (install.status !== 200) return { ok: false, step: `install ${kind} ${id}`, status: install.status, refusal: install.body }
    installed.push(id)
  }
  return { ok: true, calls: plan.calls.length, definitions: { installed, unchanged } }
}

/** `norn seed [--json]`, run in the project while its `norn dev` serves. */
export async function seedCli(args: readonly string[]): Promise<number> {
  const unknown = args.filter((arg) => arg !== "--json")
  if (unknown.length > 0) throw new ProjectError("cli.project.usage", "Usage: norn seed [--json], in a project whose norn dev is running.", 2)
  const { root } = readProject()
  const result = await seed(seedSession(root), readSeed(root))
  if (args.includes("--json")) process.stdout.write(`${JSON.stringify(result)}\n`)
  else if (result.ok) process.stdout.write(`seeded ${result.calls} call(s); definitions installed ${result.definitions.installed.length}, unchanged ${result.definitions.unchanged.length}\n`)
  else process.stdout.write(`refused at ${result.step} (${result.status}): ${JSON.stringify(result.refusal)}\n`)
  return result.ok ? 0 : 1
}
