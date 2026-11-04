import { cpSync } from "node:fs"
import alchemy from "alchemy"
import { DurableObjectNamespace, Vite } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"
import { agentDeploy, deployTarget } from "./deploy-target.ts"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on the custom domains in STUDIO_DOMAINS
// (comma-separated); browser navigations to
// client-side routes fall back to index.html. Credentials come from
// CLOUDFLARE_* in the environment. State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
// Each stage has its own Worker (production keeps the live one), and a fresh
// state store adopts its stage's existing Worker instead of failing to create it.
// With AI_GATEWAY_URL/AI_GATEWAY_TOKEN set, the Worker also serves the
// assistant at /api/agent: the gateway credential becomes an encrypted Worker
// secret (never part of the browser bundle), and the checker runs in the Worker.
const target = deployTarget(process.env)
const agent = agentDeploy(process.env)
if (agent !== undefined) cpSync(agent.checkerPkg, new URL("./worker/checker/", import.meta.url), { recursive: true })
const app = await alchemy("norn-studio", {
  stage: target.stage,
  password: process.env.ALCHEMY_PASSWORD,
  stateStore: (scope) => new FileSystemStateStore(scope, { rootDir: process.env.ALCHEMY_STATE_DIR ?? ".alchemy" }),
})

export const studio = await Vite("studio", {
  name: target.worker,
  spa: true,
  url: false,
  adopt: true,
  domains: target.domains.map((domainName) => ({ domainName, adopt: true })),
  ...agent === undefined ? { assets: "dist" } : {
    assets: { directory: "dist", run_worker_first: ["/api/*"] },
    entrypoint: "./worker/index.ts",
    compatibilityDate: "2026-11-01",
    compatibilityFlags: ["nodejs_compat"],
    bindings: {
      AGENT_BUDGET: DurableObjectNamespace("agent-budget", { className: "AgentBudget", sqlite: true }),
      AI_GATEWAY_URL: alchemy.secret(agent.gatewayUrl),
      AI_GATEWAY_TOKEN: alchemy.secret(agent.gatewayToken),
    },
  },
})

await app.finalize()
