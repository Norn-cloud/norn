import { writeFileSync } from "node:fs"
import { join } from "node:path"

// The self-hosted Studio: the same Worker, assets and bindings that alchemy.run.ts
// declares on Cloudflare, written as the wrangler.json `celld deploy` reads (#728, D55).
// celld has no secret store: a production fleet takes only string `vars`, kept in
// the fleet bucket, whose credentials already control the whole fleet. So the file
// carries secrets: it is generated at deploy time into a directory outside the
// repository, never committed, and written readable by its owner only.
// Magic-link email is absent: celld has no Email Service binding, so sign-in links
// refuse there, as they do on Cloudflare without a sender.

export interface CelldStudio {
  /** The stage; production stays on Cloudflare. */
  readonly stage: string
  /** The public origin Better Auth issues cookies and host tokens for. */
  readonly authUrl: string
  readonly authSecret: string
  readonly defaultOrg?: string
  /** The cell Worker's script name in the same fleet bucket, bound as NORN_HOST. */
  readonly host?: string
  /** The assistant's AI Gateway; without it the assistant is off. */
  readonly agent?: { readonly gatewayUrl: string; readonly gatewayToken: string }
}

const NAME = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/

/** Read the deploy's settings from the environment, refusing anything incomplete. */
export function celldStudio(env: Readonly<Record<string, string | undefined>>): CelldStudio {
  const stage = env.STUDIO_STAGE ?? ""
  if (!NAME.test(stage)) throw new Error(`STUDIO_STAGE must be lowercase letters, digits or inner dashes, got ${JSON.stringify(stage)}`)
  if (stage === "prod") throw new Error("Production Studio stays on Cloudflare; celld takes another stage")
  const authUrl = env.BETTER_AUTH_URL ?? ""
  if (!/^https:\/\/[a-z0-9.-]+$/.test(authUrl)) throw new Error("BETTER_AUTH_URL must be an https origin without a path")
  const authSecret = env.BETTER_AUTH_SECRET ?? ""
  if (authSecret.length < 32) throw new Error("BETTER_AUTH_SECRET must be at least 32 characters")
  if (env.EMAIL_FROM !== undefined) throw new Error("celld has no Email Service binding; unset EMAIL_FROM")
  const host = env.NORN_HOST_SERVICE
  if (host !== undefined && !NAME.test(host)) throw new Error(`NORN_HOST_SERVICE must be a script name, got ${JSON.stringify(host)}`)
  const { AI_GATEWAY_URL: gatewayUrl, AI_GATEWAY_TOKEN: gatewayToken } = env
  if ((gatewayUrl === undefined) !== (gatewayToken === undefined)) throw new Error("AI_GATEWAY_URL and AI_GATEWAY_TOKEN go together")
  return {
    stage, authUrl, authSecret,
    ...env.STUDIO_DEFAULT_ORG === undefined ? {} : { defaultOrg: env.STUDIO_DEFAULT_ORG },
    ...host === undefined ? {} : { host },
    ...gatewayUrl === undefined || gatewayToken === undefined ? {} : { agent: { gatewayUrl, gatewayToken } },
  }
}

/** The wrangler.json for one stage, binding for binding the Cloudflare deploy's. */
export function celldConfig(studio: CelldStudio): Record<string, unknown> {
  const vars: Record<string, string> = { BETTER_AUTH_SECRET: studio.authSecret, BETTER_AUTH_URL: studio.authUrl }
  if (studio.defaultOrg !== undefined) vars.STUDIO_DEFAULT_ORG = studio.defaultOrg
  if (studio.agent !== undefined) Object.assign(vars, { AI_GATEWAY_URL: studio.agent.gatewayUrl, AI_GATEWAY_TOKEN: studio.agent.gatewayToken })
  return {
    name: `norn-studio-${studio.stage}`,
    main: "worker/index.ts",
    compatibility_date: "2026-11-01",
    compatibility_flags: ["nodejs_compat"],
    assets: { directory: "dist", binding: "ASSETS", not_found_handling: "single-page-application", run_worker_first: ["/api/*"] },
    d1_databases: [{ binding: "AUTH_DB", database_name: `norn-studio-auth-${studio.stage}`, migrations_dir: "worker/migrations" }],
    ...studio.host === undefined ? {} : { services: [{ binding: "NORN_HOST", service: studio.host }] },
    ...studio.agent === undefined ? {} : {
      durable_objects: { bindings: [{ name: "AGENT_BUDGET", class_name: "AgentBudget" }] },
      migrations: [{ tag: "v1", new_sqlite_classes: ["AgentBudget"] }],
    },
    vars,
  }
}

// `node celld-config.ts <project-dir>`: write <project-dir>/wrangler.json (mode 0600).
if (import.meta.main) {
  const dir = process.argv[2]
  if (dir === undefined) throw new Error("usage: celld-config.ts <project-dir>")
  writeFileSync(join(dir, "wrangler.json"), `${JSON.stringify(celldConfig(celldStudio(process.env)), null, 2)}\n`, { mode: 0o600 })
  console.log(`wrote ${join(dir, "wrangler.json")} for norn-studio-${process.env.STUDIO_STAGE}`)
}
