/**
 * `norn seed`: replay a project's seed (`seed/calls.json`, `norn-calls/1`, as
 * `norn init --template` lays it out) into its running `norn dev`, through the
 * cell's own action route. Each call is sent by its `actor`, a local principal
 * of the session (the developer, the reviewer or a named one,
 * DEV-PRINCIPALS-1), with its own idempotency key. The cell decides every call
 * by the pack's access policy, laws and lifecycles; a call it refuses stops the
 * seed with the cell's refusal. Sending the seed again replays: the same keys
 * commit nothing new. Questions and dashboards are sources, which `norn dev`
 * installs itself (DEV-READ-DEFINITIONS-1).
 *
 * It decides nothing and holds no policy: the dev host is the only authority.
 */
import { existsSync, readFileSync, realpathSync } from "node:fs"
import { isAbsolute, join, relative, resolve, sep } from "node:path"
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

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)

/** Whether `path`, with every symlink resolved, is inside the project root `realRoot`. */
function insideProject(realRoot: string, path: string): boolean {
  let real: string
  try { real = realpathSync(path) }
  catch { return false } // a dangling link resolves nowhere, so not inside the project
  return real.startsWith(realRoot + sep)
}

/**
 * The running `norn dev` of `root`, as its descriptor (`norn-dev/1`) names it.
 * A token file is read only when it resolves, symlinks and all, inside the
 * project: a bearer token is never read from anywhere else.
 */
export function seedSession(root: string): SeedSession {
  const path = join(root, ".norn/dev.json")
  if (!existsSync(path)) throw new ProjectError("cli.project.io", `No running norn dev in ${root}: start it there first (${path} is missing).`, 3)
  const invalid = (why: string) => new ProjectError("cli.project.io", `${path} is not a loopback norn-dev/1 descriptor: ${why}.`, 3)
  let descriptor: unknown
  try { descriptor = JSON.parse(readFileSync(path, "utf8")) }
  catch { throw invalid("it is not JSON") }
  if (!isRecord(descriptor) || descriptor["format"] !== "norn-dev/1") throw invalid("its format is not norn-dev/1")
  const { url, scope, actor } = descriptor
  if (typeof url !== "string" || !LOOPBACK_URL.test(url)) throw invalid("its url is not a loopback address")
  if (!isRecord(scope) || typeof scope["orgId"] !== "string" || typeof scope["environmentId"] !== "string") throw invalid("it names no scope")
  if (typeof actor !== "string" || actor === "") throw invalid("it names no actor")
  const realRoot = realpathSync(root)
  const token = (file: unknown): string => {
    if (typeof file !== "string") throw invalid("a session names no token file")
    const at = resolve(root, file)
    const inside = relative(resolve(root), at)
    if (inside.startsWith("..") || isAbsolute(inside)) throw new ProjectError("cli.project.io", `The token file ${file} is outside the project.`, 3)
    if (!existsSync(at)) throw new ProjectError("cli.project.io", `The token file ${file} is missing: restart norn dev in ${root}.`, 3)
    if (!insideProject(realRoot, at)) throw new ProjectError("cli.project.io", `The token file ${file} is outside the project.`, 3)
    return readFileSync(at, "utf8").trim()
  }
  const sessionOf = (value: unknown): { actor: string; tokenFile: unknown } => {
    if (!isRecord(value) || typeof value["actor"] !== "string" || value["actor"] === "") throw invalid("a session names no actor")
    return { actor: value["actor"], tokenFile: value["tokenFile"] }
  }
  const tokens = new Map<string, string>([[actor, token(descriptor["tokenFile"])]])
  if (descriptor["reviewer"] !== undefined) {
    const reviewer = sessionOf(descriptor["reviewer"])
    tokens.set(reviewer.actor, token(reviewer.tokenFile))
  }
  const principals = descriptor["principals"] ?? []
  if (!Array.isArray(principals)) throw invalid("its principals are not a list")
  for (const principal of principals.map(sessionOf)) tokens.set(principal.actor, token(principal.tokenFile))
  return { url, scope: { orgId: scope["orgId"], environmentId: scope["environmentId"] }, developer: actor, tokens }
}

export interface SeedCall {
  readonly actor: string
  readonly action: string
  readonly idempotencyKey: string
  readonly input: Record<string, unknown>
}

/** The project's seed calls. */
export function readSeed(root: string): SeedCall[] {
  const path = join(root, "seed/calls.json")
  if (!existsSync(path)) return []
  const calls = JSON.parse(readFileSync(path, "utf8")) as { format?: unknown; calls?: SeedCall[] }
  if (calls.format !== "norn-calls/1" || !Array.isArray(calls.calls)) throw new ProjectError("cli.project.io", `${path} is not a norn-calls/1 document.`, 3)
  return calls.calls
}

/** What the seed did, or the cell's refusal that stopped it. */
export type SeedResult =
  | { readonly ok: true; readonly calls: number }
  | { readonly ok: false; readonly step: string; readonly status: number; readonly refusal: unknown }

/** Send each call as its actor; stop at the first refusal. */
export async function seed(session: SeedSession, calls: readonly SeedCall[], fetcher: typeof fetch = fetch): Promise<SeedResult> {
  const query = new URLSearchParams({ orgId: session.scope.orgId, environmentId: session.scope.environmentId }).toString()
  for (const call of calls) {
    const token = session.tokens.get(call.actor)
    if (token === undefined) throw new ProjectError("cli.project.invalid_manifest", `The seed sends a call as ${call.actor}, who is not a local principal of this norn dev: name it in norn.json dev.principals.`)
    const response = await fetcher(`${session.url}/v1/actions/${encodeURIComponent(call.action)}?${query}`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ input: call.input, idempotencyKey: call.idempotencyKey }),
    })
    const text = await response.text()
    if (response.status !== 200) {
      let refusal: unknown = text === "" ? null : text
      try { if (text !== "") refusal = JSON.parse(text) } catch { /* not JSON: the body as the server sent it */ }
      return { ok: false, step: `${call.action} as ${call.actor} (${call.idempotencyKey})`, status: response.status, refusal }
    }
  }
  return { ok: true, calls: calls.length }
}

/** `norn seed [--json]`, run in the project while its `norn dev` serves. */
export async function seedCli(args: readonly string[]): Promise<number> {
  const unknown = args.filter((arg) => arg !== "--json")
  if (unknown.length > 0) throw new ProjectError("cli.project.usage", "Usage: norn seed [--json], in a project whose norn dev is running.", 2)
  const { root } = readProject()
  const result = await seed(seedSession(root), readSeed(root))
  if (args.includes("--json")) process.stdout.write(`${JSON.stringify(result)}\n`)
  else if (result.ok) process.stdout.write(`seeded ${result.calls} call(s)\n`)
  else process.stdout.write(`refused at ${result.step} (${result.status}): ${JSON.stringify(result.refusal)}\n`)
  return result.ok ? 0 : 1
}
