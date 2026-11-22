/**
 * Checks a deployed production Studio Worker against the recorded-demo recipe
 * (deploy-target.ts PROD_RECORDED_DEMO): its bindings by name and type, the AgentBudget Durable
 * Object class and that the class belongs to this Worker, the compatibility settings, no sign-in
 * bindings, and both production custom domains attached to it. Read-only GETs; `--cutover` also
 * opens one Workers Tail per host and deletes it.
 *
 * `--cutover` also asks each production hostname's /api/agent for the assistant's own answer to an
 * unauthenticated POST (400 "Missing session.", no model call), and ties that answer to the
 * destination Worker: a Workers Tail opened on the production Worker must show the probe's own
 * request (a one-time nonce as its query key) run by that Worker at its active deployment version. A 400
 * from another Worker or a stale version fails. Run it right after the custom domains move. The
 * tail is deleted afterwards.
 *
 *   bun scripts/prod-parity.ts --account norn [--cutover]  # CLOUDFLARE_WORKERS_API_TOKEN, CLOUDFLARE_ACCOUNT_ID
 *   bun scripts/prod-parity.ts --account legacy [--cutover] # LEGACY_CLOUDFLARE_CLEANUP_TOKEN, LEGACY_CLOUDFLARE_ACCOUNT_ID
 *
 * Both sets come from Infisical prod /norn/cloudflare. Prints the differences, never a value;
 * the tail is read in memory only (never printed: its events carry request headers);
 * exits 1 when there are any (or the Worker is missing).
 */
import { activeVersion, agentAnswerIssue, agentTailIssue, cloudflareCredentials, domainParity, LEGACY_CLOUDFLARE_VARIABLES, PROD_DOMAINS, prodParity, PROD_RECORDED_DEMO, type LiveDeployment, type LiveDomain, type LiveSettings, type TailEvent } from "../deploy-target.ts"

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
/** Opens a tail on the production Worker, runs `probe`, and returns the tail's events (the tail is deleted). */
async function tailDuring(probeUrl: string, probe: () => Promise<void>): Promise<TailEvent[]> {
  const base = `/workers/scripts/${PROD_RECORDED_DEMO.worker}/tails`
  const created = await fetch(`https://api.cloudflare.com/client/v4/accounts/${accountId}${base}`, { method: "POST", headers: { authorization: `Bearer ${apiToken}`, "content-type": "application/json" }, body: "{}", signal: AbortSignal.timeout(30_000) })
  if (!created.ok) throw new Error(`${which}: Cloudflare answered ${created.status} opening a tail`)
  const tail = ((await created.json()) as { result: { id: string; url: string } }).result
  const events: TailEvent[] = []
  try {
    const socket = new WebSocket(tail.url, "trace-v1")
    await new Promise<void>((resolve, reject) => {
      socket.addEventListener("open", () => resolve(), { once: true })
      socket.addEventListener("error", () => reject(new Error(`${which}: the tail did not open`)), { once: true })
    })
    socket.send(JSON.stringify({ filters: [], debug: false }))
    socket.addEventListener("message", (message) => {
      const text = typeof message.data === "string" ? message.data : new TextDecoder().decode(message.data as ArrayBuffer)
      try { events.push(JSON.parse(text) as TailEvent) } catch { /* not an event */ }
    })
    await probe()
    // Tail events arrive within seconds of the request; wait for ours, up to 30 s.
    for (let i = 0; i < 60 && !events.some((event) => event.event?.request?.url === probeUrl); i++) await Bun.sleep(500)
    socket.close()
  } finally {
    await fetch(`https://api.cloudflare.com/client/v4/accounts/${accountId}${base}/${tail.id}`, { method: "DELETE", headers: { authorization: `Bearer ${apiToken}` }, signal: AbortSignal.timeout(30_000) }).catch(() => undefined)
  }
  return events
}

if (process.argv.includes("--cutover")) {
  const deployments = await get(`/workers/scripts/${PROD_RECORDED_DEMO.worker}/deployments`)
  if (!deployments.ok) throw new Error(`${which}: Cloudflare answered ${deployments.status} for the deployments`)
  const version = activeVersion(((await deployments.json()) as { result: { deployments: LiveDeployment[] } }).result.deployments)
  for (const hostname of PROD_DOMAINS) {
    // The nonce is the query key: Tail redacts query values, never keys.
    const probeUrl = `https://${hostname}/api/agent?norn-parity-${crypto.randomUUID().replaceAll("-", "")}`
    let answer: string | undefined = `https://${hostname}/api/agent did not answer`
    const events = await tailDuring(probeUrl, async () => {
      const agent = await fetch(probeUrl, { method: "POST", headers: { "content-type": "application/json" }, body: "{}", redirect: "manual", signal: AbortSignal.timeout(30_000) }).catch(() => undefined)
      if (agent !== undefined) answer = agentAnswerIssue(hostname, agent.status, agent.headers.get("content-type"), await agent.text())
    })
    for (const issue of [answer, agentTailIssue(probeUrl, events, version)]) if (issue !== undefined) issues.push(issue)
  }
}
console.log(issues.length === 0 ? `${which}: ${PROD_RECORDED_DEMO.worker} matches the recorded-demo recipe${process.argv.includes("--cutover") ? ", and /api/agent answers on both domains from it at its active version (tail-verified)" : ""}` : `${which}: ${PROD_RECORDED_DEMO.worker} differs:\n  ${issues.join("\n  ")}`)
process.exit(issues.length === 0 ? 0 : 1)
