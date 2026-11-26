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
import { isFetchableUrl, sanitizeUrlForDiagnostic, type FetchLike } from "./setup.ts"
import { compileInputPack } from "./worker.ts"

export const PLAN_PATH = "/v1/cell/definitions:plan"
export const APPLY_PATH = "/v1/cell/definitions:apply"
export const DEFAULT_HTTP_TIMEOUT_MS = 30_000
export const MAX_RESPONSE_BYTES = 16 * 1024 * 1024

type Doc = Record<string, any>

export interface Target {
  readonly url: string
  readonly org: string
  readonly env: string
  readonly headers: Readonly<Record<string, string>>
  readonly timeoutMs?: number
  readonly maxResponseBytes?: number
  readonly fetchImpl?: FetchLike
}

function sanitizeTargetUrlForDiagnostic(rawUrl: string): string {
  try {
    const parsed = new URL(rawUrl)
    const hadQuery = rawUrl.includes("?") || parsed.search !== ""
    const hadHash = rawUrl.includes("#") || parsed.hash !== ""
    parsed.search = ""
    parsed.hash = ""
    const base = sanitizeUrlForDiagnostic(parsed.toString())
    return `${base}${hadQuery ? "?<redacted>" : ""}${hadHash ? "#<redacted>" : ""}`
  } catch {
    return "[malformed URL redacted]"
  }
}

/**
 * Validate and normalize a target base URL for `norn plan|apply`.
 *
 * Rejects non-HTTP(S) schemes, URL userinfo, query strings (`?`), and
 * fragments (`#`) so appending API paths and query parameters cannot be
 * diverted or silently swallowed into an existing query or fragment.
 */
export function normalizeTargetUrl(rawUrl: string): string {
  if (rawUrl.includes("?") || rawUrl.includes("#")) {
    throw new Error(`refusing target URL with query or fragment: ${sanitizeTargetUrlForDiagnostic(rawUrl)}`)
  }
  let parsed: URL
  try {
    parsed = new URL(rawUrl)
  } catch {
    throw new Error(`invalid target URL: ${sanitizeTargetUrlForDiagnostic(rawUrl)}`)
  }
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
    throw new Error(`refusing target URL with unsupported protocol: ${sanitizeTargetUrlForDiagnostic(rawUrl)}`)
  }
  if (parsed.username !== "" || parsed.password !== "") {
    throw new Error(`refusing target URL with embedded credentials: ${sanitizeTargetUrlForDiagnostic(rawUrl)}`)
  }
  if (parsed.hostname.length === 0 || parsed.search !== "" || parsed.hash !== "") {
    throw new Error(`invalid target URL: ${sanitizeTargetUrlForDiagnostic(rawUrl)}`)
  }
  const cleanPath = parsed.pathname.replace(/\/+$/, "")
  return `${parsed.origin}${cleanPath}`
}

export function credentialsAllowed(url: string): boolean {
  if (url.includes("?") || url.includes("#")) return false
  if (!isFetchableUrl(url)) return false
  try {
    const parsed = new URL(url)
    return parsed.search === "" && parsed.hash === ""
  } catch {
    return false
  }
}

const COOKIE_MONTHS: Readonly<Record<string, number>> = {
  jan: 0,
  feb: 1,
  mar: 2,
  apr: 3,
  may: 4,
  jun: 5,
  jul: 6,
  aug: 7,
  sep: 8,
  oct: 9,
  nov: 10,
  dec: 11,
}

/**
 * Parse an RFC 6265 §5.1.1 cookie-date (`Expires` attribute value).
 *
 * Returns UTC epoch milliseconds, or `undefined` when the input does not match
 * the RFC 6265 grammar (for example `"0"` or `"not-a-date"`).
 */
export function parseCookieDate(input: string): number | undefined {
  const tokens = input.split(/[\x09\x20-\x2f\x3b-\x40\x5b-\x60\x7b-\x7e]+/).filter(Boolean)
  let hour: number | undefined
  let minute: number | undefined
  let second: number | undefined
  let dayOfMonth: number | undefined
  let month: number | undefined
  let year: number | undefined

  for (const token of tokens) {
    if (hour === undefined) {
      const m = /^(\d{1,2}):(\d{1,2}):(\d{1,2})(?:[^\d].*)?$/.exec(token)
      if (m) {
        hour = Number(m[1])
        minute = Number(m[2])
        second = Number(m[3])
        continue
      }
    }
    if (dayOfMonth === undefined) {
      const m = /^(\d{1,2})(?:[^\d].*)?$/.exec(token)
      if (m) {
        dayOfMonth = Number(m[1])
        continue
      }
    }
    if (month === undefined && token.length >= 3) {
      const key = token.slice(0, 3).toLowerCase()
      const parsedMonth = COOKIE_MONTHS[key]
      if (parsedMonth !== undefined) {
        month = parsedMonth
        continue
      }
    }
    if (year === undefined) {
      const m = /^(\d{2,4})(?:[^\d].*)?$/.exec(token)
      if (m) {
        year = Number(m[1])
        continue
      }
    }
  }

  if (
    hour === undefined
    || minute === undefined
    || second === undefined
    || dayOfMonth === undefined
    || month === undefined
    || year === undefined
  ) {
    return undefined
  }
  if (year >= 70 && year <= 99) year += 1900
  else if (year >= 0 && year <= 69) year += 2000
  if (year < 1601 || dayOfMonth < 1 || dayOfMonth > 31 || hour > 23 || minute > 59 || second > 59) {
    return undefined
  }
  const utc = Date.UTC(year, month, dayOfMonth, hour, minute, second)
  const check = new Date(utc)
  if (
    check.getUTCFullYear() !== year
    || check.getUTCMonth() !== month
    || check.getUTCDate() !== dayOfMonth
  ) {
    return undefined
  }
  return utc
}

/**
 * Deduplicate `Set-Cookie` headers into a single `Cookie` request header using
 * RFC 6265 last-write-wins semantics per cookie name and honoring `Max-Age`
 * (precedence over `Expires`) and past `Expires` deletions.
 */
export function extractCookieHeader(setCookies: ReadonlyArray<string>, nowMs = Date.now()): string {
  const jar = new Map<string, string>()
  for (const raw of setCookies) {
    const parts = raw.split(";")
    const pair = parts[0]?.trim() ?? ""
    const eq = pair.indexOf("=")
    if (eq <= 0) continue
    const name = pair.slice(0, eq).trim()
    const value = pair.slice(eq + 1).trim()
    if (!name || /[\r\n]/.test(name) || /[\r\n]/.test(value)) continue
    let maxAgeExpired: boolean | undefined
    let expiresExpired = false
    for (const rawAttr of parts.slice(1)) {
      const attr = rawAttr.trim()
      if (!attr) continue
      const attrEq = attr.indexOf("=")
      if (attrEq <= 0) continue
      const attrName = attr.slice(0, attrEq).trim().toLowerCase()
      const attrValue = attr.slice(attrEq + 1).trim()
      if (attrName === "max-age" && /^-?\d+$/.test(attrValue)) {
        maxAgeExpired = Number(attrValue) <= 0
      } else if (attrName === "expires") {
        const parsed = parseCookieDate(attrValue)
        if (parsed !== undefined) {
          expiresExpired = parsed <= nowMs
        }
      }
    }
    const expired = maxAgeExpired !== undefined ? maxAgeExpired : expiresExpired
    if (expired) {
      jar.delete(name)
    } else {
      jar.set(name, value)
    }
  }
  return [...jar.entries()].map(([name, value]) => `${name}=${value}`).join("; ")
}

function cancelBodyWithoutWaiting(response: Response): void {
  const body = response.body
  if (!body) return
  try {
    void body.cancel().catch(() => undefined)
  } catch {
    // Already locked or disturbed.
  }
}

export async function readBoundedText(
  response: Response,
  limit: number,
  label: string,
  deadlineMs: number,
): Promise<string> {
  const declaredRaw = response.headers.get("content-length")?.trim()
  if (declaredRaw && /^\d+$/.test(declaredRaw) && Number(declaredRaw) > limit) {
    cancelBodyWithoutWaiting(response)
    throw new Error(`${label} response exceeds size limit (${limit} bytes)`)
  }
  if (!response.body) return ""
  const reader = response.body.getReader()
  const decoder = new TextDecoder()
  let totalBytes = 0
  let text = ""
  try {
    for (;;) {
      const remaining = deadlineMs - Date.now()
      if (remaining <= 0) {
        throw new Error(`${label} timed out reading response`)
      }
      let timer: ReturnType<typeof setTimeout> | undefined
      const readResult = await Promise.race([
        reader.read(),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error(`${label} timed out reading response`)), remaining)
        }),
      ]).finally(() => {
        if (timer !== undefined) clearTimeout(timer)
      })
      if (readResult.done) break
      totalBytes += readResult.value.byteLength
      if (totalBytes > limit) {
        throw new Error(`${label} response exceeds size limit (${limit} bytes)`)
      }
      text += decoder.decode(readResult.value, { stream: true })
    }
    text += decoder.decode()
    return text
  } catch (error) {
    try {
      void reader.cancel().catch(() => undefined)
    } catch {
      // Ignore cancellation errors while surfacing the original failure.
    }
    throw error
  } finally {
    try {
      reader.releaseLock()
    } catch {
      // Reader already released or detached.
    }
  }
}

function resolveHttpTimeout(timeoutMs?: number): number {
  const value = timeoutMs ?? DEFAULT_HTTP_TIMEOUT_MS
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`invalid HTTP timeout (${value}ms)`)
  }
  return value
}

function resolveMaxResponseBytes(maxResponseBytes?: number): number {
  const value = maxResponseBytes ?? MAX_RESPONSE_BYTES
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`invalid max response size (${value} bytes)`)
  }
  return value
}

/** Sign in to Studio by email and password; the session cookie. */
async function signIn(origin: string, email: string, password: string, timeoutMs = DEFAULT_HTTP_TIMEOUT_MS): Promise<string> {
  const timeout = resolveHttpTimeout(timeoutMs)
  const response = await fetch(`${origin}/api/auth/sign-in/email`, {
    method: "POST", redirect: "manual",
    signal: AbortSignal.timeout(timeout),
    headers: { "content-type": "application/json", origin },
    body: JSON.stringify({ email, password }),
  })
  try {
    if (!response.ok) throw new Error(`Studio sign-in refused (${response.status})`)
    const cookieHeader = extractCookieHeader(response.headers.getSetCookie())
    if (cookieHeader.length === 0) throw new Error("Studio sign-in returned no session")
    return cookieHeader
  } finally {
    cancelBodyWithoutWaiting(response)
  }
}

export async function post(target: Target, path: string, body?: unknown): Promise<{ status: number; document: Doc }> {
  const baseUrl = normalizeTargetUrl(target.url)
  const timeout = resolveHttpTimeout(target.timeoutMs)
  const maxBytes = resolveMaxResponseBytes(target.maxResponseBytes)
  const fetchImpl = target.fetchImpl ?? fetch
  const deadline = Date.now() + timeout
  const query = new URLSearchParams({ orgId: target.org, environmentId: target.env })
  const response = await fetchImpl(`${baseUrl}${path}?${query}`, {
    redirect: "manual",
    signal: AbortSignal.timeout(timeout),
    ...(body === undefined
      ? { method: "GET", headers: { accept: "application/json", ...target.headers } }
      : { method: "POST", headers: { "content-type": "application/json", accept: "application/json", ...target.headers }, body: JSON.stringify(body) }),
  })
  if (response.status >= 300 && response.status < 400) {
    cancelBodyWithoutWaiting(response)
    throw new Error(`${path} answered with unexpected redirect (${response.status})`)
  }
  const text = await readBoundedText(response, maxBytes, path, deadline)
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
 */
export async function change(target: Target, command: "plan" | "apply", artifacts: ReadonlyArray<unknown>, json: boolean, expectedVersion?: number, definitions: ReadonlyArray<RetainedDefinitionDoc> = []): Promise<{ out: string[]; ok: boolean }> {
  const out: string[] = []
  const show = (status: number, document: Doc, summary: (d: Doc) => string) => {
    const ok = status >= 200 && status < 300
    out.push(json ? JSON.stringify(document) : ok ? summary(document) : refusalSummary(status, document))
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
 * definitions); any other directory or module compiles as a pack.
 */
export function loadPack(input: string, scope: { readonly orgId: string; readonly environmentId: string }): PackResult {
  return compileInputPack(input, { scope, overrideProjectScope: true })
}

function option(args: ReadonlyArray<string>, name: string): string | undefined {
  const index = args.indexOf(name)
  const value = index < 0 ? undefined : args[index + 1]
  return value === undefined || value.startsWith("--") ? undefined : value
}

const USAGE = "usage: norn plan|apply <pack-dir|file.ts> --url URL --org ID --env ID (--email EMAIL --password-env VAR | --token-env VAR) [--expected-version N] [--json]\n"

export async function main(command: "plan" | "apply", args: ReadonlyArray<string>): Promise<number> {
  const input = args[0]
  const rawUrl = option(args, "--url")
  const org = option(args, "--org")
  const env = option(args, "--env")
  if (!input || input.startsWith("--") || !rawUrl || !org || !env) { process.stderr.write(USAGE); return 2 }
  let url: string
  try {
    url = normalizeTargetUrl(rawUrl)
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`)
    return 2
  }
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
  const pack = loadPack(input, { orgId: org, environmentId: env })
  if (!pack.ok) { process.stdout.write(`${JSON.stringify(pack)}\n`); return 1 }
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
  const { out, ok } = await change({ url, org, env, headers }, command, artifacts, args.includes("--json"), expected, pack.retained)
  process.stdout.write(`${out.join("\n")}\n`)
  return ok ? 0 : 1
}
