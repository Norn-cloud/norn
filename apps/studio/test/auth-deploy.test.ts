import { expect, test } from "bun:test"
import { authDeploy, cellHost, deployTarget, requireWorkerScript } from "../deploy-target.ts"

const preview = deployTarget({ ALCHEMY_STAGE: "preview", STUDIO_DOMAINS: "studio-preview.norn.cloud" })
const secret = "s".repeat(32)

test("every stage needs a session secret and a state password", () => {
  expect(() => authDeploy({}, preview)).toThrow("BETTER_AUTH_SECRET")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: "short" }, preview)).toThrow("32 characters")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret }, preview)).toThrow("ALCHEMY_PASSWORD")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "  " }, preview)).toThrow("ALCHEMY_PASSWORD")
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: " p " }, preview)).toEqual({ secret, url: "https://studio-preview.norn.cloud" })
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p" }, preview)).toEqual({ secret, url: "https://studio-preview.norn.cloud" })
})

test("users sign in on the stage's first domain; a mail sender is optional", () => {
  const prod = deployTarget({ ALCHEMY_STAGE: "prod", NORN_DEPLOY_PROD: "1" })
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_EMAIL_FROM: "studio@norn.cloud" }, prod))
    .toEqual({ secret, url: "https://studio.norn.cloud", emailFrom: "studio@norn.cloud" })
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_EMAIL_FROM: "nope" }, prod)).toThrow("STUDIO_EMAIL_FROM")
})

test("a stage may name the organization members' sessions start in, and it must be an id", () => {
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_DEFAULT_ORG: "org_demo" }, preview))
    .toEqual({ secret, url: "https://studio-preview.norn.cloud", defaultOrg: "org_demo" })
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_DEFAULT_ORG: "org demo; drop" }, preview)).toThrow("STUDIO_DEFAULT_ORG")
})

test("only preview binds a cell Worker, by script name; prod has none until promotion", () => {
  expect(cellHost(preview)).toBe("norn-cell-preview")
  expect(cellHost(deployTarget({ ALCHEMY_STAGE: "prod", NORN_DEPLOY_PROD: "1" }))).toBeUndefined()
  expect(cellHost(deployTarget({ ALCHEMY_STAGE: "pr-12", STUDIO_DOMAINS: "studio-pr-12.norn.cloud" }))).toBeUndefined()
})

test("a missing cell Worker fails the deploy by name; an existing one passes", async () => {
  const env = { CLOUDFLARE_ACCOUNT_ID: "a", CLOUDFLARE_EMAIL: "e", CLOUDFLARE_API_KEY: "k" }
  let asked = ""
  const answer = (status: number) => async (url: RequestInfo | URL) => { asked = String(url); return new Response("{}", { status }) }
  await expect(requireWorkerScript("norn-cell-preview", env, answer(404) as typeof fetch)).rejects.toThrow("The cell Worker norn-cell-preview does not exist")
  expect(asked).toBe("https://api.cloudflare.com/client/v4/accounts/a/workers/scripts/norn-cell-preview/settings")
  await expect(requireWorkerScript("norn-cell-preview", env, answer(500) as typeof fetch)).rejects.toThrow("Cloudflare answered 500")
  await requireWorkerScript("norn-cell-preview", env, answer(200) as typeof fetch)
  await expect(requireWorkerScript("norn-cell-preview", {}, answer(200) as typeof fetch)).rejects.toThrow("CLOUDFLARE_ACCOUNT_ID")
  // No credentials is named as such, before any request goes out.
  asked = ""
  await expect(requireWorkerScript("norn-cell-preview", { CLOUDFLARE_ACCOUNT_ID: "a", CLOUDFLARE_EMAIL: "e" }, answer(200) as typeof fetch)).rejects.toThrow("CLOUDFLARE_API_TOKEN")
  expect(asked).toBe("")
  await requireWorkerScript("norn-cell-preview", { CLOUDFLARE_ACCOUNT_ID: "a", CLOUDFLARE_API_TOKEN: "t" }, answer(200) as typeof fetch)
})
