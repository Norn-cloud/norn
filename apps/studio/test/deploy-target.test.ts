import { expect, test } from "bun:test"
import { deployTarget } from "../deploy-target.ts"

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
