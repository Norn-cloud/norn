/**
 * Checks a deployed production Studio Worker against the recorded-demo recipe
 * (deploy-target.ts PROD_RECORDED_DEMO): its bindings by name and type, the AgentBudget Durable
 * Object class and that the class belongs to this Worker, the compatibility settings, no sign-in
 * bindings, and both production custom domains attached to it. Read-only: two GETs.
 *
 * `--cutover` also asks each production hostname's /api/agent for the assistant's own answer to an
 * unauthenticated POST (400 "Missing session.", no model call): run it right after the custom
 * domains move, so /api/agent is proven live where users reach it.
 *
 *   bun scripts/prod-parity.ts --account norn [--cutover]  # CLOUDFLARE_WORKERS_API_TOKEN, CLOUDFLARE_ACCOUNT_ID
 *   bun scripts/prod-parity.ts --account legacy [--cutover] # LEGACY_CLOUDFLARE_CLEANUP_TOKEN, LEGACY_CLOUDFLARE_ACCOUNT_ID
 *
 * Both sets come from Infisical prod /norn/cloudflare. Prints the differences, never a value;
 * exits 1 when there are any (or the Worker is missing).
 */
import { agentAnswerIssue, cloudflareCredentials, domainParity, LEGACY_CLOUDFLARE_VARIABLES, PROD_DOMAINS, prodParity, PROD_RECORDED_DEMO, type LiveDomain, type LiveSettings } from "../deploy-target.ts"

const which = process.argv[process.argv.indexOf("--account") + 1]
if (which !== "norn" && which !== "legacy") throw new Error("--account norn or --account legacy is required")
const legacySet = LEGACY_CLOUDFLARE_VARIABLES.filter((name) => process.env[name])
if (legacySet.length > 0) throw new Error(`${legacySet.join(", ")} set: unset ${legacySet.length === 1 ? "it" : "them"}`)
const { accountId, apiToken } = which === "norn"
  ? cloudflareCredentials(process.env)
  : { accountId: process.env.LEGACY_CLOUDFLARE_ACCOUNT_ID ?? "", apiToken: process.env.LEGACY_CLOUDFLARE_CLEANUP_TOKEN ?? "" }
if (!accountId || !apiToken) throw new Error("LEGACY_CLOUDFLARE_ACCOUNT_ID and LEGACY_CLOUDFLARE_CLEANUP_TOKEN are required (Infisical prod /norn/cloudflare)")

const get = (path: string) => fetch(`https://api.cloudflare.com/client/v4/accounts/${accountId}${path}`, { headers: { authorization: `Bearer ${apiToken}` }, signal: AbortSignal.timeout(30_000) })
const response = await get(`/workers/scripts/${PROD_RECORDED_DEMO.worker}/settings`)
if (response.status === 404) {
  console.log(`${which}: ${PROD_RECORDED_DEMO.worker} is not deployed`)
  process.exit(1)
}
if (!response.ok) throw new Error(`${which}: Cloudflare answered ${response.status}`)
const issues = prodParity(((await response.json()) as { result: LiveSettings }).result)
const domains = await get("/workers/domains")
if (!domains.ok) throw new Error(`${which}: Cloudflare answered ${domains.status} for the custom domains`)
issues.push(...domainParity(((await domains.json()) as { result: LiveDomain[] }).result))
if (process.argv.includes("--cutover")) {
  for (const hostname of PROD_DOMAINS) {
    const agent = await fetch(`https://${hostname}/api/agent`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}", redirect: "manual", signal: AbortSignal.timeout(30_000) }).catch(() => undefined)
    const issue = agent === undefined ? `https://${hostname}/api/agent did not answer` : agentAnswerIssue(hostname, agent.status, agent.headers.get("content-type"), await agent.text())
    if (issue !== undefined) issues.push(issue)
  }
}
console.log(issues.length === 0 ? `${which}: ${PROD_RECORDED_DEMO.worker} matches the recorded-demo recipe${process.argv.includes("--cutover") ? ", and /api/agent answers on both domains" : ""}` : `${which}: ${PROD_RECORDED_DEMO.worker} differs:\n  ${issues.join("\n  ")}`)
process.exit(issues.length === 0 ? 0 : 1)
