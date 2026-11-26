/**
 * `norn plan|apply <pack-dir|file.ts>` (HOST-INSTALL-1): compile a pack from
 * its typed sources, as `norn compile` does, and send it to a running hosted
 * cell's `POST /v1/cell/definitions:plan` / `:apply`. The cell decides: it
 * admits the pack over its active definitions, refuses a broken one with the
 * checker's findings, and installs only for an owner or admin. `apply` plans
 * first and applies against the planned revision.
 *
 *   norn apply fixtures/packs/batches --url https://studio.example/api/host \
 *     --org ORG --env ENV --email you@example.com --password-env NORN_PASSWORD
 *
 * `--token-env VAR` sends a host bearer token instead (a host reached
 * directly). Credentials are read from the environment, never arguments.
 *
 * A pack's retained definitions (`Norn.approval`) are not artifacts of the
 * set, but they travel with it (#1469): `plan` sends them with the set and
 * prints the server's words for each; `apply` signs those words and the cell
 * installs the set and its approvals in one transaction. An approval the
 * cell refuses fails the command and applies nothing.
 */
import { existsSync } from "node:fs"
import type { RetainedDefinitionDoc } from "./compiler/definitions.ts"
import { loadChecker } from "./checker-abi.ts"
import { bindSourceSchemas, checkPack, pendingSourceSchemas, type PackResult } from "./pack.ts"
import { isFetchableUrl } from "./setup.ts"
import { loadInputPack, omittedLines, type OmittedDefinition } from "./worker.ts"

export const PLAN_PATH = "/v1/cell/definitions:plan"
export const APPLY_PATH = "/v1/cell/definitions:apply"

type Doc = Record<string, any>

export interface Target {
  readonly url: string
  readonly org: string
  readonly env: string
  readonly headers: Readonly<Record<string, string>>
}

export function credentialsAllowed(url: string): boolean {
  return isFetchableUrl(url)
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

export async function post(target: Target, path: string, body?: unknown): Promise<{ status: number; document: Doc }> {
  const query = new URLSearchParams({ orgId: target.org, environmentId: target.env })
  const response = await fetch(`${target.url}${path}?${query}`, {
    redirect: "manual",
    ...(body === undefined
      ? { method: "GET", headers: { accept: "application/json", ...target.headers } }
      : { method: "POST", headers: { "content-type": "application/json", accept: "application/json", ...target.headers }, body: JSON.stringify(body) }),
  })
  const text = await response.text()
  try { return { status: response.status, document: JSON.parse(text) } }
  catch { throw new Error(`${path} answered ${response.status} with a non-JSON body`) }
}

function diagnostics(document: Doc): string[] {
  return (document.diagnostics ?? []).map((d: Doc) => `  ${d.severity} ${d.code}${d.span?.pointer ? ` at ${d.span.pointer}` : ""}: ${d.message}`)
}

function planSummary(plan: Doc): string {
  const lines = [`plan over revision ${plan.baseVersion} -> ${plan.nextVersion}${plan.unchanged ? " (unchanged: the pack is already installed)" : ""}`]
  for (const a of plan.added ?? []) lines.push(`  + ${a.artifactId} (${a.kind} ${a.toVersion})`)
  for (const a of plan.changed ?? []) lines.push(`  ~ ${a.artifactId} (${a.kind} ${a.fromVersion} -> ${a.toVersion})`)
  for (const id of plan.same ?? []) lines.push(`  = ${id}`)
  if ((plan.newObjectTypes ?? []).length > 0) lines.push(`  new types: ${plan.newObjectTypes.join(", ")}`)
  lines.push(`  checked ${plan.checkedObjects} stored object(s) under the new laws`, ...diagnostics(plan))
  return lines.join("\n")
}

function refusalSummary(status: number, refusal: Doc): string {
  return [`refused (${status}) ${refusal.code}: ${refusal.message}`, `  ${refusal.explanation ?? ""}`, ...diagnostics(refusal)].join("\n")
}

function appliedSummary(applied: Doc): string {
  const verb = applied.outcome === "unchanged" ? "unchanged: already at" : "installed"
  return `${verb} revision ${applied.definitionsVersion} over ${applied.previousVersion} (receipt sha256 ${applied.receiptSha256}, commit ${applied.commitSeq})`
}

/** The approval lines of a plan or apply: unchanged, or the words signed. */
function approvalLines(document: Doc, verb: "signs" | "signed"): string[] {
  return (document.approvals ?? []).map((a: Doc) => a.unchanged
    ? `  = approval ${a.id} (unchanged)`
    : [`  + approval ${a.id}${a.fromRevision === undefined ? "" : ` over revision ${a.fromRevision}`}, ${verb}:`, ...String(a.restatement).split("\n").map((line) => `      ${line}`)].join("\n"))
}

/**
 * Plan, or plan then apply, `artifacts` on `target` with the pack's retained
 * `definitions` (its approvals); the lines to print and whether the cell
 * accepted all of it.
 *
 * #1469: the approvals travel in the same requests as the set. The plan
 * admits them against the submitted set's actions and returns the words to
 * sign; the apply carries each sign-off, and the cell installs the set and
 * its approvals in one transaction. A refused approval, a stale revision or
 * a failure anywhere publishes nothing, so a new action is never live
 * without its gate.
 *
 * `omitted` names the source questions and dashboards not sent (a hosted cell
 * installs them only through the definitions API): a note line each, and an
 * `omittedDefinitions` list in every JSON document printed.
 */
export async function change(target: Target, command: "plan" | "apply", artifacts: ReadonlyArray<unknown>, json: boolean, expectedVersion?: number, definitions: ReadonlyArray<RetainedDefinitionDoc> = [], omitted: ReadonlyArray<OmittedDefinition> = []): Promise<{ out: string[]; ok: boolean }> {
  const out: string[] = json ? [] : omittedLines(omitted)
  const show = (status: number, document: Doc, summary: (d: Doc) => string) => {
    const ok = status >= 200 && status < 300
    out.push(json ? JSON.stringify(omitted.length > 0 ? { ...document, omittedDefinitions: omitted } : document) : ok ? summary(document) : refusalSummary(status, document))
    return ok
  }
  const approvals = definitions.map((definition) => ({ kind: definition.kind, id: definition.id, payload: definition.payload }))
  const body = (extra: ReadonlyMap<string, Doc>) => ({ artifacts, ...(approvals.length > 0 ? { approvals: approvals.map((approval) => ({ ...approval, ...extra.get(approval.id) })) } : {}) })
  let expected = expectedVersion
  let planned: Doc | undefined
  if (command === "plan" || expected === undefined || approvals.length > 0) {
    const plan = await post(target, PLAN_PATH, body(new Map()))
    if (!show(plan.status, plan.document, (d) => [planSummary(d), ...approvalLines(d, "signs")].join("\n"))) return { out, ok: false }
    if (command === "plan") return { out, ok: true }
    planned = plan.document
    expected ??= planned.baseVersion
    if (typeof expected !== "number") throw new Error("the plan names no base revision")
  }
  // Each approval is signed with the words the plan returned for it.
  const signed = new Map<string, Doc>((planned?.approvals ?? []).map((a: Doc) =>
    [String(a.id), { expectedVersion: a.fromRevision ?? null, signoff: { restatementSha256: a.restatementSha256 } }]))
  const applied = await post(target, APPLY_PATH, { ...body(signed), expectedVersion: expected })
  return { out, ok: show(applied.status, applied.document, (d) => [appliedSummary(d), ...approvalLines(d.plan ?? {}, "signed")].join("\n")) }
}

/**
 * The pack `input` names, scoped to the target cell: a project directory (one
 * holding norn.json) is read by the shared project loader, exactly as `norn
 * dev` and `norn compile` read it (its manifest's sources, JSON artifacts and
 * definitions); any other directory or module compiles as a pack. `omitted`
 * names the project's source questions and dashboards, which are not sent.
 */
export function loadPack(input: string, scope: { readonly orgId: string; readonly environmentId: string }): { readonly pack: PackResult; readonly omitted: OmittedDefinition[] } {
  return loadInputPack(input, { scope, overrideProjectScope: true })
}

function option(args: ReadonlyArray<string>, name: string): string | undefined {
  const index = args.indexOf(name)
  const value = index < 0 ? undefined : args[index + 1]
  return value === undefined || value.startsWith("--") ? undefined : value
}

const USAGE = "usage: norn plan|apply <pack-dir|file.ts> --url URL --org ID --env ID (--email EMAIL --password-env VAR | --token-env VAR) [--expected-version N] [--json]\n"

export async function main(command: "plan" | "apply", args: ReadonlyArray<string>): Promise<number> {
  const input = args[0]
  const url = option(args, "--url")?.replace(/\/+$/, "")
  const org = option(args, "--org")
  const env = option(args, "--env")
  if (!input || input.startsWith("--") || !url || !org || !env) { process.stderr.write(USAGE); return 2 }
  if (!existsSync(input)) { process.stderr.write(`${command}: ${input} does not exist\n`); return 2 }
  let expected: number | undefined
  if (args.includes("--expected-version")) {
    const raw = option(args, "--expected-version")
    if (raw === undefined || !/^\d+$/.test(raw) || !Number.isSafeInteger(Number(raw))) {
      process.stderr.write("--expected-version requires a non-negative integer\n")
      return 2
    }
    expected = Number(raw)
  }
  const email = option(args, "--email")
  const passwordEnv = option(args, "--password-env")
  const tokenEnv = option(args, "--token-env")
  if ((email || tokenEnv) && !credentialsAllowed(url)) { process.stderr.write("credentials are sent only over HTTPS (or HTTP to loopback)\n"); return 2 }
  const { pack, omitted } = loadPack(input, { orgId: org, environmentId: env })
  if (!pack.ok) { process.stdout.write(`${JSON.stringify(pack)}\n`); return 1 }
  // Printed before anything is sent, so a stop below still says what was left out.
  if (args.includes("--json")) for (const line of omittedLines(omitted)) process.stderr.write(`${line}\n`)
  // MAPPING-SOURCE-REF-1: a mapping bound to a source schema of the pack by
  // reference is pinned with the checker's hash before the pack is sent.
  let artifacts = pack.artifacts
  if (pendingSourceSchemas(artifacts)) {
    const checker = await loadChecker().catch(() => undefined)
    if (!checker) { process.stderr.write("a mapping binds a source schema by reference; set NORN_CHECKER_WASM_PKG so the checker can pin its hash\n"); return 2 }
    artifacts = bindSourceSchemas(artifacts, checkPack(checker, artifacts, pack.baseArtifacts).receipt)
  }
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
  const { out, ok } = await change({ url, org, env, headers }, command, artifacts, args.includes("--json"), expected, pack.retained, omitted)
  process.stdout.write(`${out.join("\n")}\n`)
  return ok ? 0 : 1
}
