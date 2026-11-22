/**
 * Checks a deployed production Studio Worker against the recorded-demo recipe
 * (deploy-target.ts PROD_RECORDED_DEMO): its bindings by name and type, the AgentBudget Durable
 * Object class, the compatibility settings, and no sign-in bindings. Read-only: one GET.
 *
 *   bun scripts/prod-parity.ts --account norn     # CLOUDFLARE_WORKERS_API_TOKEN, CLOUDFLARE_ACCOUNT_ID
 *   bun scripts/prod-parity.ts --account legacy   # LEGACY_CLOUDFLARE_CLEANUP_TOKEN, LEGACY_CLOUDFLARE_ACCOUNT_ID
 *
 * Both sets come from Infisical prod /norn/cloudflare. Prints the differences, never a value;
 * exits 1 when there are any (or the Worker is missing).
 */
import { cloudflareCredentials, LEGACY_CLOUDFLARE_VARIABLES, prodParity, PROD_RECORDED_DEMO, type LiveSettings } from "../deploy-target.ts"

const which = process.argv[process.argv.indexOf("--account") + 1]
if (which !== "norn" && which !== "legacy") throw new Error("--account norn or --account legacy is required")
const legacySet = LEGACY_CLOUDFLARE_VARIABLES.filter((name) => process.env[name])
if (legacySet.length > 0) throw new Error(`${legacySet.join(", ")} set: unset ${legacySet.length === 1 ? "it" : "them"}`)
const { accountId, apiToken } = which === "norn"
  ? cloudflareCredentials(process.env)
  : { accountId: process.env.LEGACY_CLOUDFLARE_ACCOUNT_ID ?? "", apiToken: process.env.LEGACY_CLOUDFLARE_CLEANUP_TOKEN ?? "" }
if (!accountId || !apiToken) throw new Error("LEGACY_CLOUDFLARE_ACCOUNT_ID and LEGACY_CLOUDFLARE_CLEANUP_TOKEN are required (Infisical prod /norn/cloudflare)")

const response = await fetch(`https://api.cloudflare.com/client/v4/accounts/${accountId}/workers/scripts/${PROD_RECORDED_DEMO.worker}/settings`, { headers: { authorization: `Bearer ${apiToken}` }, signal: AbortSignal.timeout(30_000) })
if (response.status === 404) {
  console.log(`${which}: ${PROD_RECORDED_DEMO.worker} is not deployed`)
  process.exit(1)
}
if (!response.ok) throw new Error(`${which}: Cloudflare answered ${response.status}`)
const issues = prodParity(((await response.json()) as { result: LiveSettings }).result)
console.log(issues.length === 0 ? `${which}: ${PROD_RECORDED_DEMO.worker} matches the recorded-demo recipe` : `${which}: ${PROD_RECORDED_DEMO.worker} differs:\n  ${issues.join("\n  ")}`)
process.exit(issues.length === 0 ? 0 : 1)
