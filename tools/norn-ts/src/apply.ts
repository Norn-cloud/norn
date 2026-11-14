/**
 * `norn plan|apply <pack-dir|file.ts>` (HOST-INSTALL-1): compile a pack from
 * its typed sources, as `norn compile` does, and send it to a running hosted
 * cell's `POST /v1/cell/definitions:plan` / `:apply`. The cell decides: it
 * admits the pack over its active definitions, refuses a broken one with the
 * checker's findings, and installs only for an owner or admin. `apply` plans
 * first and applies against the planned revision.
 *
 *   norn apply registry/packs/jewelry --url https://studio.example/api/host \
 *     --org ORG --env ENV --email you@example.com --password-env NORN_PASSWORD
 *
 * `--token-env VAR` sends a host bearer token instead (a host reached
 * directly). Credentials are read from the environment, never arguments.
 */
import { statSync } from "node:fs"
import { resolve } from "node:path"
import { compilePack } from "./pack.ts"

export const PLAN_PATH = "/v1/cell/definitions:plan"
export const APPLY_PATH = "/v1/cell/definitions:apply"

type Doc = Record<string, any>

export interface Target {
  readonly url: string
  readonly org: string
  readonly env: string
  readonly headers: Readonly<Record<string, string>>
}

/** Whether `url` may carry credentials: HTTPS, or plain HTTP to loopback. */
export function credentialsAllowed(url: string): boolean {
  const parsed = new URL(url)
  return parsed.protocol === "https:" || (parsed.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(parsed.hostname))
}

/** Sign in to Studio by email and password; the session cookie. */
async function signIn(origin: string, email: string, password: string): Promise<string> {
  const response = await fetch(`${origin}/api/auth/sign-in/email`, {
    method: "POST", redirect: "manual",
    headers: { "content-type": "application/json", origin },
    body: JSON.stringify({ email, password }),
  })
  if (!response.ok) throw new Error(`Studio sign-in refused (${response.status})`)
  const cookies = response.headers.getSetCookie().map((cookie) => cookie.split(";")[0]).filter(Boolean)
  if (cookies.length === 0) throw new Error("Studio sign-in returned no session")
  return cookies.join("; ")
}

export async function post(target: Target, path: string, body: unknown): Promise<{ status: number; document: Doc }> {
  const query = new URLSearchParams({ orgId: target.org, environmentId: target.env })
  const response = await fetch(`${target.url}${path}?${query}`, {
    method: "POST", redirect: "manual",
    headers: { "content-type": "application/json", accept: "application/json", ...target.headers },
    body: JSON.stringify(body),
  })
  const text = await response.text()
  try { return { status: response.status, document: JSON.parse(text) } }
  catch { throw new Error(`${path} answered ${response.status} with a non-JSON body`) }
}

function diagnostics(document: Doc): string[] {
  return (document.diagnostics ?? []).map((d: Doc) => `  ${d.severity} ${d.code}${d.span?.pointer ? ` at ${d.span.pointer}` : ""}: ${d.message}`)
}

export function planSummary(plan: Doc): string {
  const lines = [`plan over revision ${plan.baseVersion} -> ${plan.nextVersion}${plan.unchanged ? " (unchanged: the pack is already installed)" : ""}`]
  for (const a of plan.added ?? []) lines.push(`  + ${a.artifactId} (${a.kind} ${a.toVersion})`)
  for (const a of plan.changed ?? []) lines.push(`  ~ ${a.artifactId} (${a.kind} ${a.fromVersion} -> ${a.toVersion})`)
  for (const id of plan.same ?? []) lines.push(`  = ${id}`)
  if ((plan.newObjectTypes ?? []).length > 0) lines.push(`  new types: ${plan.newObjectTypes.join(", ")}`)
  lines.push(`  checked ${plan.checkedObjects} stored object(s) under the new laws`, ...diagnostics(plan))
  return lines.join("\n")
}

export function refusalSummary(status: number, refusal: Doc): string {
  return [`refused (${status}) ${refusal.code}: ${refusal.message}`, `  ${refusal.explanation ?? ""}`, ...diagnostics(refusal)].join("\n")
}

export function appliedSummary(applied: Doc): string {
  const verb = applied.outcome === "unchanged" ? "unchanged: already at" : "installed"
  return `${verb} revision ${applied.definitionsVersion} over ${applied.previousVersion} (receipt sha256 ${applied.receiptSha256}, commit ${applied.commitSeq})`
}

/** Plan, or plan then apply, `artifacts` on `target`; the lines to print and whether the cell accepted. */
export async function change(target: Target, command: "plan" | "apply", artifacts: ReadonlyArray<unknown>, json: boolean, expectedVersion?: number): Promise<{ out: string[]; ok: boolean }> {
  const out: string[] = []
  const show = (status: number, document: Doc, summary: (d: Doc) => string) => {
    const ok = status >= 200 && status < 300
    out.push(json ? JSON.stringify(document) : ok ? summary(document) : refusalSummary(status, document))
    return ok
  }
  let expected = expectedVersion
  if (command === "plan" || expected === undefined) {
    const planned = await post(target, PLAN_PATH, { artifacts })
    if (!show(planned.status, planned.document, planSummary)) return { out, ok: false }
    if (command === "plan") return { out, ok: true }
    expected = planned.document.baseVersion
  }
  const applied = await post(target, APPLY_PATH, { artifacts, expectedVersion: expected })
  return { out, ok: show(applied.status, applied.document, appliedSummary) }
}

function option(args: ReadonlyArray<string>, name: string): string | undefined {
  const index = args.indexOf(name)
  return index < 0 ? undefined : args[index + 1]
}

const USAGE = "usage: norn plan|apply <pack-dir|file.ts> --url URL --org ID --env ID (--email EMAIL --password-env VAR | --token-env VAR) [--expected-version N] [--json]\n"

export async function main(command: "plan" | "apply", args: ReadonlyArray<string>): Promise<number> {
  const input = args[0]
  const url = option(args, "--url")?.replace(/\/+$/, "")
  const org = option(args, "--org")
  const env = option(args, "--env")
  if (!input || input.startsWith("--") || !url || !org || !env) { process.stderr.write(USAGE); return 2 }
  const email = option(args, "--email")
  const passwordEnv = option(args, "--password-env")
  const tokenEnv = option(args, "--token-env")
  if ((email || tokenEnv) && !credentialsAllowed(url)) { process.stderr.write("credentials are sent only over HTTPS (or HTTP to loopback)\n"); return 2 }
  const pack = compilePack(resolve(input), { scope: { orgId: org, environmentId: env }, root: process.cwd(), ...(statSync(input).isFile() ? { modules: [resolve(input)] } : {}) })
  if (!pack.ok) { process.stdout.write(`${JSON.stringify(pack)}\n`); return 1 }
  const headers: Record<string, string> = {}
  if (tokenEnv) {
    const token = process.env[tokenEnv]
    if (!token) { process.stderr.write("token environment variable is unavailable\n"); return 2 }
    headers.authorization = `Bearer ${token}`
  } else if (email) {
    const password = passwordEnv ? process.env[passwordEnv] : undefined
    if (!password) { process.stderr.write("--password-env names no set variable\n"); return 2 }
    const origin = new URL(url).origin
    // Studio accepts a state change only from its own origin.
    headers.cookie = await signIn(origin, email, password)
    headers.origin = origin
  }
  const expected = option(args, "--expected-version")
  const { out, ok } = await change({ url, org, env, headers }, command, pack.artifacts, args.includes("--json"), expected === undefined ? undefined : Number(expected))
  process.stdout.write(`${out.join("\n")}\n`)
  return ok ? 0 : 1
}
