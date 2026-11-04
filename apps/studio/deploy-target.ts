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

/** The assistant's deploy-time inputs, or nothing when this deploy runs without it. */
export interface AgentDeploy {
  readonly gatewayUrl: string
  readonly gatewayToken: string
  /** The built checker ABI package (crates/norn-checker-wasm/scripts/build-pkg.sh). */
  readonly checkerPkg: string
}

export function agentDeploy(env: Readonly<Record<string, string | undefined>>): AgentDeploy | undefined {
  const { AI_GATEWAY_URL: gatewayUrl, AI_GATEWAY_TOKEN: gatewayToken, NORN_CHECKER_WASM_PKG: checkerPkg } = env
  if (gatewayUrl === undefined && gatewayToken === undefined) return undefined
  if (gatewayUrl === undefined || gatewayToken === undefined) throw new Error("AI_GATEWAY_URL and AI_GATEWAY_TOKEN are set together")
  if (!/^https:\/\/gateway\.ai\.cloudflare\.com\/v1\/[0-9a-f]{32}\/[a-z0-9-]+(\/compat)?\/?$/.test(gatewayUrl)) throw new Error("AI_GATEWAY_URL must be a Cloudflare AI Gateway URL")
  if (checkerPkg === undefined) throw new Error("NORN_CHECKER_WASM_PKG is required with the assistant")
  if (env.ALCHEMY_PASSWORD === undefined) throw new Error("ALCHEMY_PASSWORD is required to encrypt the assistant's secrets in state")
  return { gatewayUrl, gatewayToken, checkerPkg }
}
