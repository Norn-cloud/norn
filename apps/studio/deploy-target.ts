import { existsSync, readFileSync, readdirSync, type Dirent } from "node:fs"
import { join } from "node:path"

/** Where one deploy goes: production keeps the live Worker and domains; any other stage gets its own Worker and must name its domains. */
export interface DeployTarget {
  readonly stage: string
  readonly worker: string
  readonly domains: readonly string[]
}

const PRODUCTION_DOMAINS = "studio.norn.cloud,studio-demo.norn.cloud"

/** A hostname of at least two DNS labels, each 1–63 lowercase letters, digits or inner dashes. */
const hostname = (domain: string): boolean =>
  domain.length <= 253 && domain.split(".").length >= 2 && domain.split(".").every((label) => /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(label))

export function deployTarget(env: Readonly<Record<string, string | undefined>>): DeployTarget {
  const stage = env.ALCHEMY_STAGE ?? "prod"
  if (!/^[a-z0-9-]+$/.test(stage)) throw new Error(`ALCHEMY_STAGE must be lowercase letters, digits or dashes, got ${JSON.stringify(stage)}`)
  const production = stage === "prod"
  const listed = env.STUDIO_DOMAINS ?? (production ? PRODUCTION_DOMAINS : undefined)
  if (listed === undefined) throw new Error(`STUDIO_DOMAINS is required for stage ${stage}`)
  const domains = listed.split(",").map((domain) => domain.trim())
  if (!domains.every(hostname)) throw new Error(`STUDIO_DOMAINS must be comma-separated hostnames, got ${JSON.stringify(listed)}`)
  const taken = domains.filter((domain) => !production && PRODUCTION_DOMAINS.split(",").includes(domain))
  if (taken.length > 0) throw new Error(`Stage ${stage} cannot take the production domains ${taken.join(", ")}`)
  return { stage, worker: production ? "norn-studio-recorded" : `norn-studio-recorded-${stage}`, domains }
}

/** Better Auth's deploy-time inputs. Every stage signs users in; there is no unauthenticated Studio. */
export interface AuthDeploy {
  /** The Worker secret Better Auth signs sessions with; never written to state in plain text. */
  readonly secret: string
  /** The origin users sign in on: the stage's first domain. */
  readonly url: string
  /** Optional Cloudflare Email Service sender for magic links. */
  readonly emailFrom?: string
}

export function authDeploy(env: Readonly<Record<string, string | undefined>>, target: DeployTarget): AuthDeploy {
  const secret = env.BETTER_AUTH_SECRET
  if (secret === undefined || secret.length < 32) throw new Error("BETTER_AUTH_SECRET (at least 32 characters) is required: every stage requires sign-in")
  if (env.ALCHEMY_PASSWORD === undefined) throw new Error("ALCHEMY_PASSWORD is required to encrypt the Worker secrets in state")
  const emailFrom = env.STUDIO_EMAIL_FROM
  if (emailFrom !== undefined && !/^[^@\s]+@[a-z0-9.-]+$/.test(emailFrom)) throw new Error("STUDIO_EMAIL_FROM must be an email address")
  return { secret, url: `https://${target.domains[0]}`, ...emailFrom === undefined ? {} : { emailFrom } }
}

/** The preview stages' workspace map: the seeded demo workspace reads the recorded `analytics` cell. */
export const PREVIEW_WORKSPACE_ENVIRONMENTS: Readonly<Record<string, string>> = { org_demo: "analytics" }

/**
 * The Worker's STUDIO_WORKSPACE_ENVIRONMENTS for a stage, as JSON: the operator's map when set (validated: an object of
 * non-empty strings), else the preview map on a non-production stage, else none on production. Production never gets
 * the preview default: its workspaces have no data environment until the map names them.
 */
export function workspaceEnvironments(env: Readonly<Record<string, string | undefined>>, target: DeployTarget): string {
  const configured = env.STUDIO_WORKSPACE_ENVIRONMENTS
  if (configured === undefined) return JSON.stringify(target.stage === "prod" ? {} : PREVIEW_WORKSPACE_ENVIRONMENTS)
  let mapping: unknown
  try { mapping = JSON.parse(configured) } catch { throw new Error("STUDIO_WORKSPACE_ENVIRONMENTS must be a JSON object from workspace id to environment id") }
  if (typeof mapping !== "object" || mapping === null || Array.isArray(mapping) || !Object.values(mapping).every((value) => typeof value === "string" && value.trim() !== ""))
    throw new Error("STUDIO_WORKSPACE_ENVIRONMENTS must be a JSON object from workspace id to environment id")
  return JSON.stringify(mapping)
}

/** Environment variable names whose values are credentials. */
// Not a bare AUTH: AUTH_URL or AUTH_MODE hold plain settings that would fail deploys falsely.
export const SECRET_NAME = /TOKEN|KEY|SECRET|PASSWORD|CREDENTIAL|PRIVATE|AUTH_CONFIG/i

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
