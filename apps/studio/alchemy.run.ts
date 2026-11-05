import { existsSync, readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import alchemy from "alchemy"
import { Vite } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"
import { deployTarget, leakedSecrets } from "./deploy-target.ts"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on the custom domains in STUDIO_DOMAINS
// (comma-separated); browser navigations to
// client-side routes fall back to index.html. Credentials come from
// CLOUDFLARE_* in the environment. State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
// Each stage has its own Worker (production keeps the live one), and a fresh
// state store adopts its stage's existing Worker instead of failing to create it.
// `vite build` runs before this script (see the deploy script), never inside
// Alchemy: its build step hands the whole host environment to the command and
// records it in state. Alchemy only uploads dist.
const target = deployTarget(process.env)
const stateDir = process.env.ALCHEMY_STATE_DIR ?? ".alchemy"
const app = await alchemy("norn-studio", {
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

// No credential from the environment may end up in state: fail the deploy if one does, naming only the variable.
const files = !existsSync(stateDir) ? [] : (readdirSync(stateDir, { recursive: true, withFileTypes: true }) as import("node:fs").Dirent[])
  .filter((entry) => entry.isFile()).map((entry) => join(entry.parentPath, entry.name))
const leaks: Record<string, number> = {}
for (const file of files) for (const [name, count] of Object.entries(leakedSecrets(readFileSync(file, "utf8"), process.env))) leaks[name] = (leaks[name] ?? 0) + count
if (Object.keys(leaks).length > 0) {
  console.error(`Alchemy state holds credential values for: ${Object.keys(leaks).sort().join(", ")}`)
  process.exit(1)
}
console.log(`State scan: ${files.length} files, no credential values`)
