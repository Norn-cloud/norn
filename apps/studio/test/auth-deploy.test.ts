import { expect, test } from "bun:test"
import { authDeploy, deployTarget } from "../deploy-target.ts"

const preview = deployTarget({ ALCHEMY_STAGE: "preview", STUDIO_DOMAINS: "studio-preview.norn.cloud" })
const secret = "s".repeat(32)

test("every stage needs a session secret and a state password", () => {
  expect(() => authDeploy({}, preview)).toThrow("BETTER_AUTH_SECRET")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: "short" }, preview)).toThrow("32 characters")
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret }, preview)).toThrow("ALCHEMY_PASSWORD")
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p" }, preview)).toEqual({ secret, url: "https://studio-preview.norn.cloud" })
})

test("users sign in on the stage's first domain; a mail sender is optional", () => {
  const prod = deployTarget({})
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_EMAIL_FROM: "studio@norn.cloud" }, prod))
    .toEqual({ secret, url: "https://studio.norn.cloud", emailFrom: "studio@norn.cloud" })
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_EMAIL_FROM: "nope" }, prod)).toThrow("STUDIO_EMAIL_FROM")
})

test("a stage may name the organization members' sessions start in, and it must be an id", () => {
  expect(authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_DEFAULT_ORG: "org_demo" }, preview))
    .toEqual({ secret, url: "https://studio-preview.norn.cloud", defaultOrg: "org_demo" })
  expect(() => authDeploy({ BETTER_AUTH_SECRET: secret, ALCHEMY_PASSWORD: "p", STUDIO_DEFAULT_ORG: "org demo; drop" }, preview)).toThrow("STUDIO_DEFAULT_ORG")
})
