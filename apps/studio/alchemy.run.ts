import alchemy from "alchemy"
import { Vite } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on one custom domain; browser navigations to
// client-side routes fall back to index.html. Credentials come from
// CLOUDFLARE_* in the environment. State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
const app = await alchemy("norn-studio", {
  stateStore: (scope) => new FileSystemStateStore(scope, { rootDir: process.env.ALCHEMY_STATE_DIR ?? ".alchemy" }),
})

export const studio = await Vite("studio", {
  name: "norn-studio-recorded",
  assets: "dist",
  spa: true,
  url: false,
  domains: [process.env.STUDIO_DOMAIN ?? "studio-demo.norn.cloud"],
})

await app.finalize()
