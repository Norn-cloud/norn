import { expect, test } from "bun:test"
import { deployTarget, leakedSecrets } from "../deploy-target.ts"

test("production keeps the live Worker and both demo domains", () => {
  expect(deployTarget({})).toEqual({ stage: "prod", worker: "norn-studio-recorded", domains: ["studio.norn.cloud", "studio-demo.norn.cloud"] })
})

test("any other stage gets its own Worker and only the domains it names", () => {
  expect(deployTarget({ ALCHEMY_STAGE: "preview", STUDIO_DOMAINS: "studio-preview.norn.cloud" }))
    .toEqual({ stage: "preview", worker: "norn-studio-recorded-preview", domains: ["studio-preview.norn.cloud"] })
})

test("a non-production stage never falls back to the production domains", () => {
  expect(() => deployTarget({ ALCHEMY_STAGE: "preview" })).toThrow("STUDIO_DOMAINS is required")
})

test("a malformed domain list or stage refuses before anything deploys", () => {
  expect(() => deployTarget({ STUDIO_DOMAINS: "studio.norn.cloud," })).toThrow("comma-separated hostnames")
  expect(() => deployTarget({ ALCHEMY_STAGE: "Pre view" })).toThrow("ALCHEMY_STAGE")
})

test("a DNS label longer than 63 characters is refused", () => {
  expect(() => deployTarget({ STUDIO_DOMAINS: `${"a".repeat(64)}.norn.cloud` })).toThrow("comma-separated hostnames")
  expect(deployTarget({ STUDIO_DOMAINS: `${"a".repeat(63)}.norn.cloud` }).domains).toEqual([`${"a".repeat(63)}.norn.cloud`])
  expect(() => deployTarget({ STUDIO_DOMAINS: "-studio.norn.cloud" })).toThrow("comma-separated hostnames")
})

test("a non-production stage cannot take the production hostnames", () => {
  expect(() => deployTarget({ ALCHEMY_STAGE: "preview", STUDIO_DOMAINS: "studio-preview.norn.cloud,studio.norn.cloud" }))
    .toThrow("cannot take the production domains studio.norn.cloud")
  expect(() => deployTarget({ ALCHEMY_STAGE: "preview", STUDIO_DOMAINS: "studio-demo.norn.cloud" })).toThrow("studio-demo.norn.cloud")
})

test("a state scan names the credential variables whose values it finds, and only those", () => {
  const env = { GH_TOKEN: "ghp_example_value_123", S3_SECRET_KEY: "s3-secret-value", PATH: "/usr/bin:/bin", SHORT_KEY: "abc" }
  const state = JSON.stringify({ props: { env: { GH_TOKEN: "ghp_example_value_123", PATH: "/usr/bin:/bin" } }, also: "ghp_example_value_123" })
  expect(leakedSecrets(state, env)).toEqual({ GH_TOKEN: 2 })
  expect(leakedSecrets(JSON.stringify({ props: { assets: "dist" } }), env)).toEqual({})
})

test("a state scan finds a credential written with JSON escapes", () => {
  const env = { API_SECRET: 'quote"and\\backslash\nnewline' }
  expect(leakedSecrets(JSON.stringify({ props: { env } }), env)).toEqual({ API_SECRET: 1 })
})
