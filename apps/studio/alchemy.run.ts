import alchemy from "alchemy"
import { Vite } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on the custom domains in STUDIO_DOMAINS
// (comma-separated); browser navigations to
// client-side routes fall back to index.html. Credentials come from
// CLOUDFLARE_* in the environment. State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
const domains = (process.env.STUDIO_DOMAINS ?? "studio.norn.cloud,studio-demo.norn.cloud").split(",").map((domain) => domain.trim())
if (domains.some((domain) => !/^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(domain))) {
  throw new Error(`STUDIO_DOMAINS must be comma-separated hostnames, got ${JSON.stringify(process.env.STUDIO_DOMAINS)}`)
}

// A fixed stage, and adoption of the existing Worker, so a fresh state store
// (another operator, a new checkout) updates the deployed Worker instead of
// failing to create it.
const app = await alchemy("norn-studio", {
  stage: process.env.ALCHEMY_STAGE ?? "prod",
  stateStore: (scope) => new FileSystemStateStore(scope, { rootDir: process.env.ALCHEMY_STATE_DIR ?? ".alchemy" }),
})

export const studio = await Vite("studio", {
  name: "norn-studio-recorded",
  assets: "dist",
  spa: true,
  url: false,
  adopt: true,
  domains: domains.map((domainName) => ({ domainName, adopt: true })),
})

await app.finalize()
