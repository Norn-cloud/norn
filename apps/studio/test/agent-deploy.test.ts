import { expect, test } from "bun:test"
import { agentDeploy } from "../deploy-target.ts"

const URL = `https://gateway.ai.cloudflare.com/v1/${"0".repeat(32)}/default/compat`

test("a deploy without gateway settings runs without the assistant", () => {
  expect(agentDeploy({})).toBeUndefined()
})

test("the assistant needs its gateway URL, token, checker package and state password together", () => {
  expect(() => agentDeploy({ AI_GATEWAY_URL: URL })).toThrow("together")
  expect(() => agentDeploy({ AI_GATEWAY_URL: "https://example.com/v1", AI_GATEWAY_TOKEN: "t" })).toThrow("Gateway URL")
  expect(() => agentDeploy({ AI_GATEWAY_URL: URL, AI_GATEWAY_TOKEN: "t" })).toThrow("NORN_CHECKER_WASM_PKG")
  expect(() => agentDeploy({ AI_GATEWAY_URL: URL, AI_GATEWAY_TOKEN: "t", NORN_CHECKER_WASM_PKG: "/pkg" })).toThrow("ALCHEMY_PASSWORD")
  expect(agentDeploy({ AI_GATEWAY_URL: URL, AI_GATEWAY_TOKEN: "t", NORN_CHECKER_WASM_PKG: "/pkg", ALCHEMY_PASSWORD: "p" })).toEqual({ gatewayUrl: URL, gatewayToken: "t", checkerPkg: "/pkg" })
})
