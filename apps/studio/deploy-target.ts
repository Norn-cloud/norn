/** Where one deploy goes: production keeps the live Worker and domains; any other stage gets its own Worker and must name its domains. */
export interface DeployTarget {
  readonly stage: string
  readonly worker: string
  readonly domains: readonly string[]
}

const PRODUCTION_DOMAINS = "studio.norn.cloud,studio-demo.norn.cloud"

export function deployTarget(env: Readonly<Record<string, string | undefined>>): DeployTarget {
  const stage = env.ALCHEMY_STAGE ?? "prod"
  if (!/^[a-z0-9-]+$/.test(stage)) throw new Error(`ALCHEMY_STAGE must be lowercase letters, digits or dashes, got ${JSON.stringify(stage)}`)
  const production = stage === "prod"
  const listed = env.STUDIO_DOMAINS ?? (production ? PRODUCTION_DOMAINS : undefined)
  if (listed === undefined) throw new Error(`STUDIO_DOMAINS is required for stage ${stage}`)
  const domains = listed.split(",").map((domain) => domain.trim())
  if (domains.some((domain) => !/^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(domain))) {
    throw new Error(`STUDIO_DOMAINS must be comma-separated hostnames, got ${JSON.stringify(listed)}`)
  }
  return { stage, worker: production ? "norn-studio-recorded" : `norn-studio-recorded-${stage}`, domains }
}
