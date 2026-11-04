import alchemy from "alchemy"
import { Vite } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"
import { deployTarget } from "./deploy-target.ts"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on the custom domains in STUDIO_DOMAINS
// (comma-separated); browser navigations to
// client-side routes fall back to index.html. Credentials come from
// CLOUDFLARE_* in the environment. State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
// Each stage has its own Worker (production keeps the live one), and a fresh
// state store adopts its stage's existing Worker instead of failing to create it.
const target = deployTarget(process.env)
const app = await alchemy("norn-studio", {
  stage: target.stage,
  stateStore: (scope) => new FileSystemStateStore(scope, { rootDir: process.env.ALCHEMY_STATE_DIR ?? ".alchemy" }),
})

export const studio = await Vite("studio", {
  name: target.worker,
  assets: "dist",
  spa: true,
  url: false,
  adopt: true,
  domains: target.domains.map((domainName) => ({ domainName, adopt: true })),
})

await app.finalize()
