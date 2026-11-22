import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { authDeploy, cellHost, cloudflareCredentials, deployTarget, NORN_CLOUDFLARE_ACCOUNT_ID, prodParity, PROD_RECORDED_DEMO, requireSignInRecipe, requireWorkerScript, useCloudflareCredentials, wranglerEnv } from "../deploy-target.ts"

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
  const order = ["requireSignInRecipe(target)", "cloudflareCredentials(process.env)", "authDeploy(process.env, target)", "requireWorkerScript(", "createCloudflareApi()", "await alchemy("].map((marker) => run.indexOf(marker))
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
  expect(PROD_RECORDED_DEMO.worker).toBe(prod.worker)
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

test("Wrangler runs with a sanitized environment: toolchain, the checked token, its own config dir", () => {
  const parent = { PATH: "/bin", HOME: "/home/x", GH_TOKEN: "gh", CLOUDFLARE_API_KEY: "k", CLOUDFLARE_API_TOKEN: "combined", INFISICAL_TOKEN: "i", BETTER_AUTH_SECRET: secret }
  expect(wranglerEnv(parent, { accountId: NORN_CLOUDFLARE_ACCOUNT_ID, apiToken: "workers-token" }, "/tmp/seed")).toEqual({
    PATH: "/bin", HOME: "/home/x", XDG_CONFIG_HOME: "/tmp/seed", WRANGLER_SEND_METRICS: "false", CLOUDFLARE_API_TOKEN: "workers-token", CLOUDFLARE_ACCOUNT_ID: NORN_CLOUDFLARE_ACCOUNT_ID,
  })
  const seed = readFileSync(new URL("../scripts/seed-demo-org.ts", import.meta.url), "utf8")
  expect(seed).toContain("env: wranglerEnv(process.env, credentials, dir)")
  expect(seed).not.toContain("env: process.env")
  expect(seed.indexOf("cloudflareCredentials(process.env)")).toBeLessThan(seed.indexOf("discoverD1(`"))
})

test("only preview binds a cell Worker, by script name; prod has none until promotion", () => {
  expect(cellHost(preview)).toBe("norn-cell-preview")
  expect(cellHost(prod)).toBeUndefined()
  expect(cellHost(deployTarget({ ALCHEMY_STAGE: "pr-12", STUDIO_DOMAINS: "studio-pr-12.norn.cloud" }))).toBeUndefined()
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
