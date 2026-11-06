import alchemy from "alchemy"
import { Vite } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"
import { deployTarget, scanState } from "./deploy-target.ts"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on the custom domains in STUDIO_DOMAINS
// (comma-separated); browser navigations to
// client-side routes fall back to index.html. Credentials come from
// CLOUDFLARE_* in the environment. State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
// ALCHEMY_STAGE must name the stage: unset, the deploy refuses rather than
// meaning production, and prod also needs NORN_DEPLOY_PROD=1.
// Each stage has its own Worker (production keeps the live one), and a fresh
// state store adopts its stage's existing Worker instead of failing to create it.
// `vite build` runs before this script (see the deploy script), never inside
// Alchemy: its build step hands the whole host environment to the command and
// records it in state. Alchemy only uploads dist.
const target = deployTarget(process.env)
const stateDir = process.env.ALCHEMY_STATE_DIR ?? ".alchemy"
// Alchemy keeps this app's state under <stateDir>/<APP>/; the post-deploy scan reads exactly that subtree.
const APP = "norn-studio"
const app = await alchemy(APP, {
  stage: target.stage,
  stateStore: (scope) => new FileSystemStateStore(scope, { rootDir: stateDir }),
})

export const studio = await Vite("studio", {
  name: target.worker,
  assets: "dist",
  // Built beforehand; an empty command makes Alchemy skip its build step entirely.
  build: "",
  spa: true,
  url: false,
  adopt: true,
  domains: target.domains.map((domainName) => ({ domainName, adopt: true })),
})

await app.finalize()

// No credential from the environment may end up in this app's state: fail the deploy if one does, naming only the variable.
// Only the app's own subtree is scanned; the folder may also hold the state password the assistant's secrets are encrypted with.
const scan = scanState(stateDir, APP, process.env)
if (Object.keys(scan.leaks).length > 0) {
  console.error(`Alchemy state holds credential values for: ${Object.keys(scan.leaks).sort().join(", ")}`)
  process.exit(1)
}
console.log(`State scan: ${scan.files} files, no credential values`)
