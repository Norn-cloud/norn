import { existsSync, readFileSync, readdirSync, type Dirent } from "node:fs"
import { join } from "node:path"
import { exportConfig } from "../norn-cell-do/otlp.mjs"

// Enables client disconnect signals for the read requests the Studio Worker forwards.
export const STUDIO_WORKER_COMPATIBILITY_FLAGS = ["nodejs_compat", "enable_request_signal"] as const

/**
 * Which Studio a stage runs:
 * - `recorded-demo`: production (studio, studio-demo). The static recorded Studio plus the assistant
 *   at /api/agent (AGENT_BUDGET Durable Object, class AgentBudget; AI_GATEWAY_URL/AI_GATEWAY_TOKEN),
 *   no sign-in. Built from 7009fd0e0b (branch studio/promote), live as norn-studio-recorded. This
 *   entry point (alchemy.run.ts) does not build it and refuses it: deploying the sign-in Studio
 *   over it would delete the AgentBudget class and put sign-in on studio and studio-demo.
 * - `sign-in`: every other stage. Better Auth with its own D1, invite-only sign-up.
 */
export type Recipe = "recorded-demo" | "sign-in"

/** Where one deploy goes: production keeps the live Worker and domains; any other stage gets its own Worker and must name its domains. */
export interface DeployTarget {
  readonly stage: string
  readonly worker: string
  readonly domains: readonly string[]
  readonly recipe: Recipe
}

/**
 * What production must keep, read from the live Worker's metadata (scripts/prod-parity.ts checks a
 * deployed Worker against it): the bindings by name and type, the Durable Object class, the
 * compatibility settings, and no sign-in bindings.
 */
export const PROD_RECORDED_DEMO = {
  worker: "norn-studio-recorded",
  compatibilityDate: "2026-11-01",
  compatibilityFlags: ["nodejs_compat"],
  bindings: { AGENT_BUDGET: "durable_object_namespace", AI_GATEWAY_URL: "secret_text", AI_GATEWAY_TOKEN: "secret_text", ASSETS: "assets" },
  durableObjectClass: { AGENT_BUDGET: "AgentBudget" },
  forbidden: ["AUTH_DB", "BETTER_AUTH_SECRET", "BETTER_AUTH_URL"],
} as const

export interface LiveBinding { readonly name: string; readonly type: string; readonly class_name?: string | null; readonly script_name?: string | null }
export interface LiveSettings { readonly compatibility_date?: string; readonly compatibility_flags?: readonly string[]; readonly bindings?: readonly LiveBinding[] }

/** How a deployed Worker differs from the production recipe; empty when it matches. */
export function prodParity(live: LiveSettings): string[] {
  const issues: string[] = []
  const bindings = live.bindings ?? []
  for (const [name, type] of Object.entries(PROD_RECORDED_DEMO.bindings)) {
    const found = bindings.find((binding) => binding.name === name)
    if (found === undefined) issues.push(`missing binding ${name} (${type})`)
    else if (found.type !== type) issues.push(`binding ${name} is ${found.type}, expected ${type}`)
  }
  for (const [name, className] of Object.entries(PROD_RECORDED_DEMO.durableObjectClass)) {
    const found = bindings.find((binding) => binding.name === name)
    if (found !== undefined && found.class_name !== className) issues.push(`${name} binds class ${found.class_name ?? "none"}, expected ${className}`)
    // The class must belong to the production Worker itself: a binding to another script's class
    // (script_name set to anything else) would keep budgets in a Worker this recipe does not own.
    if (found !== undefined && found.script_name != null && found.script_name !== PROD_RECORDED_DEMO.worker) issues.push(`${name} binds ${className} owned by ${found.script_name}, expected ${PROD_RECORDED_DEMO.worker} itself`)
  }
  for (const name of PROD_RECORDED_DEMO.forbidden) if (bindings.some((binding) => binding.name === name)) issues.push(`sign-in binding ${name} on production`)
  if (live.compatibility_date !== PROD_RECORDED_DEMO.compatibilityDate) issues.push(`compatibility date ${live.compatibility_date ?? "none"}, expected ${PROD_RECORDED_DEMO.compatibilityDate}`)
  for (const flag of PROD_RECORDED_DEMO.compatibilityFlags) if (!(live.compatibility_flags ?? []).includes(flag)) issues.push(`missing compatibility flag ${flag}`)
  return issues
}

const PRODUCTION_DOMAINS = "studio.norn.cloud,studio-demo.norn.cloud"
/** Production's custom domains; both must be attached to the production Worker. */
export const PROD_DOMAINS = PRODUCTION_DOMAINS.split(",")

export interface LiveDomain { readonly hostname: string; readonly service: string; readonly environment?: string; readonly enabled?: boolean }

/**
 * How the account's Worker custom domains differ from production's: each production hostname has
 * exactly one attachment, to the production Worker, in its `production` environment, and enabled.
 * A missing `environment` or `enabled` is a difference, not a pass.
 */
export function domainParity(domains: readonly LiveDomain[]): string[] {
  const issues: string[] = []
  for (const hostname of PROD_DOMAINS) {
    const found = domains.filter((domain) => domain.hostname === hostname)
    if (found.length === 0) issues.push(`custom domain ${hostname} is not attached`)
    else if (found.length > 1) issues.push(`custom domain ${hostname} has ${found.length} attachments, expected one`)
    else {
      const [domain] = found as [LiveDomain]
      if (domain.service !== PROD_RECORDED_DEMO.worker) issues.push(`custom domain ${hostname} is attached to ${domain.service}, expected ${PROD_RECORDED_DEMO.worker}`)
      if (domain.environment !== "production") issues.push(`custom domain ${hostname} serves environment ${domain.environment ?? "none"}, expected production`)
      if (domain.enabled !== true) issues.push(`custom domain ${hostname} is not enabled`)
    }
  }
  return issues
}

export interface LiveDeployment { readonly id: string; readonly created_on: string; readonly versions: readonly { readonly version_id: string; readonly percentage: number }[] }

/** The version serving all traffic: the newest deployment, with one version at 100%. Undefined during a split rollout or with no deployment. */
export function activeVersion(deployments: readonly LiveDeployment[]): string | undefined {
  const newest = [...deployments].sort((a, b) => Date.parse(b.created_on) - Date.parse(a.created_on))[0]
  if (newest === undefined || newest.versions.length !== 1 || newest.versions[0]!.percentage !== 100) return undefined
  return newest.versions[0]!.version_id
}

/**
 * The cutover probes count only if the Worker did not change under them: the deployments read
 * before and after both probes must have the same newest deployment, with one version at 100%.
 */
export function deploymentStableIssue(before: readonly LiveDeployment[], after: readonly LiveDeployment[]): string | undefined {
  const newest = (ds: readonly LiveDeployment[]) => [...ds].sort((a, b) => Date.parse(b.created_on) - Date.parse(a.created_on))[0]
  const [b, a] = [newest(before), newest(after)]
  if (b === undefined || a === undefined) return "no deployment before or after the probes"
  if (a.id !== b.id) return `the active deployment changed during the probes (${b.id} → ${a.id})`
  const version = activeVersion(after)
  if (version === undefined || version !== activeVersion(before)) return `deployment ${a.id} does not serve one version at 100% throughout the probes`
  return undefined
}

/** One event from a Workers Tail (trace-v1), as far as the parity check reads it. */
export interface TailEvent {
  readonly scriptName?: string
  readonly scriptVersion?: { readonly id?: string }
  readonly event?: { readonly request?: { readonly url?: string; readonly method?: string }; readonly response?: { readonly status?: number } }
}

/**
 * Ties the cutover probe to the destination Worker: among the events of a tail opened on the
 * production Worker, the probe's own request (a one-time nonce as its query key; Tail redacts query
 * values) must appear, run by that Worker at its active version, answering 400. A 400 from some other Worker, or from a stale version, fails.
 */
export function agentTailIssue(probeUrl: string, events: readonly TailEvent[], version: string | undefined): string | undefined {
  const mine = events.filter((event) => event.event?.request?.url === probeUrl && event.event.request.method === "POST")
  if (mine.length === 0) return `${probeUrl} never reached ${PROD_RECORDED_DEMO.worker} (no tail event for the probe)`
  if (version === undefined) return `${PROD_RECORDED_DEMO.worker} has no single active version at 100%`
  const wrong = mine.find((event) => event.scriptName !== PROD_RECORDED_DEMO.worker || event.scriptVersion?.id !== version)
  if (wrong !== undefined) return `${probeUrl} ran on ${wrong.scriptName ?? "an unnamed Worker"} version ${wrong.scriptVersion?.id ?? "unknown"}, expected ${PROD_RECORDED_DEMO.worker} version ${version}`
  const status = mine[0]!.event?.response?.status
  if (status !== 400) return `${probeUrl}: ${PROD_RECORDED_DEMO.worker} answered ${status ?? "nothing"} in its tail, not the assistant's 400`
  return undefined
}

/**
 * The assistant's own answer to an unauthenticated POST /api/agent with an empty body: 400 JSON
 * `{"error":"Missing session."}`. Anything else (the static Studio's 404, a 5xx, an edge page)
 * means /api/agent is not live on that host. Costs no model call.
 */
export function agentAnswerIssue(hostname: string, status: number, contentType: string | null, body: string): string | undefined {
  let error: unknown
  try {
    error = (JSON.parse(body) as { error?: unknown }).error
  } catch {
    error = undefined
  }
  if (status === 400 && /^application\/json/.test(contentType ?? "") && error === "Missing session.") return undefined
  return `https://${hostname}/api/agent answered ${status} ${contentType ?? ""}, not the assistant's 400 "Missing session."`
}

/** A hostname of at least two DNS labels, each 1–63 lowercase letters, digits or inner dashes. */
const hostname = (domain: string): boolean =>
  domain.length <= 253 && domain.split(".").length >= 2 && domain.split(".").every((label) => /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(label))

/**
 * The stage is always named: an unset ALCHEMY_STAGE refuses instead of meaning
 * production, and production also needs NORN_DEPLOY_PROD=1 to confirm it.
 */
export function deployTarget(env: Readonly<Record<string, string | undefined>>): DeployTarget {
  const stage = env.ALCHEMY_STAGE
  if (stage === undefined || stage === "") throw new Error("ALCHEMY_STAGE is required: name the stage to deploy (prod also needs NORN_DEPLOY_PROD=1)")
  if (!/^[a-z0-9-]+$/.test(stage)) throw new Error(`ALCHEMY_STAGE must be lowercase letters, digits or dashes, got ${JSON.stringify(stage)}`)
  const production = stage === "prod"
  if (production && env.NORN_DEPLOY_PROD !== "1") throw new Error("Deploying prod needs NORN_DEPLOY_PROD=1 to confirm it")
  const listed = env.STUDIO_DOMAINS ?? (production ? PRODUCTION_DOMAINS : undefined)
  if (listed === undefined) throw new Error(`STUDIO_DOMAINS is required for stage ${stage}`)
  const domains = listed.split(",").map((domain) => domain.trim())
  if (!domains.every(hostname)) throw new Error(`STUDIO_DOMAINS must be comma-separated hostnames, got ${JSON.stringify(listed)}`)
  const taken = domains.filter((domain) => !production && PRODUCTION_DOMAINS.split(",").includes(domain))
  if (taken.length > 0) throw new Error(`Stage ${stage} cannot take the production domains ${taken.join(", ")}`)
  return { stage, worker: production ? "norn-studio-recorded" : `norn-studio-recorded-${stage}`, domains, recipe: production ? "recorded-demo" : "sign-in" }
}

/** This entry point builds the sign-in Studio only; production's recipe is refused before anything runs. */
export function requireSignInRecipe(target: DeployTarget): void {
  if (target.recipe !== "sign-in") {
    throw new Error(`Stage ${target.stage} runs the recorded-demo Studio (${PROD_RECORDED_DEMO.worker}: /api/agent with the AgentBudget Durable Object, no sign-in), which this entry point does not build. Deploying the sign-in Studio there would delete the AgentBudget class and put sign-in on ${target.domains.join(" and ")}. Deploy production from the recorded-demo recipe (7009fd0e0b) and check it with scripts/prod-parity.ts.`)
  }
}

/** The Norn Cloudflare account; every deploy and script refuses any other. */
export const NORN_CLOUDFLARE_ACCOUNT_ID = "bcf71bcfcfd886ae15ece17dfe87228d"

/** Global API Key variables; any of them set refuses the run, whatever else is set. */
export const LEGACY_CLOUDFLARE_VARIABLES = ["CLOUDFLARE_API_KEY", "CLOUDFLARE_EMAIL", "CLOUDFLARE_ACCOUNT_EMAIL"] as const

export interface CloudflareCredentials { readonly accountId: string; readonly apiToken: string }

/**
 * The only Cloudflare credential Studio accepts: the Norn account's workers token,
 * CLOUDFLARE_WORKERS_API_TOKEN from Infisical prod /norn/cloudflare, with CLOUDFLARE_ACCOUNT_ID the
 * Norn account. Checked before any API call or spawn. The Global API Key variables refuse the run;
 * a CLOUDFLARE_API_TOKEN from elsewhere (the combined token, a shell value) is never used.
 */
export function cloudflareCredentials(env: Readonly<Record<string, string | undefined>>): CloudflareCredentials {
  const legacy = LEGACY_CLOUDFLARE_VARIABLES.filter((name) => env[name] !== undefined && env[name] !== "")
  if (legacy.length > 0) throw new Error(`${legacy.join(", ")} set: Studio deploys use the Norn workers token only (Infisical prod /norn/cloudflare); unset ${legacy.length === 1 ? "it" : "them"}`)
  const apiToken = env.CLOUDFLARE_WORKERS_API_TOKEN?.trim()
  if (!apiToken) throw new Error("CLOUDFLARE_WORKERS_API_TOKEN is required (Infisical prod /norn/cloudflare)")
  if (env.CLOUDFLARE_ACCOUNT_ID?.trim() !== NORN_CLOUDFLARE_ACCOUNT_ID) throw new Error("CLOUDFLARE_ACCOUNT_ID must be the Norn account (Infisical prod /norn/cloudflare)")
  return { accountId: NORN_CLOUDFLARE_ACCOUNT_ID, apiToken }
}

/**
 * Point this process's Cloudflare variables at the checked credentials, for libraries that read
 * them (Alchemy): CLOUDFLARE_API_TOKEN becomes the workers token, every other CLOUDFLARE_* goes.
 */
export function useCloudflareCredentials(env: Record<string, string | undefined>, credentials: CloudflareCredentials): void {
  for (const name of Object.keys(env)) if (name.startsWith("CLOUDFLARE_")) delete env[name]
  env.CLOUDFLARE_API_TOKEN = credentials.apiToken
  env.CLOUDFLARE_ACCOUNT_ID = credentials.accountId
}

/** Variables a spawned tool (Wrangler) may inherit besides its credentials: the toolchain only (not HOME). */
const TOOL_ENV = ["PATH", "LANG", "LC_ALL", "TERM", "TMPDIR", "NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE"] as const

/**
 * A sanitized environment for Wrangler: the toolchain variables, the checked token and account,
 * and a temporary HOME and XDG directories inside `scratchDir` (a fresh temporary directory the
 * caller removes), so a stored `wrangler login`, `~/.wrangler` or another account's cache is
 * never read. Nothing else from the parent: no Global API Key, no other secret, not its HOME.
 */
export function wranglerEnv(env: Readonly<Record<string, string | undefined>>, credentials: CloudflareCredentials, scratchDir: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const name of TOOL_ENV) { const value = env[name]; if (value !== undefined) out[name] = value }
  return {
    ...out,
    HOME: join(scratchDir, "home"),
    XDG_CONFIG_HOME: join(scratchDir, "config"),
    XDG_CACHE_HOME: join(scratchDir, "cache"),
    XDG_DATA_HOME: join(scratchDir, "data"),
    XDG_STATE_HOME: join(scratchDir, "state"),
    WRANGLER_SEND_METRICS: "false",
    CLOUDFLARE_API_TOKEN: credentials.apiToken,
    CLOUDFLARE_ACCOUNT_ID: credentials.accountId,
  }
}

/** Better Auth's deploy-time inputs. Every stage signs users in; there is no unauthenticated Studio. */
export interface AuthDeploy {
  /** The Worker secret Better Auth signs sessions with; never written to state in plain text. */
  readonly secret: string
  /** The origin users sign in on: the stage's first domain. */
  readonly url: string
  /** The exact HTTPS origins served by this stage. */
  readonly origins: readonly string[]
  /** Optional Cloudflare Email Service sender for magic links. */
  readonly emailFrom?: string
  /** Optional organization members' sessions start in, e.g. `org_demo` on preview. */
  readonly defaultOrg?: string
  /** The invited addresses; nobody else can create an account. */
  readonly signUpEmails: readonly string[]
}

export function authDeploy(env: Readonly<Record<string, string | undefined>>, target: DeployTarget): AuthDeploy {
  const secret = env.BETTER_AUTH_SECRET
  if (secret === undefined || secret.length < 32) throw new Error("BETTER_AUTH_SECRET (at least 32 characters) is required: every stage requires sign-in")
  if (!env.ALCHEMY_PASSWORD?.trim()) throw new Error("ALCHEMY_PASSWORD is required to encrypt the Worker secrets in state")
  const emailFrom = env.STUDIO_EMAIL_FROM
  if (emailFrom !== undefined && !/^[^@\s]+@[a-z0-9.-]+$/.test(emailFrom)) throw new Error("STUDIO_EMAIL_FROM must be an email address")
  const defaultOrg = env.STUDIO_DEFAULT_ORG
  if (defaultOrg !== undefined && !/^[A-Za-z0-9_-]{1,64}$/.test(defaultOrg)) throw new Error("STUDIO_DEFAULT_ORG must be an organization id")
  // Invite-only on every stage: without the list the Worker would take any sign-up.
  const signUpEmails = [...new Set((env.STUDIO_SIGNUP_EMAILS ?? "").split(",").map((email) => email.trim().toLowerCase()).filter(Boolean))]
  if (signUpEmails.length === 0) throw new Error(`STUDIO_SIGNUP_EMAILS is required: the invited addresses (Infisical prod /auth/NORN_STUDIO_${target.stage.toUpperCase().replace(/-/g, "_")}_SIGNUP_EMAILS); sign-up is invite-only`)
  if (!signUpEmails.every((email) => /^[^@\s,]+@[a-z0-9.-]+\.[a-z]{2,}$/.test(email))) throw new Error("STUDIO_SIGNUP_EMAILS must be comma-separated email addresses")
  return { secret, url: `https://${target.domains[0]}`, origins: target.domains.map((domain) => `https://${domain}`), ...emailFrom === undefined ? {} : { emailFrom }, ...defaultOrg === undefined ? {} : { defaultOrg }, signUpEmails }
}

/** The OpenTelemetry export a deploy passes to the Worker (worker/telemetry.ts); none when the endpoint is unset. */
export interface OtelSettings {
  readonly endpoint: string
  /** "name=value,..."; holds the collector's credentials, so it is deployed as a secret. */
  readonly headers?: string
  readonly resourceAttributes?: string
}

/** OTEL_EXPORTER_OTLP_ENDPOINT (an http(s) URL), its headers and resource attributes, or undefined when export is off. */
export function otelSettings(env: Readonly<Record<string, string | undefined>>): OtelSettings | undefined {
  const endpoint = env.OTEL_EXPORTER_OTLP_ENDPOINT
  if (endpoint === undefined || endpoint === "") {
    if (env.OTEL_EXPORTER_OTLP_HEADERS !== undefined) throw new Error("OTEL_EXPORTER_OTLP_HEADERS is set without OTEL_EXPORTER_OTLP_ENDPOINT")
    return undefined
  }
  let url: URL
  try { url = new URL(endpoint) } catch { throw new Error("OTEL_EXPORTER_OTLP_ENDPOINT must be an http(s) URL") }
  if ((url.protocol !== "https:" && url.protocol !== "http:") || url.username !== "" || url.password !== "") throw new Error("OTEL_EXPORTER_OTLP_ENDPOINT must be an http(s) URL without credentials; put them in OTEL_EXPORTER_OTLP_HEADERS")
  // OTEL-TLS-1: refuse at deploy what the Worker would silently turn off (malformed headers, credentials over plain HTTP to a remote host).
  if (exportConfig(env, "norn-studio") === null) throw new Error("OTEL_EXPORTER_OTLP_HEADERS must be name=value pairs of valid HTTP fields, and credentials need an https endpoint (or a loopback one)")
  return {
    endpoint,
    ...env.OTEL_EXPORTER_OTLP_HEADERS === undefined ? {} : { headers: env.OTEL_EXPORTER_OTLP_HEADERS },
    ...env.OTEL_RESOURCE_ATTRIBUTES === undefined ? {} : { resourceAttributes: env.OTEL_RESOURCE_ATTRIBUTES },
  }
}

/** Environment variable names whose values are credentials. */
// Not a bare AUTH: AUTH_URL or AUTH_MODE hold plain settings that would fail deploys falsely.
// OTLP_HEADERS carries the telemetry collector's credentials (otelSettings).
export const SECRET_NAME = /TOKEN|KEY|SECRET|PASSWORD|CREDENTIAL|PRIVATE|AUTH_CONFIG|OTLP_HEADERS/i

/**
 * Which credential-named variables' values appear in a text (a state file), by
 * name and count. Values shorter than 8 characters are too generic to match.
 * Callers print only this: names and counts, never values.
 */
export function leakedSecrets(text: string, env: Readonly<Record<string, string | undefined>>): Record<string, number> {
  const found: Record<string, number> = {}
  for (const [name, value] of Object.entries(env)) {
    if (!SECRET_NAME.test(name) || value === undefined || value.length < 8) continue
    // State is JSON: a value with quotes, backslashes or newlines appears escaped there.
    const escaped = JSON.stringify(value).slice(1, -1)
    const count = text.split(value).length - 1 + (escaped === value ? 0 : text.split(escaped).length - 1)
    if (count > 0) found[name] = count
  }
  return found
}

/**
 * Scan one app's Alchemy state (`<stateDir>/<app>/`, every file) for credential values
 * from the environment. Files beside it, such as the state password, are not the app's
 * state and are not scanned. Returns file count and leaks by variable name only.
 */
export function scanState(stateDir: string, app: string, env: Readonly<Record<string, string | undefined>>): { readonly files: number; readonly leaks: Record<string, number> } {
  const root = join(stateDir, app)
  const files = !existsSync(root) ? [] : (readdirSync(root, { recursive: true, withFileTypes: true }) as Dirent[])
    .filter((entry) => entry.isFile()).map((entry) => join(entry.parentPath, entry.name))
  const leaks: Record<string, number> = {}
  for (const file of files) for (const [name, count] of Object.entries(leakedSecrets(readFileSync(file, "utf8"), env))) leaks[name] = (leaks[name] ?? 0) + count
  return { files: files.length, leaks }
}

/** The Norn cell Worker each stage's Studio reaches as `NORN_HOST`; prod has none until promotion. */
export const CELL_HOSTS: Readonly<Record<string, string>> = { preview: "norn-cell-preview" }

export const cellHost = (target: DeployTarget): string | undefined => CELL_HOSTS[target.stage]

/**
 * Fail the deploy, naming the script, unless the cell Worker the stage binds to
 * already exists on the account: a binding to a missing script must not deploy.
 */
export async function requireWorkerScript(script: string, credentials: CloudflareCredentials, fetcher: typeof fetch = fetch): Promise<void> {
  // The workers token for the Norn account only (cloudflareCredentials), checked before the request.
  // Bounded (30 s, cloudflare-api.ts call): a stalled connection fails the deploy instead of hanging it.
  // Loaded here, not at the top: celld-deploy-cell imports this module without Studio's packages.
  const { workerSettingsStatus } = await import("./cloudflare-api.ts")
  // Decided from the status line alone, as before: a 2xx passes when its headers arrive.
  const status = await workerSettingsStatus({ credentials, fetch: fetcher }, script)
  if (status >= 200 && status < 300) return
  if (status === 404) throw new Error(`The cell Worker ${script} does not exist on this account; deploy it before binding Studio to it as NORN_HOST`)
  throw new Error(`Could not check the cell Worker ${script}: Cloudflare answered ${status}`)
}
