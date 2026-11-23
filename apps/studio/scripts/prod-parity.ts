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
 * from another Worker or a stale version fails. The deployments are read again after both probes:
 * the same deployment, with one version at 100%, or it fails. Run it right after the custom domains
 * move. The tail is deleted afterwards.
 *
 *   bun scripts/prod-parity.ts --account norn [--cutover]  # CLOUDFLARE_WORKERS_API_TOKEN, CLOUDFLARE_ACCOUNT_ID
 *   bun scripts/prod-parity.ts --account legacy [--cutover] # LEGACY_CLOUDFLARE_CLEANUP_TOKEN, LEGACY_CLOUDFLARE_ACCOUNT_ID
 *
 * Both sets come from Infisical prod /norn/cloudflare. Prints the differences, never a value;
 * the tail is read in memory only (never printed: its events carry request headers);
 * exits 1 when there are any (or the Worker is missing).
 */
import { closeTail, liveSettings, openTail, workerDeployments, workerDomains, workerSettings, type CloudflareClient } from "../cloudflare-api.ts"
import { activeVersion, agentAnswerIssue, agentTailIssue, cloudflareCredentials, deploymentStableIssue, domainParity, LEGACY_CLOUDFLARE_VARIABLES, PROD_DOMAINS, prodParity, PROD_RECORDED_DEMO, type CloudflareCredentials, type LiveDeployment, type TailEvent } from "../deploy-target.ts"

/** The production Worker's recipe and custom-domain issues, or undefined when it is not deployed. */
export async function recipeIssues(which: string, client: CloudflareClient): Promise<string[] | undefined> {
  const settings = await workerSettings(client, PROD_RECORDED_DEMO.worker)
  if (!settings.ok && settings.status === 404) return undefined
  if (!settings.ok) throw new Error(`${which}: Cloudflare answered ${settings.status}`)
  const issues = prodParity(liveSettings(settings.value))
  const domains = await workerDomains(client)
  if (!domains.ok) throw new Error(`${which}: Cloudflare answered ${domains.status} for the custom domains`)
  issues.push(...domainParity(domains.value))
  return issues
}

/** The production Worker's deployments. */
export async function readDeployments(which: string, client: CloudflareClient): Promise<LiveDeployment[]> {
  const deployments = await workerDeployments(client, PROD_RECORDED_DEMO.worker)
  if (!deployments.ok) throw new Error(`${which}: Cloudflare answered ${deployments.status} for the deployments`)
  return deployments.value
}

/** Opens a tail on the production Worker, runs `probe`, and returns the tail's events (the tail is deleted). */
export async function tailDuring(which: string, client: CloudflareClient, probeUrl: string, probe: () => Promise<void>, socketFor: (url: string) => WebSocket = (url) => new WebSocket(url, "trace-v1")): Promise<TailEvent[]> {
  const created = await openTail(client, PROD_RECORDED_DEMO.worker)
  if (!created.ok) throw new Error(`${which}: Cloudflare answered ${created.status} opening a tail`)
  const tail = created.value
  const events: TailEvent[] = []
  try {
    const socket = socketFor(tail.url)
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
    await closeTail(client, PROD_RECORDED_DEMO.worker, tail.id).catch(() => undefined)
  }
  return events
}

async function main(argv: readonly string[]) {
  const which = argv[argv.indexOf("--account") + 1]
  if (which !== "norn" && which !== "legacy") throw new Error("--account norn or --account legacy is required")
  const legacySet = LEGACY_CLOUDFLARE_VARIABLES.filter((name) => process.env[name])
  if (legacySet.length > 0) throw new Error(`${legacySet.join(", ")} set: unset ${legacySet.length === 1 ? "it" : "them"}`)
  const credentials: CloudflareCredentials = which === "norn"
    ? cloudflareCredentials(process.env)
    : { accountId: process.env.LEGACY_CLOUDFLARE_ACCOUNT_ID ?? "", apiToken: process.env.LEGACY_CLOUDFLARE_CLEANUP_TOKEN ?? "" }
  if (!credentials.accountId || !credentials.apiToken) throw new Error("LEGACY_CLOUDFLARE_ACCOUNT_ID and LEGACY_CLOUDFLARE_CLEANUP_TOKEN are required (Infisical prod /norn/cloudflare)")
  const client: CloudflareClient = { credentials }

  const issues = await recipeIssues(which, client)
  if (issues === undefined) {
    console.log(`${which}: ${PROD_RECORDED_DEMO.worker} is not deployed`)
    process.exit(1)
  }
  const cutover = argv.includes("--cutover")
  if (cutover) {
    const before = await readDeployments(which, client)
    const version = activeVersion(before)
    for (const hostname of PROD_DOMAINS) {
      // The nonce is the query key: Tail redacts query values, never keys.
      const probeUrl = `https://${hostname}/api/agent?norn-parity-${crypto.randomUUID().replaceAll("-", "")}`
      let answer: string | undefined = `https://${hostname}/api/agent did not answer`
      const events = await tailDuring(which, client, probeUrl, async () => {
        const agent = await fetch(probeUrl, { method: "POST", headers: { "content-type": "application/json" }, body: "{}", redirect: "manual", signal: AbortSignal.timeout(30_000) }).catch(() => undefined)
        if (agent !== undefined) answer = agentAnswerIssue(hostname, agent.status, agent.headers.get("content-type"), await agent.text())
      })
      for (const issue of [answer, agentTailIssue(probeUrl, events, version)]) if (issue !== undefined) issues.push(issue)
    }
    const stable = deploymentStableIssue(before, await readDeployments(which, client))
    if (stable !== undefined) issues.push(stable)
  }
  console.log(issues.length === 0 ? `${which}: ${PROD_RECORDED_DEMO.worker} matches the recorded-demo recipe${cutover ? ", and /api/agent answers on both domains from it at its active version (tail-verified)" : ""}` : `${which}: ${PROD_RECORDED_DEMO.worker} differs:\n  ${issues.join("\n  ")}`)
  process.exit(issues.length === 0 ? 0 : 1)
}

if (import.meta.main) await main(process.argv.slice(2))
