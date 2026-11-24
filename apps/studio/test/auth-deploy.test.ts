import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { activeVersion, agentAnswerIssue, agentTailIssue, authDeploy, deploymentStableIssue, cellHost, cellHostUrl, cloudflareCredentials, deployTarget, domainParity, NORN_CLOUDFLARE_ACCOUNT_ID, PROD_DOMAINS, prodParity, PROD_RECORDED_DEMO, requireSignInRecipe, requireWorkerScript, useCloudflareCredentials, wranglerEnv } from "../deploy-target.ts"

const preview = deployTarget({ ALCHEMY_STAGE: "preview", STUDIO_DOMAINS: "studio-preview.norn.cloud" })
const prod = deployTarget({ ALCHEMY_STAGE: "prod", NORN_DEPLOY_PROD: "1" })
const secret = "s".repeat(32)
const invited = { STUDIO_SIGNUP_EMAILS: "analyst@demo.norn.cloud, Viewer@demo.norn.cloud" }
const signUpEmails = ["analyst@demo.norn.cloud", "viewer@demo.norn.cloud"]
const norn = { CLOUDFLARE_ACCOUNT_ID: NORN_CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_WORKERS_API_TOKEN: "workers-token" }

test("every stage needs a session secret and a state password", () => {
  expect(() => authDeploy({}, preview)).toThrow("BETTER_AUTH_SECRET")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: "short" }, preview)).toThrow("32 characters")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ...invited }, preview)).toThrow("ALCHEMY_PASSWORD")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "  ", ...invited }, preview)).toThrow("ALCHEMY_PASSWORD")
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: " p ", ...invited }, preview)).toEqual({ secret, url: "https://studio-preview.norn.cloud", origins: ["https://studio-preview.norn.cloud"], signUpEmails })
})

test("sign-up is invite-only on every stage: the list is required, normalized and checked", () => {
  const base = { BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p" }
  expect(() => authDeploy(base, preview)).toThrow("STUDIO_SIGNUP_EMAILS is required")
  expect(() => authDeploy({ ...base, STUDIO_SIGNUP_EMAILS: " , " }, preview)).toThrow("NORN_STUDIO_PREVIEW_SIGNUP_EMAILS")
  expect(() => authDeploy({ ...base, STUDIO_SIGNUP_EMAILS: "not-an-address" }, preview)).toThrow("comma-separated email addresses")
  expect(authDeploy({ ...base, STUDIO_SIGNUP_EMAILS: "a@demo.norn.cloud,A@demo.norn.cloud" }, preview).signUpEmails).toEqual(["a@demo.norn.cloud"])
  const run = readFileSync(new URL("../alchemy.run.ts", import.meta.url), "utf8")
  expect(run).toContain('STUDIO_SIGNUP_EMAILS: auth.signUpEmails.join(",")')
})

test("users sign in on the stage's first domain; a mail sender is optional", () => {
  const pr = deployTarget({ ALCHEMY_STAGE: "pr-1", STUDIO_DOMAINS: "studio-pr-1.norn.cloud,studio-pr-1b.norn.cloud" })
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_EMAIL_FROM: "studio@norn.cloud", ...invited }, pr))
    .toEqual({ secret, url: "https://studio-pr-1.norn.cloud", origins: ["https://studio-pr-1.norn.cloud", "https://studio-pr-1b.norn.cloud"], emailFrom: "studio@norn.cloud", signUpEmails })
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_EMAIL_FROM: "nope", ...invited }, pr)).toThrow("STUDIO_EMAIL_FROM")
})

test("a stage may name the organization members' sessions start in, and it must be an id", () => {
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_DEFAULT_ORG: "org_demo", ...invited }, preview))
    .toEqual({ secret, url: "https://studio-preview.norn.cloud", origins: ["https://studio-preview.norn.cloud"], defaultOrg: "org_demo", signUpEmails })
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_DEFAULT_ORG: "org demo; drop", ...invited }, preview)).toThrow("STUDIO_DEFAULT_ORG")
})

test("production is the recorded-demo Studio: this entry point refuses it before anything runs", () => {
  expect(prod.recipe).toBe("recorded-demo")
  expect(preview.recipe).toBe("sign-in")
  expect(() => requireSignInRecipe(prod)).toThrow("AgentBudget")
  expect(() => requireSignInRecipe(prod)).toThrow("studio.norn.cloud and studio-demo.norn.cloud")
  requireSignInRecipe(preview)
  // The refusal comes first: before credentials, auth, or any API call.
  const run = readFileSync(new URL("../alchemy.run.ts", import.meta.url), "utf8")
  const order = ["requireSignInRecipe(target)", "cloudflareCredentials(process.env)", "authDeploy(process.env, target)", "requireWorkerScript(host", "await alchemy(", "await createCloudflareApi()"].map((marker) => run.indexOf(marker))
  expect(order.every((at) => at >= 0)).toBe(true)
  expect([...order].sort((a, b) => a - b)).toEqual(order)
})

test("production parity: the live recorded-demo metadata matches; the sign-in Studio does not", () => {
  // As read from the live norn-studio-recorded on 2026-11-22 (old account, read-only token).
  const live = {
    compatibility_date: "2026-11-01",
    compatibility_flags: ["nodejs_compat"],
    bindings: [
      { name: "AGENT_BUDGET", type: "durable_object_namespace", class_name: "AgentBudget" },
      { name: "AI_GATEWAY_TOKEN", type: "secret_text" },
      { name: "AI_GATEWAY_URL", type: "secret_text" },
      { name: "ASSETS", type: "assets" },
    ],
  }
  expect(prodParity(live)).toEqual([])
  const signIn = { compatibility_date: "2026-10-20", compatibility_flags: ["nodejs_compat"], bindings: [{ name: "ASSETS", type: "assets" }, { name: "AUTH_DB", type: "d1" }, { name: "BETTER_AUTH_SECRET", type: "secret_text" }] }
  expect(prodParity(signIn)).toEqual([
    "missing binding AGENT_BUDGET (durable_object_namespace)",
    "missing binding AI_GATEWAY_URL (secret_text)",
    "missing binding AI_GATEWAY_TOKEN (secret_text)",
    "sign-in binding AUTH_DB on production",
    "sign-in binding BETTER_AUTH_SECRET on production",
    "compatibility date 2026-10-20, expected 2026-11-01",
  ])
  expect(prodParity({ ...live, bindings: live.bindings.map((b) => b.name === "AGENT_BUDGET" ? { ...b, class_name: "Other" } : b) })).toEqual(["AGENT_BUDGET binds class Other, expected AgentBudget"])
  // The class must belong to the production Worker itself, not to another script.
  expect(prodParity({ ...live, bindings: live.bindings.map((b) => b.name === "AGENT_BUDGET" ? { ...b, script_name: "norn-studio-recorded-preview" } : b) })).toEqual(["AGENT_BUDGET binds AgentBudget owned by norn-studio-recorded-preview, expected norn-studio-recorded itself"])
  expect(prodParity({ ...live, bindings: live.bindings.map((b) => b.name === "AGENT_BUDGET" ? { ...b, script_name: "norn-studio-recorded" } : b) })).toEqual([])
  expect(prod.worker).toBe(PROD_RECORDED_DEMO.worker)
})

test("production parity: both prod domains are attached, enabled, in production, to the production Worker", () => {
  const attached = PROD_DOMAINS.map((hostname) => ({ hostname, service: "norn-studio-recorded", environment: "production", enabled: true }))
  const demo = attached[1]!
  expect(PROD_DOMAINS).toEqual(["studio.norn.cloud", "studio-demo.norn.cloud"])
  expect(domainParity(attached)).toEqual([])
  expect(domainParity(attached.slice(0, 1))).toEqual(["custom domain studio-demo.norn.cloud is not attached"])
  expect(domainParity([attached[0]!, { ...demo, service: "norn-studio-recorded-preview" }])).toEqual(["custom domain studio-demo.norn.cloud is attached to norn-studio-recorded-preview, expected norn-studio-recorded"])
  expect(domainParity([attached[0]!, { ...demo, enabled: false }])).toEqual(["custom domain studio-demo.norn.cloud is not enabled"])
  // Missing fields are differences, not passes.
  expect(domainParity([attached[0]!, { hostname: demo.hostname, service: demo.service }])).toEqual(["custom domain studio-demo.norn.cloud serves environment none, expected production", "custom domain studio-demo.norn.cloud is not enabled"])
  expect(domainParity([attached[0]!, { ...demo, environment: "staging" }])).toEqual(["custom domain studio-demo.norn.cloud serves environment staging, expected production"])
  expect(domainParity([...attached, demo])).toEqual(["custom domain studio-demo.norn.cloud has 2 attachments, expected one"])
  expect(domainParity([...attached, { hostname: "preview.norn.cloud", service: "x" }])).toEqual([])
})

test("cutover: /api/agent must give the assistant's own answer, nothing else", () => {
  expect(agentAnswerIssue("studio.norn.cloud", 400, "application/json", '{"error":"Missing session."}')).toBeUndefined()
  expect(agentAnswerIssue("studio.norn.cloud", 404, "text/html", "<html>")).toContain("not the assistant's 400")
  expect(agentAnswerIssue("studio.norn.cloud", 502, "text/html", "bad gateway")).toContain("502")
  expect(agentAnswerIssue("studio.norn.cloud", 400, "text/html", '{"error":"Missing session."}')).toBeDefined()
  expect(agentAnswerIssue("studio.norn.cloud", 400, "application/json", '{"error":"other"}')).toBeDefined()
  const script = readFileSync(new URL("../scripts/prod-parity.ts", import.meta.url), "utf8")
  expect(script).toContain('argv.includes("--cutover")')
  expect(script).toContain("domainParity(")
  expect(script).toContain("agentTailIssue(probeUrl, events, version)")
  expect(script.indexOf("deploymentStableIssue(before, await readDeployments(which, client))")).toBeGreaterThan(script.lastIndexOf("agentTailIssue(probeUrl"))
})

test("cutover: the answer is tied to the production Worker at its active version", () => {
  const url = "https://studio.norn.cloud/api/agent?norn-parity-abc"
  const ran = (scriptName: string, id: string, status = 400, at = url) => ({ scriptName, scriptVersion: { id }, event: { request: { url: at, method: "POST" }, response: { status } } })
  expect(agentTailIssue(url, [ran("norn-studio-recorded", "v2")], "v2")).toBeUndefined()
  expect(agentTailIssue(url, [], "v2")).toContain("never reached norn-studio-recorded")
  expect(agentTailIssue(url, [ran("norn-studio-recorded", "v2", 400, "https://studio.norn.cloud/api/agent?other")], "v2")).toContain("never reached")
  expect(agentTailIssue(url, [ran("norn-studio-recorded", "v1")], "v2")).toContain("version v1, expected norn-studio-recorded version v2")
  expect(agentTailIssue(url, [ran("norn-studio-recorded-preview", "v2")], "v2")).toContain("ran on norn-studio-recorded-preview")
  expect(agentTailIssue(url, [ran("norn-studio-recorded", "v2", 404)], "v2")).toContain("answered 404")
  expect(agentTailIssue(url, [ran("norn-studio-recorded", "v2")], undefined)).toContain("no single active version")
  const d = (id: string, created_on: string, versions: { version_id: string; percentage: number }[]) => ({ id, created_on, versions })
  expect(activeVersion([d("a", "2026-11-01T00:00:00Z", [{ version_id: "v1", percentage: 100 }]), d("b", "2026-11-02T00:00:00Z", [{ version_id: "v2", percentage: 100 }])])).toBe("v2")
  expect(activeVersion([d("b", "2026-11-02T00:00:00Z", [{ version_id: "v2", percentage: 50 }, { version_id: "v1", percentage: 50 }])])).toBeUndefined()
  expect(activeVersion([])).toBeUndefined()
  // Re-read after both probes: the same deployment, one version at 100%, or the cutover fails.
  const one = [d("a", "2026-11-01T00:00:00Z", [{ version_id: "v1", percentage: 100 }])]
  expect(deploymentStableIssue(one, one)).toBeUndefined()
  expect(deploymentStableIssue(one, [...one, d("b", "2026-11-02T00:00:00Z", [{ version_id: "v2", percentage: 100 }])])).toMatch(/changed during the probes \(a → b\)/)
  expect(deploymentStableIssue(one, [d("a", "2026-11-01T00:00:00Z", [{ version_id: "v1", percentage: 90 }, { version_id: "v2", percentage: 10 }])])).toMatch(/one version at 100%/)
  expect(deploymentStableIssue(one, [])).toMatch(/no deployment/)
  expect(deploymentStableIssue([d("a", "2026-11-01T00:00:00Z", [{ version_id: "v1", percentage: 50 }, { version_id: "v2", percentage: 50 }])], one)).toMatch(/one version at 100%/)
})

test("Cloudflare credentials: the Norn workers token and account only, and never the Global key", () => {
  expect(cloudflareCredentials(norn)).toEqual({ accountId: NORN_CLOUDFLARE_ACCOUNT_ID, apiToken: "workers-token" })
  expect(() => cloudflareCredentials({ ...norn, CLOUDFLARE_API_KEY: "k" })).toThrow("CLOUDFLARE_API_KEY set")
  expect(() => cloudflareCredentials({ ...norn, CLOUDFLARE_EMAIL: "e", CLOUDFLARE_ACCOUNT_EMAIL: "e" })).toThrow("CLOUDFLARE_EMAIL, CLOUDFLARE_ACCOUNT_EMAIL set")
  // A combined or shell CLOUDFLARE_API_TOKEN is not a way in.
  expect(() => cloudflareCredentials({ CLOUDFLARE_ACCOUNT_ID: NORN_CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_API_TOKEN: "combined" })).toThrow("CLOUDFLARE_WORKERS_API_TOKEN is required")
  expect(() => cloudflareCredentials({ ...norn, CLOUDFLARE_WORKERS_API_TOKEN: "  " })).toThrow("CLOUDFLARE_WORKERS_API_TOKEN is required")
  expect(() => cloudflareCredentials({ ...norn, CLOUDFLARE_ACCOUNT_ID: "0".repeat(32) })).toThrow("must be the Norn account")
  expect(() => cloudflareCredentials({ CLOUDFLARE_WORKERS_API_TOKEN: "t" })).toThrow("must be the Norn account")
})

test("the process environment ends up with the checked token as Alchemy's only Cloudflare credential", () => {
  const env: Record<string, string | undefined> = { ...norn, CLOUDFLARE_API_TOKEN: "combined", CLOUDFLARE_INFRA_API_TOKEN: "infra", PATH: "/bin" }
  useCloudflareCredentials(env, cloudflareCredentials(env))
  expect(env).toEqual({ PATH: "/bin", CLOUDFLARE_API_TOKEN: "workers-token", CLOUDFLARE_ACCOUNT_ID: NORN_CLOUDFLARE_ACCOUNT_ID })
})

test("Wrangler runs with a sanitized environment: toolchain, the checked token, a temporary HOME and XDG dirs", () => {
  const parent = { PATH: "/bin", HOME: "/home/x", XDG_CONFIG_HOME: "/home/x/.config", GH_TOKEN: "gh", CLOUDFLARE_API_KEY: "k", CLOUDFLARE_API_TOKEN: "combined", INFISICAL_TOKEN: "i", BETTER_AUTH_SECRET: secret }
  expect(wranglerEnv(parent, { accountId: NORN_CLOUDFLARE_ACCOUNT_ID, apiToken: "workers-token" }, "/tmp/seed")).toEqual({
    PATH: "/bin",
    HOME: "/tmp/seed/home",
    XDG_CONFIG_HOME: "/tmp/seed/config",
    XDG_CACHE_HOME: "/tmp/seed/cache",
    XDG_DATA_HOME: "/tmp/seed/data",
    XDG_STATE_HOME: "/tmp/seed/state",
    WRANGLER_SEND_METRICS: "false",
    CLOUDFLARE_API_TOKEN: "workers-token",
    CLOUDFLARE_ACCOUNT_ID: NORN_CLOUDFLARE_ACCOUNT_ID,
  })
  const seed = readFileSync(new URL("../scripts/seed-demo-org.ts", import.meta.url), "utf8")
  expect(seed).toContain("env: wranglerEnv(process.env, credentials, dir)")
  expect(seed).not.toContain("env: process.env")
  expect(seed.indexOf("cloudflareCredentials(process.env)")).toBeLessThan(seed.indexOf("discoverD1({ credentials }"))
})

test("only preview binds a cell Worker, by script name; prod has none until promotion", () => {
  expect(cellHost(preview)).toBe("norn-cell-preview")
  expect(cellHost(prod)).toBeUndefined()
  expect(cellHost(deployTarget({ ALCHEMY_STAGE: "pr-12", STUDIO_DOMAINS: "studio-pr-12.norn.cloud" }))).toBeUndefined()
  // The alpha reaches its self-hosted Rivet runners over HTTPS, and binds no cell Worker.
  const alpha = deployTarget({ ALCHEMY_STAGE: "alpha", STUDIO_DOMAINS: "alpha.norn.cloud" })
  expect(cellHostUrl(alpha)).toBe("https://alpha-cells.norn.cloud")
  expect(cellHost(alpha)).toBeUndefined()
  expect(cellHostUrl(preview)).toBeUndefined()
})

test("a missing cell Worker fails the deploy by name; an existing one passes", async () => {
  const credentials = cloudflareCredentials(norn)
  let asked = ""
  const answer = (status: number) => async (url: RequestInfo | URL, init?: RequestInit) => { asked = `${String(url)} ${new Headers(init?.headers).get("authorization")}`; return new Response("{}", { status }) }
  await expect(requireWorkerScript("norn-cell-preview", credentials, answer(404) as typeof fetch)).rejects.toThrow("The cell Worker norn-cell-preview does not exist")
  expect(asked).toBe(`https://api.cloudflare.com/client/v4/accounts/${NORN_CLOUDFLARE_ACCOUNT_ID}/workers/scripts/norn-cell-preview/settings Bearer workers-token`)
  await expect(requireWorkerScript("norn-cell-preview", credentials, answer(500) as typeof fetch)).rejects.toThrow("Cloudflare answered 500")
  await requireWorkerScript("norn-cell-preview", credentials, answer(200) as typeof fetch)
})
