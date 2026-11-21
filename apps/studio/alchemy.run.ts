import alchemy from "alchemy"
import { createCloudflareApi, D1Database, EmailSender, Vite, WorkerRef } from "alchemy/cloudflare"
import { FileSystemStateStore } from "alchemy/state"
import { authDeploy, cellHost, deployTarget, otelSettings, requireWorkerScript, scanState } from "./deploy-target.ts"
import { billingModeStatement } from "./worker/billing.ts"
import { polarConfig } from "./worker/polar-config.ts"

// The recorded Studio: `vite build` output (synthetic recorded engine documents
// only) served as static assets on the custom domains in STUDIO_DOMAINS
// (comma-separated); browser navigations to
// client-side routes fall back to index.html. Credentials are the Norn Cloudflare
// account's API token: CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID from Infisical prod
// /norn/cloudflare (never the Global API Key). State lives in ALCHEMY_STATE_DIR (outside
// the repository) or the ignored `.alchemy`.
// ALCHEMY_STAGE must name the stage: unset, the deploy refuses rather than
// meaning production, and prod also needs NORN_DEPLOY_PROD=1.
// Each stage has its own Worker (production keeps the live one), and a fresh
// state store adopts its stage's existing Worker instead of failing to create it.
// `vite build` runs before this script (see the deploy script), never inside
// Alchemy: its build step hands the whole host environment to the command and
// records it in state. Alchemy only uploads dist.
// Every stage runs Better Auth (worker/) with its own D1 database; the
// session secret becomes an encrypted Worker secret, never part of a bundle.
// Operators load BETTER_AUTH_SECRET and ALCHEMY_PASSWORD by name from the secret store
// (path /auth: NORN_STUDIO_<STAGE>_BETTER_AUTH_SECRET, NORN_STUDIO_<STAGE>_ALCHEMY_PASSWORD).
const target = deployTarget(process.env)
const auth = authDeploy(process.env, target)
// Preview binds the Norn cell Worker as NORN_HOST by script name; the script must already exist.
const host = cellHost(target)
if (host !== undefined) await requireWorkerScript(host, process.env)
// The scan below must see the secret's value, so keep a copy before taking it out of the environment.
const scanEnv = { ...process.env }
delete process.env.BETTER_AUTH_SECRET
// OpenTelemetry export (worker/telemetry.ts): off unless OTEL_EXPORTER_OTLP_ENDPOINT is set; its headers become a Worker secret.
const otel = otelSettings(process.env)
delete process.env.OTEL_EXPORTER_OTLP_HEADERS
// Hosted billing (worker/polar-config.ts): POLAR_ACCESS_TOKEN, POLAR_WEBHOOK_SECRET, POLAR_SERVER and
// POLAR_PLANS together, or none (billing off). The token and webhook secret become encrypted Worker secrets.
const billing = polarConfig(process.env)
delete process.env.POLAR_ACCESS_TOKEN
delete process.env.POLAR_WEBHOOK_SECRET
const stateDir = process.env.ALCHEMY_STATE_DIR ?? ".alchemy"
// Alchemy keeps this app's state under <stateDir>/<APP>/; the post-deploy scan reads exactly that subtree.
const APP = "norn-studio"
const app = await alchemy(APP, {
  stage: target.stage,
  password: process.env.ALCHEMY_PASSWORD,
  stateStore: (scope) => new FileSystemStateStore(scope, { rootDir: stateDir }),
})

const authDb = await D1Database("auth-db", {
  name: `norn-studio-auth-${target.stage}`,
  migrationsDir: "./worker/migrations",
  adopt: true,
})

// The database's billing mode (worker/billing.ts billingModeStatement), written once here, after the
// migrations, from this deployment's billing settings; no request writes it. Billing off writes off.
const cloudflare = await createCloudflareApi()
const mode = await cloudflare.post(`/accounts/${cloudflare.accountId}/d1/database/${authDb.id}/query`, { sql: billingModeStatement(billing !== undefined) })
if (!mode.ok) throw new Error(`writing the billing mode failed: ${mode.status} ${await mode.text()}`)
console.log(`Billing mode: ${billing === undefined ? "off" : "on"}`)

export const studio = await Vite("studio", {
  name: target.worker,
  assets: { directory: "dist", run_worker_first: ["/api/*"] },
  entrypoint: "./worker/index.ts",
  compatibilityDate: "2026-10-20",
  compatibilityFlags: ["nodejs_compat"],
  // Never write the Worker's secrets into the local wrangler.jsonc.
  wrangler: { secrets: false },
  bindings: {
    AUTH_DB: authDb,
    BETTER_AUTH_SECRET: alchemy.secret(auth.secret),
    BETTER_AUTH_URL: auth.url,
    // Cloudflare sets cf-connecting-ip itself; the Worker refuses auth without this (#1328).
    STUDIO_CLIENT_IP_HEADER: "cf-connecting-ip",
    ...auth.defaultOrg === undefined ? {} : { STUDIO_DEFAULT_ORG: auth.defaultOrg },
    ...host === undefined ? {} : { NORN_HOST: WorkerRef({ service: host }) },
    BETTER_AUTH_TRUSTED_ORIGINS: JSON.stringify(auth.origins),
    ...otel === undefined ? {} : {
      OTEL_EXPORTER_OTLP_ENDPOINT: otel.endpoint,
      ...otel.headers === undefined ? {} : { OTEL_EXPORTER_OTLP_HEADERS: alchemy.secret(otel.headers) },
      ...otel.resourceAttributes === undefined ? {} : { OTEL_RESOURCE_ATTRIBUTES: otel.resourceAttributes },
    },
    ...billing === undefined ? {} : {
      POLAR_ACCESS_TOKEN: alchemy.secret(billing.accessToken),
      POLAR_WEBHOOK_SECRET: alchemy.secret(billing.webhookSecret),
      POLAR_SERVER: billing.server,
      POLAR_PLANS: JSON.stringify(billing.table),
    },
    // Magic links need a Cloudflare Email Service sender on the domain; without one they refuse.
    ...auth.emailFrom === undefined ? {} : { EMAIL: EmailSender({ allowedSenderAddresses: [auth.emailFrom] }), EMAIL_FROM: auth.emailFrom },
  },
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
const scan = scanState(stateDir, APP, scanEnv)
if (Object.keys(scan.leaks).length > 0) {
  console.error(`Alchemy state holds credential values for: ${Object.keys(scan.leaks).sort().join(", ")}`)
  process.exit(1)
}
console.log(`State scan: ${scan.files} files, no credential values`)
